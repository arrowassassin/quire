//! Quire's screen system.
//!
//! Every screen is a static page drawn into a 1-bit [`Frame`] in response to key events.
//! The [`Ui`] owns the screen stack, the open book, the library and the settings; the
//! platform (device or simulator) feeds it [`Event`]s and pushes the frame it returns to
//! the panel with the [`Refresh`] the screen asked for. The key grammar (brief §2) and the
//! signature elements live in `widgets`, `spine` and the screens.

#![no_std]
#![warn(missing_docs)]

extern crate alloc;
#[cfg(any(test, feature = "std"))]
extern crate std;

pub mod dict;
pub mod icons;
pub mod keyboard;
pub mod qr;
pub mod reader;
pub mod screens;
pub mod settings;
pub mod sleeppack;
pub mod spine;
pub mod text;
pub mod theme;
pub mod widgets;
pub mod zmachine;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use quire_fs::Fs;
use quire_gfx::Frame;
use quire_library::{BookId, Library, Stats};

pub use reader::Reader;
pub use settings::Settings;

/// The seven keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Bottom, outer left.
    Left,
    /// Bottom, inner left.
    Back,
    /// Bottom, inner right.
    Confirm,
    /// Bottom, outer right.
    Right,
    /// The side key left of the display. Named for what it does to a list or a
    /// page, not for where it sits: the X3 carries one key either side of the
    /// screen rather than two stacked on the right, whatever 02-hardware.md says.
    Up,
    /// The side key right of the display.
    Down,
    /// Top.
    Power,
}

/// What happened to a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// Pressed and released before the long-press threshold.
    Press,
    /// Held past 500 ms (fires once).
    Long,
    /// Still held: fires 5 times a second after the long press.
    Repeat,
    /// Released after a Long (or Repeats).
    Release,
}

/// A key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// The key.
    pub key: Key,
    /// What happened.
    pub kind: KeyKind,
}

impl KeyEvent {
    /// A short press.
    pub const fn press(key: Key) -> KeyEvent {
        KeyEvent { key, kind: KeyKind::Press }
    }
    /// A long press.
    pub const fn long(key: Key) -> KeyEvent {
        KeyEvent { key, kind: KeyKind::Long }
    }
    /// True for a short press of `key`.
    pub fn is(&self, key: Key) -> bool {
        self.key == key && self.kind == KeyKind::Press
    }
    /// True for a long press of `key`.
    pub fn is_long(&self, key: Key) -> bool {
        self.key == key && self.kind == KeyKind::Long
    }
}

/// How the panel should refresh after a draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refresh {
    /// Nothing changed.
    None,
    /// Partial (DU) refresh, ~380 ms, no flash.
    Du,
    /// Full (GC) refresh with a black-white flash.
    Gc,
}

/// Requests a screen makes of the platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SysRequest {
    /// Enter sleep (the sleep screen is already drawn).
    Sleep,
    /// Power off.
    PowerOff,
    /// Restart.
    Restart,
    /// Full refresh of the current frame.
    RefreshFull,
    /// Turn Wi-Fi on (station).
    WifiOn,
    /// Turn Wi-Fi off.
    WifiOff,
    /// Start the hotspot.
    Hotspot,
    /// Join a network.
    WifiJoin {
        /// SSID.
        ssid: String,
        /// Password.
        password: String,
    },
    /// Forget a saved network.
    WifiForget(String),
    /// Scan for networks.
    WifiScan,
    /// Lock or unlock the keys.
    LockKeys(bool),
    /// Save a screenshot of the current frame.
    Screenshot,
    /// Set the clock (local seconds).
    SetTime(u32),
    /// Start an OTA update from a URL or the SD path.
    Ota(String),
    /// Begin ingest of pending books now.
    IngestNow,
    /// Rescan the card.
    Rescan,
    /// Set the panel orientation.
    Orientation(quire_gfx::Rotation),
    /// Start a timer for `ms` milliseconds (delivered as `Event::Timer`).
    Timer(u32),
    /// The reader wants a night-jobs hour set.
    NightJobs(Option<u8>),
    /// Start or stop the Calibre wireless server.
    Calibre(bool),
    /// Trigger a sync now.
    SyncNow,
    /// Ask the network layer to fetch something (see `net`).
    Fetch(net::FetchRequest),
    /// Reboot into recovery.
    Recovery,
}

