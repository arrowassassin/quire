//! The Bookshop's side keys walk the shelves in reading order: along each shelf, then
//! on to the next. They used to jump a whole shelf per press, so the keys that plainly
//! move the cursor could never move it along a shelf.

use quire_sim::{fixture_card, Sim};
use quire_ui::Key;

/// How many presses of `key` change the screen before it stops changing.
fn steps(sim: &mut Sim, key: Key, limit: usize) -> usize {
    let mut moves = 0;
    for _ in 0..limit {
        let before = sim.hash();
        sim.press(key);
        if sim.hash() == before {
            break;
        }
        moves += 1;
    }
    moves
}

#[test]
fn the_side_keys_walk_along_every_shelf_and_back() {
    let card = fixture_card("bookshop-nav");
    let mut sim = Sim::boot(&card);
    sim.open("35-bookshop");
    assert_eq!(sim.top(), "35-bookshop");

    let forward = steps(&mut sim, Key::Down, 200);
    // Along the shelves, not just down them: jumping a shelf per press took exactly
    // one press per shelf, and there are six.
    assert!(forward > 6, "the right side key only moved {forward} times — still jumping shelves?");
    // The walk stopped because it reached the end, not because it landed somewhere
    // nothing is drawn: one more press goes nowhere.
    let at_end = sim.hash();
    sim.press(Key::Down);
    assert_eq!(sim.hash(), at_end, "the walk stopped on a place the cursor cannot be seen");

    let back = steps(&mut sim, Key::Up, 200);
    assert_eq!(back, forward, "the left side key should retrace every step");
}

#[test]
fn the_bottom_keys_never_hide_the_cursor() {
    let card = fixture_card("bookshop-nav-bottom");
    let mut sim = Sim::boot(&card);
    sim.open("35-bookshop");
    sim.press(Key::Down); // onto the first shelf
    // Right as far as it goes along the shelf: every press that is accepted must show.
    let along = steps(&mut sim, Key::Right, 20);
    let at_end = sim.hash();
    sim.press(Key::Right);
    assert_eq!(sim.hash(), at_end, "Right walked onto an empty slot");
    assert_eq!(steps(&mut sim, Key::Left, 20), along, "Left should come back the same way");
}
