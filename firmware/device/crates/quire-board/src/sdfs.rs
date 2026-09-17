//! The SD card as a `quire_fs::Fs`: FAT32 through embedded-sdmmc (vendored with long
//! file name creation), one open handle per Quire file, paths resolved component by
//! component so nothing about the card is held in RAM.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;
use core::sync::atomic::{AtomicU32, Ordering};

use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{LfnBuffer, Mode, RawDirectory, RawFile, RawVolume, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use quire_fs::{DirEntry, Fs, FsError, FsResult, ReadAt, WriteFile};

use crate::bus::{BusHandle, Role, SharedBus};
use crate::path;

/// Local time in seconds since 1970, kept current by the main loop; the card's
/// timestamps come from it.
pub static LOCAL_NOW: AtomicU32 = AtomicU32::new(0);

/// Time source for file timestamps.
pub struct Clock;

impl TimeSource for Clock {
    fn get_timestamp(&self) -> Timestamp {
        let t = LOCAL_NOW.load(Ordering::Relaxed);
        let days = (t / 86_400) as i64;
        let (y, m, d) = crate::util::civil_from_days(days);
        let secs = t % 86_400;
        Timestamp {
            year_since_1970: (y - 1970).clamp(0, 255) as u8,
            zero_indexed_month: (m - 1) as u8,
            zero_indexed_day: (d - 1) as u8,
            hours: (secs / 3600) as u8,
            minutes: ((secs / 60) % 60) as u8,
            seconds: (secs % 60) as u8,
        }
    }
}

/// The SPI device the card sits behind.
pub type CardSpi = ExclusiveDevice<BusHandle<'static>, Output<'static>, Delay>;
/// The block device.
pub type Card = SdCard<CardSpi, Delay>;
/// The volume manager: up to 8 open directories and 8 open files.
pub type Vm = VolumeManager<Card, Clock, 8, 8, 1>;

fn map<E: core::error::Error>(e: embedded_sdmmc::Error<E>) -> FsError {
    use embedded_sdmmc::Error as E2;
    match e {
        E2::NotFound => FsError::NotFound,
        E2::DiskFull | E2::NotEnoughSpace => FsError::Full,
        E2::FilenameError(_) | E2::BadCluster | E2::InvalidOffset => FsError::InvalidPath,
        E2::EndOfFile => FsError::Eof,
        other => FsError::Io(alloc::format!("{other:?}")),
    }
}

/// The mounted card. Clones share the volume manager, so the network task can hold its
/// own handle; the manager is only ever used from the one cooperative executor.
#[derive(Clone)]
pub struct SdFs {
    vm: &'static Vm,
    /// The shared SPI bus, kept so the card can be taken back down to its initialisation
    /// rate and up again after its power rail has been cut for a sleep.
    bus: &'static SharedBus,
    volume: RawVolume,
    root: RawDirectory,
    /// Bytes on the card, from the CSD.
    pub card_bytes: u64,
}

impl SdFs {
    /// Bring the card up on the shared bus: initialise at 400 kHz, then run at 20 MHz.
    pub fn mount(bus: &'static SharedBus, cs: Output<'static>, vm_cell: &'static static_cell::StaticCell<Vm>) -> Result<SdFs, String> {
        bus.set_rate(Role::Card, 400_000);
        // The card needs at least 74 clocks at 400 kHz with no chip select asserted
        // before it will answer CMD0 — it uses them to bring its own logic up. The
        // driver deliberately leaves this to the caller (it cannot deassert a chip
        // select it does not own), and warns that some cards tolerate its absence and
        // some do not. A card that does not answers CardNotFound, which reads on the
        // screen as no card at all. Ten bytes with `cs` still high is eighty clocks.
        {
            use embedded_hal::spi::SpiBus;
            let mut warmup: BusHandle<'_> = bus.handle(Role::Card);
            warmup.write(&[0xFF; 10]).map_err(|e| alloc::format!("warmup: {e:?}"))?;
        }
        let dev = ExclusiveDevice::new(bus.handle(Role::Card), cs, Delay::new()).map_err(|_| String::from("cs"))?;
        let card = SdCard::new(dev, Delay::new());
        let card_bytes = card.num_bytes().map_err(|e| alloc::format!("card: {e:?}"))?;
        bus.set_rate(Role::Card, 20_000_000);
        let vm = vm_cell.init(VolumeManager::new_with_limits(card, Clock, 0x1000));
        let volume = vm.open_raw_volume(VolumeIdx(0)).map_err(|e| alloc::format!("volume: {e:?}"))?;
        let root = vm.open_root_dir(volume).map_err(|e| alloc::format!("root: {e:?}"))?;
        Ok(SdFs { vm, bus, volume, root, card_bytes })
    }

    /// Bring the card back after its power rail was cut.
    ///
    /// Only the SPI-level initialisation is lost with the power: it is the same card with
    /// the same filesystem, so the volume and directory handles stay valid and the block
    /// cache still holds blocks that nothing has written. What has to be redone is the
    /// initialisation handshake, and that only answers at 400 kHz, so the bus drops to the
    /// rate `mount` uses and goes back to 20 MHz afterwards.
    pub fn reacquire(&self) -> Result<(), String> {
        self.vm.device(|card| card.mark_card_uninit());
        self.bus.set_rate(Role::Card, 400_000);
        // `num_bytes` runs the handshake; the size is already known from the mount.
        let r = self.vm.device(|card| card.num_bytes()).map_err(|e| alloc::format!("card: {e:?}"));
        self.bus.set_rate(Role::Card, 20_000_000);
        r.map(|_| ())
    }

    /// The volume manager (for the developer screen).
    pub fn vm(&self) -> &'static Vm {
        self.vm
    }

    /// Open the directory at `parent` (a path with the final component removed), walking
    /// from the root. The returned handle must be closed with [`SdFs::close_dir`] unless
    /// it is the root.
    fn open_path_dir(&self, parent: &str) -> FsResult<RawDirectory> {
        let mut dir = self.root;
        for comp in path::components(parent) {
            let next = self.vm.open_long_name_dir_in_dir(dir, comp).map_err(map);
            self.close_dir(dir);
            dir = next?;
        }
        Ok(dir)
    }

    fn close_dir(&self, dir: RawDirectory) {
        if dir != self.root {
            let _ = self.vm.close_dir(dir);
        }
    }

    fn with_parent<R>(&self, p: &str, f: impl FnOnce(RawDirectory, &str) -> FsResult<R>) -> FsResult<R> {
        let (parent, name) = path::split_parent(p).ok_or(FsError::InvalidPath)?;
        if !path::valid_name(name) {
            return Err(FsError::InvalidPath);
        }
        let dir = self.open_path_dir(parent)?;
        let r = f(dir, name);
        self.close_dir(dir);
        r
    }

    fn open_mode(&self, p: &str, mode: Mode) -> FsResult<RawFile> {
        self.with_parent(p, |dir, name| self.vm.open_long_name_file_in_dir(dir, name, mode).map_err(map))
    }
}