/// What a screen wants after handling an event.
pub enum Action<E: Env> {
    /// Nothing.
    None,
    /// Redraw this screen.
    Redraw,
    /// Push a screen.
    Push(Box<dyn Screen<E>>),
    /// Pop this screen.
    Pop,
    /// Pop this screen and push another.
    Replace(Box<dyn Screen<E>>),
    /// Pop everything down to the reading page (or the empty home).
    ToReader,
    /// Pop to the first screen with this name.
    PopTo(&'static str),
    /// Open a book (replaces the reader) and go to it.
    Open(BookId),
    /// Ask the platform for something, then redraw.
    System(SysRequest),
    /// Pop, then deliver a result to the screen beneath.
    PopWith(Result_),
}

/// A value handed to the screen beneath when a picker or keyboard pops.
#[derive(Clone, Debug, PartialEq)]
pub enum Result_ {
    /// Text entered.
    Text(String),
    /// A choice index.
    Choice(usize),
    /// Cancelled.
    Cancel,
    /// A book chosen.
    Book(BookId),
}

/// Battery state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Battery {
    /// Percent 0–100.
    pub percent: u8,
    /// Whether charging.
    pub charging: bool,
    /// Estimated days left, if known.
    pub days_left: Option<u16>,
    /// Cycle count, if known.
    pub cycles: Option<u16>,
    /// Health percent, if known.
    pub health: Option<u8>,
    /// Voltage in mV.
    pub millivolts: u16,
}

/// Wi-Fi state.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum WifiState {
    /// Radio off.
    #[default]
    Off,
    /// Trying to join.
    Connecting(String),
    /// Joined.
    Connected {
        /// Network name.
        ssid: String,
        /// IP address text.
        ip: String,
        /// mDNS host name (without .local).
        host: String,
        /// Signal 0–4.
        signal: u8,
    },
    /// Running the hotspot.
    Hotspot {
        /// Network name.
        ssid: String,
        /// Password.
        password: String,
        /// IP address text.
        ip: String,
    },
    /// Failed to join.
    Failed(String),
}

/// A visible network from a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    /// Name.
    pub ssid: String,
    /// Signal 0–4.
    pub signal: u8,
    /// Whether a password is needed.
    pub secured: bool,
    /// Whether we have it saved.
    pub saved: bool,
}

/// Static facts about the device.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    /// Firmware version.
    pub version: String,
    /// Build date or commit.
    pub build: String,
    /// Panel controller name.
    pub panel: String,
    /// Free heap bytes.
    pub free_heap: u32,
    /// Largest free block.
    pub largest_block: u32,
    /// Flash size bytes.
    pub flash_bytes: u32,
    /// Card total bytes, if a card is present.
    pub card_total: Option<u64>,
    /// Card free bytes.
    pub card_free: Option<u64>,
    /// Serial or MAC text.
    pub serial: String,
}

/// Everything that arrives from the platform.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A key.
    Key(KeyEvent),
    /// A timer requested with `SysRequest::Timer` fired.
    Timer,
    /// Periodic tick (about once a second while awake).
    Tick,
    /// Woke from sleep.
    Wake,
    /// Ingest progress for a book.
    Ingest {
        /// Book.
        id: BookId,
        /// Done units.
        done: u32,
        /// Total units.
        total: u32,
        /// Finished (Ok or the error text).
        finished: Option<Result<(), String>>,
    },
    /// Wi-Fi state changed.
    Wifi(WifiState),
    /// A scan finished.
    WifiScan(Vec<WifiNetwork>),
    /// A network job progressed (see `net`).
    Net(net::NetEvent),
    /// Battery changed.
    Battery(Battery),
    /// Text typed on the phone (Drop page).
    PhoneText(String),
    /// A key sent from the phone.
    PhoneKey(KeyEvent),
    /// New books arrived (a scan is due).
    BooksChanged,
}

pub mod net;

