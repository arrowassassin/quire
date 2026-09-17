//! Quire on the Xteink X3: brings the board up, mounts the card, probes the panel, and
//! runs the UI loop — keys sampled at 100 Hz, one-second ticks, ingest and page-index
//! work in idle time, light sleep between events and deep sleep after the timeout.
#![no_std]
#![no_main]

extern crate alloc;

mod display;
mod env;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcCalLine, AdcConfig, AdcPin, Attenuation};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPinWithResistors};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::peripherals::ADC1;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeupLevel};
use esp_hal::rtc_cntl::Rtc;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{ram, Blocking};
use esp_println::println;
use quire_board::bus::SharedBus;
use quire_board::i2c;
use quire_board::keys::KeyMachine;
use quire_board::power::{self, UPTIME_MS};
use quire_board::sdfs::{SdFs, Vm};
use quire_gfx::{draw_text, Frame, Ink, TextStyle};
use quire_library::{ingest_book, scan};
use quire_net::{CardFs, NetCommand, NetTaskArgs, NetToMain};
use quire_ui::{Env, Event, Refresh, SysRequest, Ui};
use static_cell::StaticCell;

use crate::display::Display;
use crate::env::DeviceEnv;

esp_bootloader_esp_idf::esp_app_desc!();

/// Build stamp shown on About.
const BUILD: &str = env!("CARGO_PKG_VERSION");

static BUS: StaticCell<SharedBus> = StaticCell::new();
static VM: StaticCell<Vm> = StaticCell::new();
/// The card handle the network task keeps for its lifetime.
static NET_FS: StaticCell<SdFs> = StaticCell::new();

type KeyPin1 = AdcPin<esp_hal::peripherals::GPIO1<'static>, ADC1<'static>, AdcCalLine<ADC1<'static>>>;
type KeyPin2 = AdcPin<esp_hal::peripherals::GPIO2<'static>, ADC1<'static>, AdcCalLine<ADC1<'static>>>;

/// The key inputs.
struct Keys {
    adc: Adc<'static, ADC1<'static>, Blocking>,
    g1: KeyPin1,
    g2: KeyPin2,
    power: Input<'static>,
    machine: KeyMachine,
}

impl Keys {
    fn read_mv(&mut self, which: u8) -> u16 {
        // The calibrated read returns millivolts; a stuck conversion reads as idle.
        let r = if which == 1 { nb::block!(self.adc.read_oneshot(&mut self.g1)) } else { nb::block!(self.adc.read_oneshot(&mut self.g2)) };
        r.unwrap_or(4095)
    }
    /// Sample all three inputs; returns the key events that fired.
    fn sample(&mut self, now_ms: u32) -> heapless::Vec<quire_ui::KeyEvent, 6> {
        let g1 = self.read_mv(1);
        let g2 = self.read_mv(2);
        let power = self.power.is_low();
        self.machine.sample(g1, g2, power, now_ms)
    }
    fn raw(&mut self) -> (u16, u16, bool) {
        (self.read_mv(1), self.read_mv(2), self.power.is_low())
    }
}

/// The pins the boot-time controller probe bit-bangs.
struct ProbePins<'a> {
    sclk: Output<'a>,
    mosi: esp_hal::gpio::Flex<'a>,
    rst: Output<'static>,
    cs: Output<'static>,
    dc: Output<'static>,
    delay: esp_hal::delay::Delay,
}

impl quire_epd::ProbeBus for ProbePins<'_> {
    fn rst(&mut self, high: bool) {
        self.rst.set_level(if high { Level::High } else { Level::Low });
    }
    fn cs(&mut self, high: bool) {
        self.cs.set_level(if high { Level::High } else { Level::Low });
    }
    fn dc(&mut self, high: bool) {
        self.dc.set_level(if high { Level::High } else { Level::Low });
    }
    fn sclk(&mut self, high: bool) {
        self.sclk.set_level(if high { Level::High } else { Level::Low });
    }
    fn mosi_drive(&mut self, high: bool) {
        self.mosi.set_output_enable(true);
        self.mosi.set_level(if high { Level::High } else { Level::Low });
    }
    fn mosi_release(&mut self) {
        self.mosi.set_output_enable(false);
        self.mosi.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        self.mosi.set_input_enable(true);
    }
    fn mosi_read(&mut self) -> bool {
        self.mosi.is_high()
    }
    fn delay_us(&mut self, us: u32) {
        self.delay.delay_micros(us);
    }
}

/// Milliseconds the system timer missed while the clocks were off in a light sleep.
///
/// Everything that asks the time — the wall clock, the one-second tick, the idle
/// timeouts — reads it through [`uptime_ms`], so accounting for a sleep here is enough to
/// keep the whole device's sense of time straight across one.
static SLEPT_MS: AtomicU32 = AtomicU32::new(0);

