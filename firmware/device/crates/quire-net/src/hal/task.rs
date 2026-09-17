//! The net task: it sleeps until a command needs the radio, then runs one *session* —
//! controller, stack, servers — until Wi-Fi is turned off (or idles out), and drops
//! everything so the driver's heap goes back to the reader.

use alloc::boxed::Box;
use alloc::string::String;
use core::net::Ipv4Addr;

use embassy_futures::join::{join4, join5};
use embassy_futures::select::{select, Either};
use embassy_net::{Config as NetConfigV4, DhcpConfig, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::WifiController;
use quire_ui::{Event, Settings, WifiState};

use super::{calibre, captive, dhcp, fetch, http, mdns, now_ms, wifi};
use crate::proto::hotspot;
use crate::proto::wifi_bin::{scan_list, NetConfig};
use crate::proto::wsmsg;
use crate::{load_config, next_command, post, save_config, with, ws_broadcast, CardFs, DynFs, NetCommand, NetToMain};

/// What the task needs from `main`.
pub struct NetTaskArgs {
    /// The radio peripheral.
    pub wifi: WIFI<'static>,
    /// The card.
    pub fs: &'static dyn CardFs,
    /// Random seed for the stack and the hotspot password.
    pub seed: u64,
    /// The base MAC (names the hotspot).
    pub mac: [u8; 6],
}

/// Idle time after which the radio goes off while the reader is on the page (06 §4b).
const IDLE_OFF_MS: u32 = 10 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    Station { target: Option<(String, String)> },
    Hotspot,
}

enum Outcome {
    Off,
    Switch(Mode),
}

#[derive(Default)]
struct Flags {
    /// The reading page is the top screen.
    reading: bool,
    /// A scan was asked for while the radio was off.
    scan_pending: bool,
}

/// Publish a Wi-Fi state to the UI and the page.
async fn set_state(state: WifiState) {
    with(|i| i.wifi = state.clone());
    post(NetToMain::Ui(Event::Wifi(state))).await;
    ws_broadcast(wsmsg::status_event());
}

/// A host name mDNS and DHCP accept.
fn clean_hostname(s: &str) -> String {
    let mut out: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').map(|c| c.to_ascii_lowercase()).take(31).collect();
    if out.is_empty() {
        out.push_str("quire");
    }
    out
}

/// The task. Spawn once from `main`.
#[embassy_executor::task]
pub async fn net_task(args: NetTaskArgs) {
    let NetTaskArgs { mut wifi, fs, seed, mac } = args;
    let mut flags = Flags::default();
    let mut next: Option<Mode> = None;
    loop {
        let mode = match next.take() {
            Some(m) => m,
            None => idle_command(fs, &mut flags).await,
        };
        match session(&mut wifi, fs, seed, mac, mode, &mut flags).await {
            Outcome::Off => set_state(WifiState::Off).await,
            Outcome::Switch(m) => next = Some(m),
        }
    }
}

/// Commands that arrive while the radio is off; returns when one needs it.
async fn idle_command(fs: &'static dyn CardFs, flags: &mut Flags) -> Mode {
    loop {
        match next_command().await {
            NetCommand::WifiOn => return Mode::Station { target: None },
            NetCommand::Scan => {
                flags.scan_pending = true;
                return Mode::Station { target: None };
            }
            NetCommand::Join { ssid, password } => return Mode::Station { target: Some((ssid, password)) },
            NetCommand::Hotspot => return Mode::Hotspot,
            NetCommand::WifiOff => {}
            cmd => {
                let _ = common(cmd, fs, flags, false).await;
            }
        }
    }
}