/// The platform behind the UI: clock, battery, radio, system requests.
pub trait Env {
    /// The card filesystem.
    type Fs: Fs;
    /// Card access.
    fn fs(&self) -> &Self::Fs;
    /// The built-in dictionary's bytes: the compiled-in blob where the `builtin-dict`
    /// feature is on; the device reads them from its assets flash partition instead.
    fn dictionary(&self) -> Option<&dyn dict::builtin::DictSource> {
        dict::builtin::compiled_in()
    }
    /// Local time, seconds since 1970 (the RTC keeps local time).
    fn now(&self) -> u32;
    /// Milliseconds since boot (for timers and the skim cadence).
    fn millis(&self) -> u32;
    /// Battery.
    fn battery(&self) -> Battery;
    /// Wi-Fi.
    fn wifi(&self) -> WifiState;
    /// Saved networks.
    fn saved_networks(&self) -> Vec<String>;
    /// Device facts.
    fn device(&self) -> DeviceInfo;
    /// Ask the platform for something.
    fn request(&mut self, req: SysRequest);
    /// Pseudo-random 32 bits.
    fn random(&mut self) -> u32;
    /// The network job runner's state (downloads, catalogs, sync). See `net`.
    fn net(&mut self) -> &mut dyn net::NetState;
}

/// Shared state every screen can reach.
pub struct Ctx<'a, E: Env> {
    /// The platform.
    pub env: &'a mut E,
    /// The library index.
    pub lib: &'a mut Library,
    /// Reading statistics.
    pub stats: &'a mut Stats,
    /// Settings.
    pub settings: &'a mut Settings,
    /// The open book, if any.
    pub reader: &'a mut Option<Reader>,
    /// Books being ingested: (id, done, total).
    pub ingesting: &'a Vec<(BookId, u32, u32)>,
    /// Text last typed from the phone, taken by the focused text field.
    pub phone_text: &'a mut Option<String>,
    /// Whether the keys are locked.
    pub locked: bool,
}

impl<E: Env> Ctx<'_, E> {
    /// Local day number today.
    pub fn today(&self) -> u16 {
        quire_library::time::day_of(self.env.now())
    }
    /// Save the library and settings if they changed.
    pub fn persist(&mut self) {
        let _ = self.lib.save(self.env.fs());
        let _ = self.settings.save(self.env.fs());
    }
}

/// A screen.
pub trait Screen<E: Env> {
    /// Stable name (matches the design artboard prefix, e.g. "22-contents").
    fn name(&self) -> &'static str;
    /// Draw into the frame (the frame arrives cleared, or holding the page beneath for overlays).
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh;
    /// Handle a key.
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E>;
    /// Handle a non-key event; default ignores.
    fn event(&mut self, _cx: &mut Ctx<E>, _ev: &Event) -> Action<E> {
        Action::None
    }
    /// A child screen popped with a result.
    fn result(&mut self, _cx: &mut Ctx<E>, _r: Result_) -> Action<E> {
        Action::Redraw
    }
    /// Whether this screen draws over the one beneath it.
    fn overlay(&self) -> bool {
        false
    }
    /// Called when the screen becomes the top again.
    fn resume(&mut self, _cx: &mut Ctx<E>) {}
    /// A minute passed while the device sleeps behind this screen and `f` still holds
    /// its last draw: repaint what the time changes and say which refresh shows it, or
    /// `Refresh::None` when nothing on the screen tells the time. Default: nothing.
    fn minute_tick(&mut self, _cx: &mut Ctx<E>, _f: &mut Frame) -> Refresh {
        Refresh::None
    }
    /// Whether [`Screen::minute_tick`] has to read the filesystem to do its work. The
    /// platform cuts the card's power rail during a long sleep, and asks this before it
    /// does: a screen that answers `false` keeps its clock ticking through the dark.
    /// Default: `true`, because a screen that redraws itself reads whatever it drew from.
    fn minute_tick_needs_fs(&self) -> bool {
        true
    }
}

/// What the frame holds after a draw, when a later draw can build on it instead of
/// starting from paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// Exactly the reading page (no overlays drawn yet), with the inversion applied to it:
    /// overlays are then drawn over it without a re-render, so no second frame is needed.
    Page(reader::RenderKey, bool),
    /// A sleep screen's draw, at this minute (`now / 60`), with the inversion applied: a
    /// minute tick while asleep repaints only what the time changes.
    Sleep(u32, bool),
}