fn uptime_ms() -> u32 {
    (Instant::now().as_millis() as u32).wrapping_add(SLEPT_MS.load(Ordering::Relaxed))
}

fn tick_uptime() {
    UPTIME_MS.store(uptime_ms(), Ordering::Relaxed);
}

/// Light sleep for at most `ms`, waking early if the Power key goes down.
///
/// The system timer is clocked from a crystal that light sleep gates, so it does not
/// advance while the core is down; the RTC's own counter does. Measuring both and booking
/// the difference keeps the device's sense of time straight whether the sleep ran its full
/// length or the Power key cut it short after a second — and books nothing at all on a
/// part or a HAL where the system timer turns out to keep running.
fn doze(rtc: &mut Rtc<'static>, ms: u32) {
    let before_sys = Instant::now().as_millis() as u32;
    let before_rtc = (rtc.current_time_us() / 1000) as u32;
    {
        // SAFETY: GPIO3 is also held as the `Input` in `keys`; the wake source only
        // programs the RTC wake bits of the same pad and does not change its mode.
        let mut wake_pin = unsafe { esp_hal::peripherals::GPIO3::steal() };
        let mut pins: [(&mut dyn RtcPinWithResistors, WakeupLevel); 1] = [(&mut wake_pin, WakeupLevel::Low)];
        let gpio = RtcioWakeupSource::new(&mut pins);
        let timer = TimerWakeupSource::new(core::time::Duration::from_millis(ms as u64));
        rtc.sleep_light(&[&timer, &gpio]);
    }
    let slept = ((rtc.current_time_us() / 1000) as u32).wrapping_sub(before_rtc);
    let counted = (Instant::now().as_millis() as u32).wrapping_sub(before_sys);
    let missed = slept.saturating_sub(counted);
    if missed > 0 {
        // The core has no atomic read-modify-write, and none is needed: every sleep is
        // entered and left from the one executor.
        SLEPT_MS.store(SLEPT_MS.load(Ordering::Relaxed).wrapping_add(missed), Ordering::Relaxed);
    }
}