/// Arms every mode handles the same way. `Some(outcome)` ends the session. `online`
/// says a station session (with its fetch worker) is running.
async fn common(cmd: NetCommand, fs: &'static dyn CardFs, flags: &mut Flags, online: bool) -> Option<Outcome> {
    match cmd {
        NetCommand::WifiOff => return Some(Outcome::Off),
        NetCommand::Forget(ssid) => {
            let mut cfg = load_config(fs);
            if cfg.forget(&ssid) {
                save_config(fs, &cfg);
            }
            post(NetToMain::SavedNetworks(cfg.names())).await;
        }
        NetCommand::Reading(b) => flags.reading = b,
        NetCommand::Typing(b) => ws_broadcast(wsmsg::typing_event(b)),
        cmd @ (NetCommand::Fetch(_) | NetCommand::Ota(_) | NetCommand::Calibre(_) | NetCommand::SyncNow) => {
            fetch::enqueue(cmd, online).await
        }
        NetCommand::WifiOn | NetCommand::Scan | NetCommand::Join { .. } | NetCommand::Hotspot => {}
    }
    None
}

/// One radio lifetime.
async fn session(wifi: &mut WIFI<'static>, fs: &'static dyn CardFs, seed: u64, mac: [u8; 6], mode: Mode, flags: &mut Flags) -> Outcome {
    let mut cfg = load_config(fs);
    if cfg.hotspot_password.is_empty() {
        cfg.hotspot_password = hotspot::password(seed ^ u64::from_le_bytes([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], 0, 0]));
        save_config(fs, &cfg);
    }
    with(|i| i.pin_set = !cfg.pin.is_empty());
    let hostname = clean_hostname(&Settings::load(&DynFs(fs)).hostname);
    let heap_before = esp_alloc::HEAP.free();
    log::info!("net: session {mode:?}, heap free {heap_before}");

    let ap_ssid = hotspot::ssid_from_mac(&mac);
    let initial = match &mode {
        Mode::Station { .. } => wifi::idle_station(),
        Mode::Hotspot => wifi::hotspot_config(&ap_ssid, &cfg.hotspot_password),
    };
    let (mut controller, interfaces) = match esp_radio::wifi::new(wifi.reborrow(), wifi::controller_config(initial)) {
        Ok(x) => x,
        Err(e) => {
            log::error!("wifi init: {e:?}");
            set_state(WifiState::Failed(String::from("the radio"))).await;
            return Outcome::Off;
        }
    };
    let ap_ip = Ipv4Addr::from(hotspot::AP_IP);
    let (driver, net_config) = match &mode {
        Mode::Station { .. } => {
            let mut d = DhcpConfig::default();
            d.hostname = hostname.as_str().try_into().ok();
            (interfaces.station, NetConfigV4::dhcpv4(d))
        }
        Mode::Hotspot => {
            let mut dns = heapless::Vec::new();
            let _ = dns.push(ap_ip);
            (
                interfaces.access_point,
                NetConfigV4::ipv4_static(StaticConfigV4 {
                    address: Ipv4Cidr::new(ap_ip, hotspot::AP_PREFIX),
                    gateway: Some(ap_ip),
                    dns_servers: dns,
                }),
            )
        }
    };
    // The stack's storage lives on the heap for the session only.
    let mut resources = Box::new(StackResources::<8>::new());
    let (stack, mut runner) = embassy_net::new(driver, net_config, &mut resources, seed);
    log::info!("net: stack up, heap free {}", esp_alloc::HEAP.free());
    crate::touch(now_ms());

    // Turn power saving off before serving. The station path sets it to Minimum
    // and the setting survives into the access point's session, where nothing
    // ever cleared it: the radio dozes while its own clients are waiting on it.
    //
    // Espressif's guide says modem sleep "works in station-only mode", which
    // reads as though the setting is simply inert for an access point, and it
    // does not describe any failure of this shape. CrossPoint, which serves the
    // same page from the same chip, disables it anyway, and says why:
    //
    //     Disable WiFi sleep to improve responsiveness and prevent 'unreachable'
    //     errors. This is critical for reliable web server operation on ESP32.
    //
    // That is the fault reported here — a client that joins, takes a DHCP lease
    // and then cannot reach the reader at all.
    wifi::power_save(&mut controller, false);

    let outcome = select(runner.run(), async {
        let http = http::serve(stack, fs, &hostname);
        match mode {
            Mode::Hotspot => {
                set_state(WifiState::Hotspot { ssid: ap_ssid.clone(), password: cfg.hotspot_password.clone(), ip: wifi::ap_ip_text() })
                    .await;
                let servers = join4(http, mdns::run(stack, &hostname, ap_ip), dhcp::run(stack, ap_ip), captive::run(stack, ap_ip));
                match select(servers, hotspot_commands(fs, &mut cfg, flags)).await {
                    Either::First(_) => Outcome::Off,
                    Either::Second(o) => o,
                }
            }
            Mode::Station { target } => {
                let servers = join5(
                    http,
                    station_mdns(stack, &hostname),
                    fetch::worker(stack, fs),
                    calibre::run(stack, fs, &hostname),
                    super::sntp::sync_once(stack),
                );
                match select(servers, station_commands(&mut controller, stack, fs, &mut cfg, &hostname, target, flags)).await {
                    Either::First(_) => Outcome::Off,
                    Either::Second(o) => o,
                }
            }
        }
    })
    .await;
    let outcome = match outcome {
        Either::First(never) => never,
        Either::Second(o) => o,
    };
    with(|i| {
        i.mirror = None;
        i.mirror_clients = 0;
        i.calibre_status.clear();
        i.cancel = None;
    });
    drop(controller);
    drop(resources);
    log::info!("net: session over, heap free {} (was {heap_before})", esp_alloc::HEAP.free());
    outcome
}