/// An open file for reading.
pub struct SdFile {
    vm: &'static Vm,
    file: RawFile,
    len: u32,
    pos: Cell<u32>,
}

impl ReadAt for SdFile {
    fn len(&self) -> u64 {
        self.len as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        if offset >= self.len as u64 || buf.is_empty() {
            return Ok(0);
        }
        let offset = offset as u32;
        if self.pos.get() != offset {
            self.vm.file_seek_from_start(self.file, offset).map_err(map)?;
            self.pos.set(offset);
        }
        let want = buf.len().min((self.len - offset) as usize);
        let n = self.vm.read(self.file, &mut buf[..want]).map_err(map)?;
        self.pos.set(offset + n as u32);
        Ok(n)
    }
}

impl Drop for SdFile {
    fn drop(&mut self) {
        let _ = self.vm.close_file(self.file);
    }
}

/// An open file for writing.
pub struct SdWriter {
    vm: &'static Vm,
    file: RawFile,
}

impl WriteFile for SdWriter {
    fn write_all(&mut self, data: &[u8]) -> FsResult<()> {
        self.vm.write(self.file, data).map_err(map)
    }
    fn flush(&mut self) -> FsResult<()> {
        self.vm.flush_file(self.file).map_err(map)
    }
}

impl Drop for SdWriter {
    fn drop(&mut self) {
        let _ = self.vm.close_file(self.file);
    }
}