/// The UI: screen stack plus the state the screens share.
pub struct Ui<E: Env> {
    /// Screens, bottom first. The bottom is always the reader (or the empty home).
    screens: Vec<Box<dyn Screen<E>>>,
    /// Library index.
    pub lib: Library,
    /// Stats.
    pub stats: Stats,
    /// Settings.
    pub settings: Settings,
    /// The open book.
    pub reader: Option<Reader>,
    /// Ingest progress.
    pub ingesting: Vec<(BookId, u32, u32)>,
    phone_text: Option<String>,
    /// Whether the keys are locked.
    pub locked: bool,
    frame: Frame,
    /// What the frame holds, when the next draw can reuse it (see [`Held`]).
    frame_holds: Option<Held>,
    /// Set while the platform is asleep (sleep screen shown).
    pub asleep: bool,
    last_saved: u32,
    /// Hash of the settings as last written, so the periodic save touches the card only
    /// when something changed.
    settings_hash: u64,
}

impl<E: Env> Ui<E> {
    /// Build the UI: load the library, stats and settings and open the current book.
    pub fn new(env: &mut E) -> Self {
        let fs = env.fs();
        let lib = Library::load(fs);
        let stats = Stats::load(fs);
        let settings = Settings::load(fs);
        let settings_hash = settings_hash(&settings);
        let mut ui = Ui {
            screens: Vec::new(),
            lib,
            stats,
            settings,
            reader: None,
            ingesting: Vec::new(),
            phone_text: None,
            locked: false,
            frame: Frame::panel(),
            frame_holds: None,
            asleep: false,
            last_saved: 0,
            settings_hash,
        };
        if !ui.settings.first_run_done {
            ui.screens.push(Box::new(screens::firstrun::FirstRun::new()));
        } else {
            ui.open_current(env);
            ui.screens.push(Box::new(screens::reading::ReadingScreen::new()));
        }
        ui
    }

    /// Open the library's current book into the reader.
    pub fn open_current(&mut self, env: &mut E) {
        if let Some(id) = self.lib.current {
            self.open_book(env, id);
        }
    }

    /// Open a book by id into the reader.
    pub fn open_book(&mut self, env: &mut E, id: BookId) -> bool {
        self.close_book(env);
        self.frame_holds = None;
        match Reader::open(env.fs(), &self.lib, &self.settings, id, env.now()) {
            Ok(r) => {
                self.reader = Some(r);
                self.lib.opened(id, env.now());
                true
            }
            Err(_) => false,
        }
    }

    /// Close the open book, recording the session.
    pub fn close_book(&mut self, env: &mut E) {
        self.frame_holds = None;
        if let Some(mut r) = self.reader.take() {
            r.close(env.fs(), &mut self.lib, &mut self.stats, env.now());
        }
        let _ = self.lib.save(env.fs());
    }