/// mDNS follows the station's address.
async fn station_mdns(stack: Stack<'_>, hostname: &str) {
    loop {
        stack.wait_config_up().await;
        let Some(ip) = stack.config_v4().map(|c| c.address.address()) else { continue };
        select(mdns::run(stack, hostname, ip), stack.wait_config_down()).await;
        Timer::after(Duration::from_millis(200)).await;
    }
}

async fn do_scan(controller: &mut WifiController<'_>, cfg: &NetConfig) {
    let seen = wifi::scan(controller).await;
    post(NetToMain::Ui(Event::WifiScan(scan_list(cfg, &seen)))).await;
}

/// Join a target (or the best saved network); publishes every state along the way.
async fn try_join(
    controller: &mut WifiController<'_>,
    stack: Stack<'_>,
    fs: &dyn CardFs,
    cfg: &mut NetConfig,
    hostname: &str,
    target: Option<(String, String)>,
    current: &mut Option<String>,
) {
    let explicit = target.as_ref().is_some_and(|t| !t.1.is_empty());
    let Some((ssid, pw)) = wifi::choose_target(controller, cfg, target).await else {
        set_state(WifiState::Failed(String::from(if cfg.networks.is_empty() {
            "anything: no saved network"
        } else {
            "a saved network: none in range"
        })))
        .await;
        return;
    };
    set_state(WifiState::Connecting(ssid.clone())).await;
    match wifi::join(controller, stack, &ssid, &pw, hostname).await {
        Ok(state) => {
            cfg.remember(&ssid, &pw, true);
            save_config(fs, cfg);
            post(NetToMain::SavedNetworks(cfg.names())).await;
            *current = Some(ssid);
            set_state(state).await;
        }
        Err(e) => {
            log::warn!("join {ssid}: {e}");
            if !explicit {
                cfg.mark(&ssid, false);
                save_config(fs, cfg);
            }
            set_state(WifiState::Failed(ssid)).await;
        }
    }
}