impl Fs for SdFs {
    type File = SdFile;
    type Writer = SdWriter;

    fn open(&self, p: &str) -> FsResult<SdFile> {
        let file = self.open_mode(p, Mode::ReadOnly)?;
        let len = self.vm.file_length(file).map_err(map)?;
        Ok(SdFile { vm: self.vm, file, len, pos: Cell::new(0) })
    }

    fn create(&self, p: &str) -> FsResult<SdWriter> {
        let file = self.open_mode(p, Mode::ReadWriteCreateOrTruncate)?;
        Ok(SdWriter { vm: self.vm, file })
    }

    fn append(&self, p: &str) -> FsResult<SdWriter> {
        let file = self.open_mode(p, Mode::ReadWriteCreateOrAppend)?;
        Ok(SdWriter { vm: self.vm, file })
    }

    fn exists(&self, p: &str) -> bool {
        if p == "/" || p.is_empty() {
            return true;
        }
        self.with_parent(p, |dir, name| self.vm.find_long_name_entry_in_dir(dir, name).map(|_| ()).map_err(map)).is_ok()
    }

    fn read_dir(&self, p: &str) -> FsResult<Vec<DirEntry>> {
        let dir = self.open_path_dir(p)?;
        let mut out = Vec::new();
        let mut storage = [0u8; 512];
        let mut lfn = LfnBuffer::new(&mut storage);
        let r = self.vm.iterate_dir_lfn(dir, &mut lfn, |e, long| {
            if e.attributes.is_volume() || e.attributes.is_lfn() {
                return core::ops::ControlFlow::Continue(());
            }
            let short = e.name.to_string();
            if short == "." || short == ".." {
                return core::ops::ControlFlow::Continue(());
            }
            let name = long.map(String::from).unwrap_or(short);
            out.push(DirEntry { name, is_dir: e.attributes.is_directory(), size: e.size as u64 });
            core::ops::ControlFlow::Continue(())
        });
        self.close_dir(dir);
        r.map_err(map)?;
        Ok(out)
    }

    fn mkdir_all(&self, p: &str) -> FsResult<()> {
        let mut dir = self.root;
        for comp in path::components(p) {
            let next = match self.vm.open_long_name_dir_in_dir(dir, comp) {
                Ok(d) => d,
                Err(embedded_sdmmc::Error::NotFound) => {
                    self.vm.make_long_name_dir_in_dir(dir, comp).map_err(map)?;
                    self.vm.open_long_name_dir_in_dir(dir, comp).map_err(map)?
                }
                Err(e) => {
                    self.close_dir(dir);
                    return Err(map(e));
                }
            };
            self.close_dir(dir);
            dir = next;
        }
        self.close_dir(dir);
        Ok(())
    }

    fn remove(&self, p: &str) -> FsResult<()> {
        self.with_parent(p, |dir, name| self.vm.delete_long_name_entry_in_dir(dir, name).map_err(map))
    }

    fn rename(&self, from: &str, to: &str) -> FsResult<()> {
        let (pf, nf) = path::split_parent(from).ok_or(FsError::InvalidPath)?;
        let (pt, nt) = path::split_parent(to).ok_or(FsError::InvalidPath)?;
        if !path::valid_name(nf) || !path::valid_name(nt) {
            return Err(FsError::InvalidPath);
        }
        if pf.eq_ignore_ascii_case(pt) {
            return self.with_parent(from, |dir, name| self.vm.rename_long_name_in_dir(dir, name, nt).map_err(map));
        }
        let df = self.open_path_dir(pf)?;
        let dt = match self.open_path_dir(pt) {
            Ok(d) => d,
            Err(e) => {
                self.close_dir(df);
                return Err(e);
            }
        };
        let r = self.vm.move_long_name(df, nf, dt, nt).map_err(map);
        self.close_dir(dt);
        self.close_dir(df);
        r
    }

    fn free_bytes(&self) -> Option<u64> {
        None
    }
}

impl SdFs {
    /// Card size in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.card_bytes
    }
    /// The raw volume (developer screen).
    pub fn volume(&self) -> RawVolume {
        self.volume
    }
}
