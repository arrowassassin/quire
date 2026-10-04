//! Background events must not earn a full refresh of an unchanged screen.
//!
//! A full (GC) refresh blocks the whole chip for about 1.2 s on the X3's panel, the
//! radio included. A screen that answered every redraw with GC turned each download
//! progress tick into one — 284 back to back during a 0.7 MB Bookshop download, with
//! the network starved for 218 s.

use quire_sim::{fixture_card, Sim};
use quire_ui::{Action, Ctx, Event, Key, KeyEvent, Refresh, Screen};
use quire_gfx::Frame;

/// A screen that redraws on anything and asks for GC every time, as the Bookshop's
/// book page did.
struct AlwaysGc;
impl<E: quire_ui::Env> Screen<E> for AlwaysGc {
    fn name(&self) -> &'static str {
        "test-always-gc"
    }
    fn draw(&mut self, _cx: &mut Ctx<E>, _f: &mut Frame) -> Refresh {
        Refresh::Gc
    }
    fn key(&mut self, _cx: &mut Ctx<E>, _ev: KeyEvent) -> Action<E> {
        Action::Redraw
    }
    fn event(&mut self, _cx: &mut Ctx<E>, _ev: &Event) -> Action<E> {
        Action::Redraw
    }
}

#[test]
fn background_events_never_escalate_to_a_full_refresh() {
    let card = fixture_card("refresh-policy");
    let mut sim = Sim::boot(&card);
    sim.push(Box::new(AlwaysGc));
    for ev in [Event::Tick, Event::Timer, Event::BooksChanged] {
        let name = format!("{ev:?}");
        let r = sim.event(ev);
        assert_ne!(r, Refresh::Gc, "{name} on an unchanged screen asked for a full refresh");
        assert_eq!(r, Refresh::Du, "{name} should still redraw, as a partial refresh");
    }
}

#[test]
fn the_readers_own_presses_keep_what_the_screen_asks_for() {
    let card = fixture_card("refresh-policy-keys");
    let mut sim = Sim::boot(&card);
    sim.push(Box::new(AlwaysGc));
    // Down: a key the global grammar does not claim, so it reaches the screen.
    assert_eq!(sim.press(Key::Down), Refresh::Gc);
}