/// The station's command loop; returns when the session should end.
#[allow(clippy::too_many_arguments)]
async fn station_commands(
    controller: &mut WifiController<'_>,
    stack: Stack<'_>,
    fs: &'static dyn CardFs,
    cfg: &mut NetConfig,
    hostname: &str,
    target: Option<(String, String)>,
    flags: &mut Flags,
) -> Outcome {
    let mut current: Option<String> = None;
    wifi::power_save(controller, true);
    if flags.scan_pending {
        flags.scan_pending = false;
        do_scan(controller, cfg).await;
    }
    try_join(controller, stack, fs, cfg, hostname, target, &mut current).await;
    let mut ps_on = true;
    let mut last_bars = 0u8;
    let mut tick = 0u32;
    let mut retries = 0u8;
    loop {
        match select(next_command(), Timer::after(Duration::from_secs(1))).await {
            Either::First(cmd) => match cmd {
                NetCommand::Hotspot => return Outcome::Switch(Mode::Hotspot),
                NetCommand::WifiOn => {
                    if current.is_none() {
                        try_join(controller, stack, fs, cfg, hostname, None, &mut current).await;
                    }
                }
                NetCommand::Scan => do_scan(controller, cfg).await,
                NetCommand::Join { ssid, password } => {
                    if current.is_some() {
                        let _ = controller.disconnect_async().await;
                        current = None;
                    }
                    try_join(controller, stack, fs, cfg, hostname, Some((ssid, password)), &mut current).await;
                }
                NetCommand::Forget(ssid) => {
                    let leaving = current.as_deref() == Some(ssid.as_str());
                    let _ = common(NetCommand::Forget(ssid), fs, flags, true).await;
                    *cfg = load_config(fs);
                    if leaving {
                        let _ = controller.disconnect_async().await;
                        return Outcome::Off;
                    }
                }
                cmd => {
                    if let Some(o) = common(cmd, fs, flags, true).await {
                        return o;
                    }
                }
            },
            Either::Second(_) => {
                tick = tick.wrapping_add(1);
                let now = now_ms();
                let (activity, busy_until) = with(|i| (i.activity_ms, i.busy_until_ms));
                if flags.reading && now.wrapping_sub(activity) > IDLE_OFF_MS {
                    log::info!("net: idle, radio off");
                    return Outcome::Off;
                }
                let busy = busy_until.wrapping_sub(now) < 1 << 31 && busy_until != now;
                if busy == ps_on {
                    ps_on = !busy;
                    wifi::power_save(controller, ps_on);
                }
                if let Some(ssid) = current.clone() {
                    if !controller.is_connected() {
                        retries += 1;
                        if retries > 3 {
                            current = None;
                            retries = 0;
                            set_state(WifiState::Failed(ssid)).await;
                        } else {
                            log::info!("net: link lost, rejoining {ssid}");
                            let pw = cfg.get(&ssid).map(|n| n.password.clone()).unwrap_or_default();
                            set_state(WifiState::Connecting(ssid.clone())).await;
                            match wifi::join(controller, stack, &ssid, &pw, hostname).await {
                                Ok(state) => {
                                    retries = 0;
                                    set_state(state).await;
                                }
                                Err(e) => log::warn!("rejoin: {e}"),
                            }
                        }
                    } else if tick.is_multiple_of(10) {
                        let bars = wifi::signal(controller);
                        if bars != last_bars {
                            last_bars = bars;
                            let ip = stack.config_v4().map(|c| alloc::format!("{}", c.address.address())).unwrap_or_default();
                            set_state(WifiState::Connected { ssid, ip, host: String::from(hostname), signal: bars }).await;
                        }
                    }
                }
            }
        }
    }
}

/// The hotspot's command loop.
async fn hotspot_commands(fs: &'static dyn CardFs, cfg: &mut NetConfig, flags: &mut Flags) -> Outcome {
    loop {
        match select(next_command(), Timer::after(Duration::from_secs(1))).await {
            Either::First(cmd) => match cmd {
                NetCommand::WifiOn => return Outcome::Switch(Mode::Station { target: None }),
                NetCommand::Join { ssid, password } => return Outcome::Switch(Mode::Station { target: Some((ssid, password)) }),
                NetCommand::Hotspot => {}
                NetCommand::Scan => {
                    // The access point cannot scan; the UI keeps its saved list.
                    post(NetToMain::Ui(Event::WifiScan(alloc::vec::Vec::new()))).await;
                }
                cmd => {
                    let was_forget = matches!(cmd, NetCommand::Forget(_));
                    if let Some(o) = common(cmd, fs, flags, false).await {
                        return o;
                    }
                    if was_forget {
                        *cfg = load_config(fs);
                    }
                }
            },
            Either::Second(_) => {
                let now = now_ms();
                let activity = with(|i| i.activity_ms);
                if flags.reading && now.wrapping_sub(activity) > IDLE_OFF_MS {
                    return Outcome::Off;
                }
            }
        }
    }
}