    /// The current frame.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Name of the top screen.
    pub fn top_name(&self) -> &'static str {
        self.screens.last().map(|s| s.name()).unwrap_or("")
    }

    /// Names of all screens, bottom first.
    pub fn stack_names(&self) -> Vec<&'static str> {
        self.screens.iter().map(|s| s.name()).collect()
    }

    fn ctx<'a>(&'a mut self, env: &'a mut E) -> (Ctx<'a, E>, &'a mut Vec<Box<dyn Screen<E>>>) {
        let cx = Ctx {
            env,
            lib: &mut self.lib,
            stats: &mut self.stats,
            settings: &mut self.settings,
            reader: &mut self.reader,
            ingesting: &self.ingesting,
            phone_text: &mut self.phone_text,
            locked: self.locked,
        };
        (cx, &mut self.screens)
    }

    /// Handle an event; returns the refresh to perform with [`Ui::frame`].
    pub fn handle(&mut self, env: &mut E, ev: Event) -> Refresh {
        // Bookkeeping events first.
        match &ev {
            Event::Ingest { id, done, total, finished } => {
                self.ingesting.retain(|(i, _, _)| i != id);
                match finished {
                    None => self.ingesting.push((*id, *done, *total)),
                    Some(res) => {
                        if let Some(e) = self.lib.get_mut(*id) {
                            match res {
                                Ok(()) => {
                                    // The platform's ingest driver already updated the entry
                                    // through the shared library; reload to be safe.
                                }
                                Err(msg) => {
                                    e.ingest = quire_library::IngestState::Failed;
                                    e.error = Some(msg.clone());
                                }
                            }
                        }
                    }
                }
            }
            Event::PhoneText(t) => self.phone_text = Some(t.clone()),
            Event::Wake => self.asleep = false,
            _ => {}
        }
        // Asleep: the platform ticks about once a minute; the sleep screen repaints its
        // time in the frame it still holds (a DU), and nothing else happens.
        if matches!(ev, Event::Tick) && self.asleep && is_sleep_screen(self.top_name()) {
            return self.sleep_tick(env);
        }
        let action = {
            let locked = self.locked;
            let (mut cx, screens) = self.ctx(env);
            let Some(top) = screens.last_mut() else { return Refresh::None };
            match &ev {
                Event::Key(k) if locked => {
                    // Locked: only holding Power unlocks; anything else shows the strip
                    // (once: a strip already showing, or the sleep screen, takes the key).
                    if k.key == Key::Power && k.kind == KeyKind::Long {
                        cx.env.request(SysRequest::LockKeys(false));
                        Action::System(SysRequest::LockKeys(false))
                    } else if top.name() == "45-locked" || is_sleep_screen(top.name()) || top.name() == "44-picker-preview" {
                        top.key(&mut cx, *k)
                    } else {
                        Action::Push(Box::new(screens::locked::LockedStrip::new()))
                    }
                }
                Event::Key(k) if is_sleep_screen(top.name()) || top.name() == "44-picker-preview" => {
                    // Asleep (or previewing a sleep screen): the screen owns every key, so
                    // Power wakes rather than stacking another sleep screen or power menu.
                    top.key(&mut cx, *k)
                }
                Event::Key(k) => {
                    // Universal grammar: long Back opens Jump from anywhere except Jump itself.
                    if k.is_long(Key::Back) && top.name() != "42-jump" && top.name() != "01-boot" && top.name() != "99-recovery" {
                        // Jump is a launcher, not a hierarchy: one already open beneath
                        // (behind its keyboard, say) is returned to rather than stacked.
                        if screens.iter().any(|s| s.name() == "42-jump") {
                            Action::PopTo("42-jump")
                        } else {
                            Action::Push(Box::new(screens::jump::Jump::new()))
                        }
                    } else if k.is_long(Key::Power) && top.name() != "41-power" {
                        Action::Push(Box::new(screens::power::PowerMenu::new()))
                    } else if k.is(Key::Power) && top.name() != "41-power" {
                        Action::Push(Box::new(screens::sleep::SleepScreen::new()))
                    } else {
                        top.key(&mut cx, *k)
                    }
                }
                Event::PhoneKey(k) => top.key(&mut cx, *k),
                other => top.event(&mut cx, other),
            }
        };
        let refresh = self.apply(env, action);
        // A sleep screen on top means the device goes to sleep once this frame is on the
        // panel: persist everything and ask the platform, whichever screen put it there.
        let top = self.top_name();
        if is_sleep_screen(top) && !self.asleep {
            self.asleep = true;
            self.flush(env);
            env.request(SysRequest::Sleep);
        } else if !self.screens.iter().any(|s| is_sleep_screen(s.name())) {
            // The sleep screen left (a Power press, or the platform's Wake): the next one
            // must ask for sleep again.
            self.asleep = false;
        }
        // Periodic persistence of positions, stats and settings: on the idle tick, never
        // inside a key press, and only what changed (the library keeps its own dirty flags).
        if matches!(ev, Event::Tick) {
            let now = env.now();
            if now.saturating_sub(self.last_saved) >= 30 {
                self.last_saved = now;
                if let Some(r) = self.reader.as_mut() {
                    r.save_position(&mut self.lib);
                }
                let _ = self.lib.save(env.fs());
                self.save_settings_if_changed(env);
            }
        }
        refresh
    }

    /// A tick while asleep: when the minute has changed since the sleep screen was drawn,
    /// the screen repaints its time in the frame (which still holds its last draw) and a
    /// DU shows it; within the same minute nothing happens. `Refresh::None` either way
    /// when the screen shows no time.
    /// Whether the next sleeping minute tick needs the card.
    ///
    /// The platform cuts the card's power rail once a light sleep has run long enough to
    /// be worth it; it asks this first. `true` when the frame no longer holds the sleep
    /// screen (the tick redraws from scratch, which reads the card) or when the screen
    /// itself says it reads.
    pub fn sleep_tick_needs_fs(&self) -> bool {
        if !matches!(self.frame_holds, Some(Held::Sleep(..))) {
            return true;
        }
        self.screens.last().is_none_or(|s| s.minute_tick_needs_fs())
    }

    fn sleep_tick(&mut self, env: &mut E) -> Refresh {
        let minute = env.now() / 60;
        let Some(Held::Sleep(drawn, inverted)) = self.frame_holds else {
            // The frame holds something else (the platform asked for a draw elsewhere):
            // draw the sleep screen afresh so the next tick can build on it.
            self.draw(env);
            return Refresh::Du;
        };
        if drawn == minute {
            return Refresh::None;
        }
        let Ui { screens, lib, stats, settings, reader, ingesting, phone_text, locked, frame, .. } = self;
        let mut cx = Ctx { env, lib, stats, settings, reader, ingesting, phone_text: &mut *phone_text, locked: *locked };
        let mut refresh = Refresh::None;
        if let Some(top) = screens.last_mut() {
            if inverted {
                frame.invert_rect(frame.bounds());
            }
            refresh = top.minute_tick(&mut cx, frame);
            if inverted {
                frame.invert_rect(frame.bounds());
            }
        }
        self.frame_holds = Some(Held::Sleep(minute, inverted));
        refresh
    }

    /// Write the settings when they differ from what was last written.
    fn save_settings_if_changed(&mut self, env: &mut E) {
        let h = settings_hash(&self.settings);
        if h != self.settings_hash && self.settings.save(env.fs()).is_ok() {
            self.settings_hash = h;
        }
    }

    /// Identity of the screen beneath the overlays (index and address), to tell a change
    /// of page from a focus move or an overlay coming and going.
    fn base_id(&self) -> (usize, *const ()) {
        let mut i = self.screens.len().saturating_sub(1);
        while i > 0 && self.screens[i].overlay() {
            i -= 1;
        }
        let ptr = self.screens.get(i).map(|s| s.as_ref() as *const dyn Screen<E> as *const ()).unwrap_or(core::ptr::null());
        (i, ptr)
    }

    /// Apply an action as if the top screen returned it (the platform and tests use it to
    /// open Jump targets directly).
    pub fn apply(&mut self, env: &mut E, action: Action<E>) -> Refresh {
        // Refresh policy: a change of the screen beneath the overlays is a full (GC)
        // refresh; a redraw of the same screen is a DU unless the screen itself asks for
        // GC (a page turn on its cadence, a list page change); overlays are always DU.
        let structural = matches!(
            action,
            Action::Push(_) | Action::Pop | Action::Replace(_) | Action::ToReader | Action::PopTo(_) | Action::Open(_) | Action::PopWith(_)
        );
        let before = self.base_id();
        let refresh = self.apply_inner(env, action);
        if structural && refresh != Refresh::None && self.base_id() != before {
            Refresh::Gc
        } else {
            refresh
        }
    }

    fn apply_inner(&mut self, env: &mut E, action: Action<E>) -> Refresh {
        match action {
            Action::None => Refresh::None,
            Action::Redraw => self.draw(env),
            Action::Push(s) => {
                self.push_bounded(s);
                self.draw(env)
            }
            Action::Pop => {
                if self.screens.len() > 1 {
                    self.screens.pop();
                }
                self.resume_top(env);
                self.draw(env)
            }
            Action::Replace(s) => {
                if self.screens.len() > 1 {
                    self.screens.pop();
                }
                self.push_bounded(s);
                self.draw(env)
            }
            Action::ToReader => {
                self.screens.truncate(1);
                if self.screens.is_empty() || self.screens[0].name() != "20-reading" {
                    self.screens.clear();
                    self.screens.push(Box::new(screens::reading::ReadingScreen::new()));
                }
                self.resume_top(env);
                self.draw(env)
            }
            Action::PopTo(name) => {
                while self.screens.len() > 1 && self.screens.last().map(|s| s.name()) != Some(name) {
                    self.screens.pop();
                }
                self.resume_top(env);
                self.draw(env)
            }
            Action::Open(id) => {
                self.open_book(env, id);
                self.screens.clear();
                self.screens.push(Box::new(screens::reading::ReadingScreen::new()));
                self.draw(env)
            }
            Action::System(req) => {
                match &req {
                    SysRequest::Sleep => self.asleep = true,
                    SysRequest::LockKeys(l) => self.locked = *l,
                    _ => {}
                }
                env.request(req);
                self.draw(env)
            }
            Action::PopWith(r) => {
                if self.screens.len() > 1 {
                    self.screens.pop();
                }
                let next = {
                    let (mut cx, screens) = self.ctx(env);
                    match screens.last_mut() {
                        Some(top) => top.result(&mut cx, r),
                        None => Action::None,
                    }
                };
                match next {
                    Action::None => self.draw(env),
                    other => self.apply(env, other),
                }
            }
        }
    }

    /// Push a screen, forgetting the oldest history above the root when the stack is at
    /// its limit (Jump can launch from any screen, so history is bounded, not the depth).
    fn push_bounded(&mut self, s: Box<dyn Screen<E>>) {
        while self.screens.len() >= MAX_STACK {
            self.screens.remove(1);
        }
        self.screens.push(s);
    }

    fn resume_top(&mut self, env: &mut E) {
        let (mut cx, screens) = self.ctx(env);
        if let Some(top) = screens.last_mut() {
            top.resume(&mut cx);
        }
    }

    /// Draw the stack into the frame: the topmost non-overlay screen, then overlays above
    /// it. When the frame already holds the reading page the overlays sit on, the page is
    /// kept and only the overlays are drawn.
    pub fn draw(&mut self, env: &mut E) -> Refresh {
        let inverted = self.settings.inverted;
        let (start, _) = self.base_id();
        let has_overlays = start + 1 < self.screens.len();
        let page_key = match self.screens.get(start) {
            Some(s) if s.name() == "20-reading" => self.reader.as_ref().map(|r| r.render_key(&self.settings)),
            _ => None,
        };
        let reuse = has_overlays && page_key.is_some() && self.frame_holds == page_key.map(|k| Held::Page(k, inverted));
        let sleep_base = self.screens.get(start).is_some_and(|s| is_sleep_screen(s.name()));
        let minute = env.now() / 60;
        let Ui { screens, lib, stats, settings, reader, ingesting, phone_text, locked, frame, .. } = self;
        let mut cx = Ctx { env, lib, stats, settings, reader, ingesting, phone_text: &mut *phone_text, locked: *locked };
        let mut refresh = Refresh::Du;
        if reuse {
            // Undo the inversion so the overlays draw on the page as rendered.
            if inverted {
                frame.invert_rect(frame.bounds());
            }
        } else {
            frame.clear(quire_gfx::Ink::White);
            if let Some(base) = screens.get_mut(start) {
                refresh = base.draw(&mut cx, frame);
            }
        }
        let n = screens.len();
        for s in screens[(start + 1).min(n)..].iter_mut() {
            // Overlays cover part of the page: a DU is enough whatever they return.
            let _ = s.draw(&mut cx, frame);
        }
        if inverted {
            frame.invert_rect(frame.bounds());
        }
        self.frame_holds = if has_overlays {
            None
        } else if let Some(k) = page_key {
            Some(Held::Page(k, inverted))
        } else if sleep_base {
            Some(Held::Sleep(minute, inverted))
        } else {
            None
        };
        refresh
    }

    /// Push a screen from outside (the platform opening Drop on a hotspot, tests).
    pub fn push(&mut self, env: &mut E, s: Box<dyn Screen<E>>) -> Refresh {
        self.apply(env, Action::Push(s))
    }

    /// Persist everything now (before sleep or power off).
    pub fn flush(&mut self, env: &mut E) {
        if let Some(r) = self.reader.as_mut() {
            r.save_position(&mut self.lib);
            r.flush_session(env.fs(), &mut self.lib, &mut self.stats, env.now());
        }
        let _ = self.lib.save(env.fs());
        if self.settings.save(env.fs()).is_ok() {
            self.settings_hash = settings_hash(&self.settings);
        }
    }
}

/// FNV-1a of the settings' encoding: a cheap in-RAM "did anything change" check.
fn settings_hash(settings: &Settings) -> u64 {
    let bytes = postcard::to_allocvec(settings).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Deepest the screen stack gets: the root plus seven screens of history.
pub const MAX_STACK: usize = 8;

/// Whether a screen name is the sleep screen the device sleeps behind.
fn is_sleep_screen(name: &str) -> bool {
    name == "40-sleep" || name == "40-sleep-charging"
}

/// Root of Quire's files on the card.
pub const ROOT: &str = quire_library::ROOT;
