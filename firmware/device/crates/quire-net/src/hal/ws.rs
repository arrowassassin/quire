//! The `/ws` WebSocket: keys and typed text from the page in, progress events and mirror
//! frames out. Frames are sent at most twice a second and only to clients that asked.

use alloc::vec::Vec;

use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Timer};
use picoserve::futures::Either as PsEither;
use picoserve::io::{Read, Write};
use picoserve::response::ws::{Message, SocketRx, SocketTx, WebSocketCallback};
use quire_ui::Event;

use super::now_ms;
use crate::proto::wsmsg::{self, Incoming};
use crate::{try_post, with, NetToMain, WS_OUT};

/// Incoming text frames are small (a key, or a field's text).
const RX_BUF: usize = 640;
/// Mirror poll period.
const MIRROR_PERIOD: Duration = Duration::from_millis(500);

/// One client.
pub struct WsClient;

impl WebSocketCallback for WsClient {
    async fn run<R: Read, W: Write<Error = R::Error>>(self, mut rx: SocketRx<R>, mut tx: SocketTx<W>) -> Result<(), W::Error> {
        let mut sub = WS_OUT.subscriber().ok();
        let mut buf = [0u8; RX_BUF];
        let mut want_mirror = false;
        let mut seen_gen = u32::MAX;
        let mut frame: Vec<u8> = Vec::new();
        // Every way out of this loop has to reach the cleanup below, a failed send
        // included. A send fails as a matter of course — a phone locks, a tab goes to
        // the background, the Wi-Fi blinks — and returning early with `?` from one of
        // these skipped the decrement, so a mirror client was counted for the rest of
        // the session and the reader kept packing frames for nobody.
        let result = loop {
            let signal = async {
                let tick = Timer::after(MIRROR_PERIOD);
                match &mut sub {
                    Some(s) => match select(s.next_message_pure(), tick).await {
                        Either::First(msg) => Some(msg),
                        Either::Second(()) => None,
                    },
                    None => {
                        tick.await;
                        None
                    }
                }
            };
            match rx.next_message(&mut buf, signal).await {
                Ok(PsEither::First(Ok(Message::Text(text)))) => {
                    with(|i| i.activity_ms = now_ms());
                    match wsmsg::parse_incoming(text) {
                        Some(Incoming::Key(k)) => {
                            try_post(NetToMain::Ui(Event::PhoneKey(k)));
                        }
                        Some(Incoming::Text(t)) => {
                            try_post(NetToMain::Ui(Event::PhoneText(t)));
                        }
                        Some(Incoming::Mirror(on)) => {
                            if on != want_mirror {
                                want_mirror = on;
                                with(|i| {
                                    if on {
                                        i.mirror_clients = i.mirror_clients.saturating_add(1);
                                    } else {
                                        i.mirror_clients = i.mirror_clients.saturating_sub(1);
                                        if i.mirror_clients == 0 {
                                            i.mirror = None;
                                        }
                                    }
                                });
                                seen_gen = u32::MAX;
                            }
                        }
                        Some(Incoming::Ping) => {
                            if let Err(e) = tx.send_text(r#"{"pong":1}"#).await {
                                break Err(e);
                            }
                        }
                        None => {}
                    }
                }
                Ok(PsEither::First(Ok(Message::Ping(d)))) => {
                    if let Err(e) = tx.send_pong(d).await {
                        break Err(e);
                    }
                }
                Ok(PsEither::First(Ok(Message::Close(_)))) => break Ok(()),
                Ok(PsEither::First(Ok(Message::Binary(_) | Message::Pong(_)))) => {}
                Ok(PsEither::First(Err(e))) => {
                    log::debug!("ws: {e:?}");
                    break Ok(());
                }
                Ok(PsEither::Second(Some(text))) => {
                    if let Err(e) = tx.send_text(&text).await {
                        break Err(e);
                    }
                }
                Ok(PsEither::Second(None)) => {
                    if want_mirror {
                        // Copy the packed frame out under the lock, then send it.
                        let fresh = with(|i| {
                            if i.mirror_gen != seen_gen {
                                if let Some(m) = &i.mirror {
                                    frame.clear();
                                    frame.extend_from_slice(m);
                                    return Some(i.mirror_gen);
                                }
                            }
                            None
                        });
                        if let Some(g) = fresh {
                            seen_gen = g;
                            if let Err(e) = tx.send_binary(&frame).await {
                                break Err(e);
                            }
                        }
                    }
                }
                Err(e) => break Err(e),
            }
        };
        if want_mirror {
            with(|i| {
                i.mirror_clients = i.mirror_clients.saturating_sub(1);
                if i.mirror_clients == 0 {
                    i.mirror = None;
                }
            });
        }
        result
    }
}