/// Draw a full-screen message (before the UI exists, or when the card is missing).
fn message_frame(title: &str, body: &str) -> Frame {
    let mut f = Frame::panel();
    f.clear(Ink::White);
    let t = quire_fonts::ui::title();
    let b = quire_fonts::ui::body();
    draw_text(&mut f, t, 32, 300, title, TextStyle::INK);
    let mut y = 360;
    for line in quire_ui::text::wrap(b, body, 464) {
        draw_text(&mut f, b, 32, y, &line, TextStyle::INK);
        y += 30;
    }
    f
}

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);

    // Heap: the region the bootloader leaves behind plus the main region. Everything
    // large and long-lived (frame, page cache, section text, a Wi-Fi session) lives
    // here. The main region is sized so the linker leaves the main stack about 40 KB:
    // DRAM is 313 KB for data, bss (this heap, the 52 KB panel plane, the network
    // statics), the mirrored IRAM code and the stack together.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 144 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
    tick_uptime();

    let reset = esp_hal::system::reset_reason();
    let wake = esp_hal::system::wakeup_cause();
    println!("quire-x3 {BUILD} boot: reset {reset:?}, wake {wake:?}, heap {} B", esp_alloc::HEAP.free());
    power::release_holds();

    // Power rails and the shared SPI bus first: the card and the panel hang off it.
    let mut sd_power = Output::new(peripherals.GPIO13, Level::High, OutputConfig::default());
    let sd_cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let epd_cs = Output::new(peripherals.GPIO21, Level::High, OutputConfig::default());
    let epd_dc = Output::new(peripherals.GPIO4, Level::High, OutputConfig::default());
    let epd_rst = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let epd_busy = Input::new(peripherals.GPIO6, InputConfig::default().with_pull(Pull::Up));
    Timer::after(Duration::from_millis(20)).await;

    // The controller probe talks to the panel half-duplex on the MOSI pad, bit-banged, before
    // the SPI peripheral claims the pins.
    let (probe, epd_dc, epd_rst, epd_cs) = {
        let sclk = Output::new(peripherals.GPIO8.reborrow(), Level::Low, OutputConfig::default());
        let mut mosi = esp_hal::gpio::Flex::new(peripherals.GPIO10.reborrow());
        mosi.apply_output_config(&OutputConfig::default());
        mosi.set_output_enable(true);
        mosi.set_low();
        let mut pins = ProbePins { sclk, mosi, rst: epd_rst, cs: epd_cs, dc: epd_dc, delay: esp_hal::delay::Delay::new() };
        let r = quire_epd::probe(&mut pins);
        pins.mosi.set_output_enable(false);
        (r, pins.dc, pins.rst, pins.cs)
    };
    println!("panel probe: {:?}", probe.verdict);

    let spi = Spi::new(peripherals.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(10)).with_mode(esp_hal::spi::Mode::_0))
        .expect("spi")
        .with_sck(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO10)
        .with_miso(peripherals.GPIO7);
    let bus: &'static SharedBus = BUS.init(SharedBus::new(spi, 10_000_000));

    let mut display = Display::new(bus, epd_dc, epd_rst, epd_busy, epd_cs, probe);

    // I²C: gauge, clock, IMU.
    let mut i2c_bus = I2c::new(peripherals.I2C0, I2cConfig::default().with_frequency(Rate::from_khz(400)))
        .expect("i2c")
        .with_sda(peripherals.GPIO20)
        .with_scl(peripherals.GPIO0);
    let battery = i2c::read_battery(&mut i2c_bus);
    let clock = i2c::read_clock(&mut i2c_bus);
    let imu = i2c::Imu::init(&mut i2c_bus);
    println!("battery {battery:?}, clock {clock:?}, imu {}", imu.is_some());

    // Keys.
    let mut adc_cfg = AdcConfig::new();
    let g1: KeyPin1 = adc_cfg.enable_pin_with_cal(peripherals.GPIO1, Attenuation::_11dB);
    let g2: KeyPin2 = adc_cfg.enable_pin_with_cal(peripherals.GPIO2, Attenuation::_11dB);
    let adc = Adc::new(peripherals.ADC1, adc_cfg);
    let power_key = Input::new(peripherals.GPIO3, InputConfig::default().with_pull(Pull::Up));
    let mut keys = Keys { adc, g1, g2, power: power_key, machine: KeyMachine::new() };

    // Local time: the clock chip, else the resume block, else a fixed epoch the first-run
    // wizard corrects.
    let resume = power::load();
    let local_now = clock.or(resume.map(|r| r.clock).filter(|c| *c > 1_600_000_000)).unwrap_or(1_789_000_000);
    let clean =
        matches!(reset, Some(esp_hal::rtc_cntl::SocResetReason::ChipPowerOn) | Some(esp_hal::rtc_cntl::SocResetReason::CoreDeepSleep))
            || !matches!(wake, esp_hal::system::SleepSource::Undefined);
    let crashes = power::note_boot(clean, local_now);
    let safe_mode = crashes >= power::SAFE_MODE_CRASHES;
    if safe_mode {
        println!("safe mode after {crashes} crashes");
    }
    // The flash handle (shared with the OTA code) and the assets partition that holds
    // the dictionary; without it the Dictionary screen offers card dictionaries only.
    let assets = quire_board::assets::FlashRegion::assets(esp_storage::FlashStorage::new(peripherals.FLASH));
    match &assets {
        Some(a) => println!("assets partition at {:#x}", a.base()),
        None => println!("assets partition not found"),
    }

    // Back held through power-on for a second boots the recovery app (the two ladder
    // keys cannot be told apart when pressed together, so one key does it).
    {
        let mut held = 0u32;
        for _ in 0..10 {
            let (g1, _, _) = keys.raw();
            if quire_board::keys::Ladders::decode(
                &quire_board::keys::levels::GROUP1,
                quire_board::keys::levels::IDLE_ABOVE,
                g1,
                quire_board::keys::levels::WINDOW1,
            )
                == Some(quire_board::keys::Key::Back)
            {
                held += 1;
            } else {
                break;
            }
            Timer::after(Duration::from_millis(100)).await;
        }
        if held >= 10 {
            println!("Back held at boot: entering recovery");
            if let Some(f) = quire_board::flash::shared() {
                let _ = quire_board::ota::boot_recovery(f);
                esp_hal::system::software_reset();
            }
        }
    }
    // Repeated crashes on an image that was never confirmed: go back to the last good
    // one ourselves, since the prebuilt bootloader does not do the app-rollback dance.
    if safe_mode {
        if let Some(f) = quire_board::flash::shared() {
            use quire_board::otadata::ImageState;
            if matches!(quire_board::ota::current_state(f), Ok(Some(ImageState::New | ImageState::PendingVerify))) {
                match quire_board::ota::rollback(f, &mut |_| {}) {
                    Ok(slot) => {
                        println!("rolled back to {slot:?}");
                        esp_hal::system::software_reset();
                    }
                    Err(e) => println!("rollback: {e}"),
                }
            }
        }
    }

    // Mount the card; without one, say so and wait for it. The card's CS pin is re-created
    // per attempt because a failed mount consumes it with the discarded card object.
    // Dropping the output releases GPIO12 (it holds the pin's lifetime, not a Drop impl).
    #[allow(clippy::drop_non_drop)]
    drop(sd_cs);
    let fs = loop {
        // SAFETY: GPIO12 was released above and is used by nothing else.
        let cs = Output::new(unsafe { esp_hal::peripherals::GPIO12::steal() }, Level::High, OutputConfig::default());
        match SdFs::mount(bus, cs, &VM) {
            Ok(fs) => break fs,
            Err(e) => {
                println!("card: {e}");
                let f = message_frame("No card", "Insert a microSD card (FAT32) with your books and press any key.");
                display.show(&f, Refresh::Gc, 1, || {});
                loop {
                    Timer::after(Duration::from_millis(50)).await;
                    tick_uptime();
                    if !keys.sample(uptime_ms()).is_empty() {
                        break;
                    }
                }
                sd_power.set_low();
                Timer::after(Duration::from_millis(200)).await;
                sd_power.set_high();
                Timer::after(Duration::from_millis(50)).await;
            }
        }
    };
    let _ = safe_mode;

    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let serial = alloc::format!("{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    let mut env = DeviceEnv::new(fs, local_now, display.name(), serial, BUILD);
    if let Some(b) = battery {
        env.battery = b;
    }
    env.assets = assets;

    // The network task owns the radio and a clone of the card handle; it sleeps until a
    // screen asks for the radio.
    let net_fs: &'static SdFs = NET_FS.init(env.fs.clone());
    let card: &'static dyn CardFs = net_fs;
    env.saved_networks = quire_net::init_from_card(card);
    let seed = {
        let rng = esp_hal::rng::Rng::new();
        (rng.random() as u64) << 32 | rng.random() as u64
    };
    let mac6 = [mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]];
    match quire_net::net_task(NetTaskArgs { wifi: peripherals.WIFI, fs: card, seed, mac: mac6 }) {
        Ok(token) => spawner.spawn(token),
        Err(e) => println!("net task: {e:?}"),
    }

    let mut ui = Ui::new(&mut env);
    display.set_upside_down(ui.settings.left_handed);
    let refresh = ui.draw(&mut env);
    display.show(ui.frame(), refresh.max(Refresh::Gc), ui.settings.gc_every_pages, || {});
    println!("ui up: {} free heap", esp_alloc::HEAP.free());
    if let Some(e) = quire_board::flash::shared().and_then(|f| quire_board::ota::mark_valid(f).err()) {
        println!("ota: mark valid: {e}"); // the booted image is confirmed: its otadata state becomes Valid
    }

    let mut rtc = Rtc::new(peripherals.LPWR);
    let mut last_tick = uptime_ms();
    let mut last_battery = uptime_ms();
    let mut last_activity = uptime_ms();
    let mut timer_due: Option<u32> = None;
    let mut ingest_queue: Vec<quire_library::BookId> = Vec::new();
    let mut scanned = false;
    let mut last_mirror = 0u32;
    let mut reading_up = false;

    loop {
        tick_uptime();
        let now_ms = uptime_ms();
        let mut refresh = Refresh::None;

        // Keys at 100 Hz.
        for ev in keys.sample(now_ms) {
            last_activity = now_ms;
            refresh = refresh.max(ui.handle(&mut env, Event::Key(ev)));
        }

        // Once a second: clock, tick, battery every 30 s, idle timeout.
        if now_ms.wrapping_sub(last_tick) >= 1000 {
            last_tick = now_ms;
            env.tick_clock();
            refresh = refresh.max(ui.handle(&mut env, Event::Tick));
            publish_status(&mut ui, &env, now_ms, reading_up);
            if now_ms.wrapping_sub(last_battery) >= 30_000 {
                last_battery = now_ms;
                if let Some(b) = i2c::read_battery(&mut i2c_bus) {
                    if b != env.battery {
                        env.battery = b;
                        refresh = refresh.max(ui.handle(&mut env, Event::Battery(b)));
                    }
                }
            }
            let idle_min = now_ms.wrapping_sub(last_activity) / 60_000;
            if !ui.asleep && ui.settings.sleep_after_min > 0 && idle_min >= ui.settings.sleep_after_min as u32 {
                refresh = refresh.max(ui.handle(&mut env, Event::Key(quire_ui::KeyEvent::press(quire_ui::Key::Power))));
            }
        }
        if let Some(due) = timer_due {
            if now_ms.wrapping_sub(due) < 1 << 31 {
                timer_due = None;
                refresh = refresh.max(ui.handle(&mut env, Event::Timer));
            }
        }

        // Requests the screens made.
        let mut go_to_sleep = false;
        for req in env.take_requests() {
            match req {
                SysRequest::Sleep => go_to_sleep = true,
                SysRequest::PowerOff => {
                    ui.flush(&mut env);
                    deep_sleep(&mut display, &mut sd_power, &mut rtc, &env);
                }
                SysRequest::Restart => {
                    ui.flush(&mut env);
                    esp_hal::system::software_reset();
                }
                SysRequest::Recovery => {
                    ui.flush(&mut env);
                    // An erased otadata makes the bootloader start the factory slot.
                    if let Some(e) = quire_board::flash::shared().and_then(|f| quire_board::ota::boot_recovery(f).err()) {
                        println!("recovery: {e}");
                    }
                    esp_hal::system::software_reset();
                }
                SysRequest::RefreshFull => refresh = Refresh::Gc,
                SysRequest::SetTime(t) => {
                    env.set_clock(t);
                    let _ = i2c::set_clock(&mut i2c_bus, t);
                }
                SysRequest::Timer(ms) => timer_due = Some(now_ms.wrapping_add(ms)),
                SysRequest::LockKeys(_) => {}
                SysRequest::Screenshot => {
                    let name = alloc::format!("/screenshot-{}.pbm", env.now());
                    let r = ui.frame().as_bitmap();
                    let bm = quire_gfx::Bitmap { w: r.w, h: r.h, bits: r.bits.to_vec() };
                    let _ = quire_library::cache::write_pbm(&env.fs, &name, &bm);
                }
                SysRequest::Rescan => scanned = false,
                SysRequest::IngestNow => ingest_queue = ui.lib.pending(),
                SysRequest::Orientation(_) => {}
                SysRequest::NightJobs(_) => {}
                SysRequest::Calibre(on) => net_send(NetCommand::Calibre(on)),
                SysRequest::SyncNow => net_send(NetCommand::SyncNow),
                SysRequest::WifiOn => net_send(NetCommand::WifiOn),
                SysRequest::WifiOff => net_send(NetCommand::WifiOff),
                SysRequest::Hotspot => net_send(NetCommand::Hotspot),
                SysRequest::WifiScan => net_send(NetCommand::Scan),
                SysRequest::WifiJoin { ssid, password } => net_send(NetCommand::Join { ssid, password }),
                SysRequest::WifiForget(ssid) => net_send(NetCommand::Forget(ssid)),
                SysRequest::Fetch(req) => net_send(NetCommand::Fetch(req)),
                // A card path installs on this task (the flash handle is not shared with
                // the network task); a URL is fetched to the card first by the network
                // task, which reports back with `OtaProgress`.
                SysRequest::Ota(src) if src.starts_with('/') => {
                    ui.flush(&mut env);
                    install_from_card(&mut ui, &mut env, &mut display, &src);
                }
                SysRequest::Ota(src) => net_send(NetCommand::Ota(src)),
            }
        }

        // What the network task reports: UI events, saved networks, settings written by
        // the Drop page, and requests for the current frame.
        while let Some(msg) = quire_net::poll_event() {
            match msg {
                NetToMain::Ui(ev) => {
                    if let Event::Wifi(state) = &ev {
                        env.wifi = state.clone();
                    }
                    refresh = refresh.max(ui.handle(&mut env, ev.clone()));
                    // The update file is on the card: write it into the other slot.
                    if let Event::Net(quire_ui::net::NetEvent::OtaProgress { finished: Some(Ok(())), .. }) = &ev {
                        if let Some(path) = quire_board::ota::find_card_update(&env.fs) {
                            ui.flush(&mut env);
                            install_from_card(&mut ui, &mut env, &mut display, path);
                        }
                    }
                }
                NetToMain::SavedNetworks(names) => env.saved_networks = names,
                NetToMain::SettingsChanged => {
                    ui.settings = quire_ui::Settings::load(&env.fs);
                    refresh = refresh.max(ui.draw(&mut env));
                }
                NetToMain::TimeSync(utc) => {
                    // The RTC keeps local time and there is no zone setting: keep the
                    // offset the user's clock implies (rounded to a quarter hour) and
                    // correct the drift. An unset clock (the fixed first-run epoch) is
                    // left for the wizard, since the zone is unknown.
                    let local = env.now();
                    if local > 1_700_000_000 {
                        let diff = local as i64 - utc as i64;
                        let quarter = 15 * 60;
                        let offset = ((diff + if diff >= 0 { quarter / 2 } else { -quarter / 2 }) / quarter) * quarter;
                        let corrected = (utc as i64 + offset) as u32;
                        if corrected.abs_diff(local) >= 2 {
                            println!("clock: {local} -> {corrected} (sntp, zone {}h)", offset / 3600);
                            env.set_clock(corrected);
                            let _ = i2c::set_clock(&mut i2c_bus, corrected);
                        }
                    }
                }
                NetToMain::ScreenRequest => {
                    let r = ui.frame().as_bitmap();
                    let bm = quire_gfx::Bitmap { w: r.w, h: r.h, bits: r.bits.to_vec() };
                    let ok = quire_library::cache::write_pbm(&env.fs, quire_net::SCREEN_FILE, &bm).is_ok();
                    quire_net::screen_ready(ok);
                }
            }
        }
        let on_page = ui.top_name() == "20-reading";
        if on_page != reading_up {
            reading_up = on_page;
            net_send(NetCommand::Reading(on_page));
        }

        if refresh != Refresh::None {
            display.set_upside_down(ui.settings.left_handed);
            let gc_every = ui.settings.gc_every_pages;
            let frame = ui.frame();
            display.show(frame, refresh, gc_every, || {
                // Keys keep being sampled during the panel's busy wait; they are queued
                // in the state machine and delivered on the next loop pass.
            });
            if quire_net::mirror_wanted() && now_ms.wrapping_sub(last_mirror) >= 500 {
                last_mirror = now_ms;
                let r = frame.as_bitmap();
                quire_net::publish_mirror(r.bits, r.w, r.h);
            }
        }

        if go_to_sleep {
            ui.flush(&mut env);
            display.sleep();
            light_sleep_until_wake(&mut keys, &mut rtc, &mut display, &mut sd_power, &mut i2c_bus, &mut env, &mut ui).await;
            last_activity = uptime_ms();
            let r = ui.handle(&mut env, Event::Wake);
            display.show(ui.frame(), r.max(Refresh::Gc), ui.settings.gc_every_pages, || {});
            continue;
        }

        // Idle work, one unit per pass so keys stay responsive.
        let mut worked = false;
        if !scanned {
            scanned = true;
            let now = env.now();
            match scan(&env.fs, &mut ui.lib, now) {
                Ok(r) if r.added > 0 || r.missing > 0 || r.returned > 0 => {
                    let _ = ui.lib.save(&env.fs);
                    refresh_after_idle(&mut ui, &mut env, &mut display, Event::BooksChanged);
                }
                Ok(_) => {}
                Err(e) => println!("scan: {e}"),
            }
            ingest_queue = ui.lib.pending();
            worked = true;
        } else if let Some(r) = ui.reader.as_mut().filter(|r| !r.index_complete()) {
            r.index_step(&env.fs, &mut ui.lib);
            worked = true;
        } else if let Some(id) = ingest_queue.pop() {
            worked = true;
            let title = ui.lib.get(id).map(|e| e.title.clone()).unwrap_or_default();
            println!("ingest {title}");
            let mut last_shown = 0u32;
            let result = ingest_book(&env.fs, &mut ui.lib, id, &mut |done, total| {
                // Progress reaches the UI at most twice a second.
                let t = uptime_ms();
                if t.wrapping_sub(last_shown) > 500 {
                    last_shown = t;
                    let _ = (done, total);
                }
            });
            let finished = Some(result.map_err(|e| alloc::format!("{e}")));
            let _ = ui.lib.save(&env.fs);
            refresh_after_idle(&mut ui, &mut env, &mut display, Event::Ingest { id, done: 1, total: 1, finished });
        } else if let Some(r) = ui.reader.as_mut() {
            // Prefetch the next section near a boundary so the turn needs no card read.
            r.prefetch_next(&env.fs);
        }

        if worked {
            embassy_futures::yield_now().await;
        } else if can_nap(&keys, &env, now_ms, last_activity) {
            doze(&mut rtc, NAP_MS);
        } else {
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}

/// How long one nap lasts.
///
/// Reading is almost all idle: between one page turn and the next the loop has nothing to
/// do, but sitting in the executor at 160 MHz still costs around 20 mA, where light sleep
/// costs about 130 µA. Napping in slices and sampling the keys on each wake takes the idle
/// draw down by better than a factor of ten.
///
/// The keys are ADC ladders rather than plain GPIOs, so there is no level for the pad to
/// wake on (see `02-hardware.md` §10.7): the slice is a timer wake instead, short enough
/// that no key can be pressed and released inside one. The Power key is a real GPIO and
/// does have a level wake, so it is answered the instant it goes down.
const NAP_MS: u32 = 25;

/// Quiet time before napping starts.
///
/// A run of page turns — holding Down, or reading quickly — stays at the full 100 Hz, so
/// nothing about turning pages changes. It is only once the reader has settled on a page
/// that the loop starts to nap through it.
const NAP_AFTER_MS: u32 = 1_500;

/// Whether the loop may nap instead of spinning.
///
/// Sleeping stops the executor, so it is only safe when nothing else needs to run: the
/// radio has to be off (a session in progress would lose its connections), and nothing
/// may be touching a key. It is also pointless on USB power, where there is no battery to
/// save and a sleep would only interrupt the serial console.
fn can_nap(keys: &Keys, env: &DeviceEnv, now_ms: u32, last_activity: u32) -> bool {
    // The state is read from the field rather than through `Env::wifi`, which clones its
    // strings: this is asked on every idle pass.
    !keys.machine.any_touched()
        && matches!(env.wifi, quire_ui::WifiState::Off)
        && !env.battery.charging
        && now_ms.wrapping_sub(last_activity) >= NAP_AFTER_MS
}

/// Write an update file from the card into the other slot, showing progress on the
/// page, then restart into it. Errors are reported through the same event.
fn install_from_card(ui: &mut Ui<DeviceEnv>, env: &mut DeviceEnv, display: &mut Display, path: &str) {
    use quire_ui::net::NetEvent;
    let Some(flash) = quire_board::flash::shared() else {
        refresh_after_idle(
            ui,
            env,
            display,
            Event::Net(NetEvent::OtaProgress {
                done: 0,
                total: 100,
                finished: Some(Err(alloc::string::String::from("flash unavailable"))),
            }),
        );
        return;
    };
    let mut shown = u32::MAX;
    // The card handle is cloned so the progress closure can draw through `env`.
    let fs = env.fs.clone();
    let result = {
        let mut progress = |pct: u32| {
            if pct != shown && (pct == 100 || shown == u32::MAX || pct.wrapping_sub(shown) >= 5) {
                shown = pct;
                refresh_after_idle(ui, env, display, Event::Net(NetEvent::OtaProgress { done: pct as u64, total: 100, finished: None }));
            }
        };
        quire_board::ota::install_from_card(flash, &fs, path, &mut progress)
    };
    match result {
        Ok((slot, _)) => {
            println!("installed {path} into {slot:?}");
            refresh_after_idle(ui, env, display, Event::Net(NetEvent::OtaProgress { done: 100, total: 100, finished: Some(Ok(())) }));
            ui.flush(env);
            esp_hal::system::software_reset();
        }
        Err(e) => {
            println!("ota: {e}");
            refresh_after_idle(
                ui,
                env,
                display,
                Event::Net(NetEvent::OtaProgress { done: 0, total: 100, finished: Some(Err(alloc::format!("{e}"))) }),
            );
        }
    }
}

/// Hand a command to the network task; a full queue is logged, never blocked on.
fn net_send(cmd: NetCommand) {
    if !quire_net::send_command(cmd) {
        println!("net: command queue full");
    }
}

/// The status the Drop page's API reports, refreshed once a second.
fn publish_status(ui: &mut Ui<DeviceEnv>, env: &DeviceEnv, now_ms: u32, reading: bool) {
    let stats = esp_alloc::HEAP.stats();
    let (book_title, book_percent) = match ui.reader.as_mut() {
        Some(r) => (Some(r.book.meta.title.clone()), (r.info().permille / 10).min(100) as u8),
        None => (None, 0),
    };
    let status = quire_net::StatusInfo {
        battery_percent: env.battery.percent,
        charging: env.battery.charging,
        version: alloc::string::String::from(BUILD),
        build: alloc::string::String::from(BUILD),
        book_title,
        book_percent,
        hostname: ui.settings.hostname.clone(),
        heap_free: (stats.size - stats.current_usage) as u32,
        heap_largest: esp_alloc::HEAP.free() as u32,
        uptime: now_ms / 1000,
        local_now: env.now(),
        reading,
    };
    quire_net::publish_status(status, env.fs.total_bytes());
}

fn refresh_after_idle(ui: &mut Ui<DeviceEnv>, env: &mut DeviceEnv, display: &mut Display, ev: Event) {
    let r = ui.handle(env, ev);
    if r != Refresh::None {
        display.show(ui.frame(), r, ui.settings.gc_every_pages, || {});
    }
}

/// Light sleep in 30 s slices until the Power key wakes us, or deep sleep once the
/// power-off timeout passes. The system timer stops in light sleep, so the clock is
/// re-read from the RTC after every slice, and a sleep screen with a clock is redrawn
/// when the minute changes.
async fn light_sleep_until_wake(
    keys: &mut Keys,
    rtc: &mut Rtc<'static>,
    display: &mut Display,
    sd_power: &mut Output<'static>,
    i2c_bus: &mut i2c::Bus,
    env: &mut DeviceEnv,
    ui: &mut Ui<DeviceEnv>,
) {
    let off_after_ms = (ui.settings.power_off_after_min as u32).saturating_mul(60_000);
    let mut slept_ms = 0u32;
    let mut card_off = false;
    loop {
        // Past the threshold the card's rail comes down for the rest of the sleep, for as
        // long as the screen can keep its clock without reading anything. Should it ever
        // need the card again, the card comes back first and the repaint is unaffected.
        let needs_fs = ui.sleep_tick_needs_fs();
        if card_off && needs_fs {
            wake_card(sd_power, &env.fs).await;
            card_off = false;
        } else if !card_off && !needs_fs && slept_ms >= CARD_OFF_AFTER_MS {
            card_off = true;
            sd_power.set_low();
            power::hold_sd_rail(true);
        }
        doze(rtc, 30_000);
        tick_uptime();
        slept_ms = slept_ms.saturating_add(30_000);
        // `uptime_ms` has already been credited with the slice, so the clock has moved on
        // by itself; the chip is read to correct the drift, not to carry the time.
        if let Some(t) = i2c::read_clock(i2c_bus) {
            env.set_clock(t);
        }
        env.tick_clock();
        // Debounce: the key has to be down for a moment.
        let mut held = 0;
        for _ in 0..6 {
            if keys.raw().2 {
                held += 1;
            }
            Timer::after(Duration::from_millis(10)).await;
        }
        if held >= 4 {
            // Wait for release so the wake does not also register as a press.
            while keys.raw().2 {
                Timer::after(Duration::from_millis(10)).await;
            }
            if card_off {
                wake_card(sd_power, &env.fs).await;
            }
            keys.machine = KeyMachine::new();
            return;
        }
        if off_after_ms > 0 && slept_ms >= off_after_ms {
            deep_sleep(display, sd_power, rtc, env);
        }
        // A sleep screen with a live clock repaints once a minute (a DU refresh); the
        // panel goes back to sleep straight after.
        let r = ui.handle(env, Event::Tick);
        if r != Refresh::None {
            display.show(ui.frame(), r, ui.settings.gc_every_pages, || {});
            display.sleep();
        }
    }
}

/// Light sleep this long before the card's power rail comes down.
///
/// A powered card is the one thing left drawing real current while the reader sleeps, and
/// a sleep is usually long: minutes in a pocket, hours in a bag. Waiting a little first
/// means a reader picked straight back up never pays for the handshake that brings the
/// card back, and a reader left alone stops paying for the card within two minutes.
const CARD_OFF_AFTER_MS: u32 = 2 * 60_000;

/// Bring the card back after [`CARD_OFF_AFTER_MS`] cut its rail.
///
/// A card that will not answer is not fatal here — it is the same state as a card pulled
/// out mid-session, which every path that touches the filesystem already has to handle —
/// but it is worth a few tries first, since a full power cycle is exactly what the card
/// saw at boot.
async fn wake_card(sd_power: &mut Output<'static>, fs: &SdFs) {
    power::release_holds();
    sd_power.set_high();
    Timer::after(Duration::from_millis(50)).await;
    for attempt in 1..=3u32 {
        match fs.reacquire() {
            Ok(()) => return,
            Err(e) => {
                println!("card: re-acquire after sleep failed ({attempt}/3): {e}");
                sd_power.set_low();
                Timer::after(Duration::from_millis(200)).await;
                sd_power.set_high();
                Timer::after(Duration::from_millis(50 * attempt as u64)).await;
            }
        }
    }
    println!("card: not answering after the sleep; leaving the rail up");
}

/// Power everything down and enter deep sleep; only the Power key wakes the device.
fn deep_sleep(display: &mut Display, sd_power: &mut Output<'static>, rtc: &mut Rtc<'static>, env: &DeviceEnv) -> ! {
    display.sleep();
    if let Some(mut r) = power::load() {
        r.clock = env.now();
        power::store(r);
    } else {
        power::store(power::Resume { clock: env.now(), ..Default::default() });
    }
    // The card rail stays off through the sleep (02-hardware.md §9.4).
    sd_power.set_low();
    power::hold_sd_rail(true);
    // SAFETY: see `light_sleep_until_wake`.
    let mut wake_pin = unsafe { esp_hal::peripherals::GPIO3::steal() };
    let mut pins: [(&mut dyn RtcPinWithResistors, WakeupLevel); 1] = [(&mut wake_pin, WakeupLevel::Low)];
    let gpio = RtcioWakeupSource::new(&mut pins);
    rtc.sleep_deep(&[&gpio])
}
