//! Nothing is drawn off the edge: on every tour screen, ink in the last columns of the
//! frame (outside every rail, side label and card) is a clipped label.

use quire_sim::{fixture_card, tour, Sim};

/// Rows with ink in the last two columns whose ink run ending at the edge is short and
/// whose row is not mostly ink — glyph-sized, so not a rule, a focused row's inversion or
/// a card frame. Rows in `skip` (the side-label ticks) are ignored.
fn clipped_rows(f: &quire_gfx::Frame, skip: &[(i32, i32)]) -> Vec<i32> {
    let w = f.width() as i32;
    let mut rows = Vec::new();
    for y in 0..f.height() as i32 {
        if skip.iter().any(|(a, b)| y >= *a && y < *b) {
            continue;
        }
        if !(w - 2..w).any(|x| f.get(x, y)) {
            continue;
        }
        let run = (0..w).rev().take_while(|x| f.get(*x, y)).count();
        let ink = (0..w).filter(|x| f.get(*x, y)).count();
        // A focused row is inverted: its white value text ends short of the edge.
        if run < 24 && ink < w as usize / 2 {
            rows.push(y);
        }
    }
    rows
}

#[test]
fn no_screen_draws_into_the_last_columns() {
    let card = fixture_card("overflow");
    let mut sim = Sim::boot(&card);
    let shots = tour(&mut sim);
    let w = quire_gfx::PANEL_W as i32;
    let h = quire_gfx::PANEL_H as i32;
    // The side labels' edge ticks sit at x = w-4..w-2 in the two side-key bands; the
    // frame's last two columns are never a legitimate place for ink on a page with margins.
    // Both side keys share one band now: they sit opposite each other rather than
    // stacked down one edge.
    let side_bands = [(quire_ui::widgets::SIDE_Y, quire_ui::widgets::SIDE_Y + quire_ui::widgets::SIDE_H)];
    let mut clipped = Vec::new();
    let mut edge_ink = Vec::new();
    for s in &shots {
        // Full-bleed screens (sleep, the inverted paper, the locked strip, game boards)
        // legitimately reach the edge; text pages and lists never do.
        // A dialog or working card screens the whole frame beneath it with the 50 % dots,
        // edges included, by design.
        let screened = matches!(s.stack.last().copied(), Some("11-dialog") | Some("12-working"));
        let full_bleed = s.name.starts_with("40-") || s.name.starts_with("41-") || s.name.contains("80-") || screened;
        if full_bleed {
            continue;
        }
        let right = clipped_rows(&s.frame, &side_bands);
        // The rail's three cell dividers run to the bottom row; anything else there is
        // text that overran the rail (a rule spanning the width is a card frame, not text).
        let cell = w / 4;
        let bottom: usize = (0..w).filter(|x| x % cell != 0 && (x + 1) % cell != 0 && s.frame.get(*x, h - 1)).count();
        if !right.is_empty() {
            edge_ink.push(format!("{}: glyph ink at the right edge on rows {right:?}", s.name));
        }
        if bottom > 0 && bottom < w as usize / 2 {
            clipped.push(format!("{}: {bottom} px on the bottom row", s.name));
        }
    }
    for l in edge_ink.iter().chain(clipped.iter()) {
        eprintln!("edge ink: {l}");
    }
    assert!(clipped.is_empty(), "text reaches the bottom row:\n{}", clipped.join("\n"));
    assert!(edge_ink.is_empty(), "ink in the last two columns:\n{}", edge_ink.join("\n"));
    let _ = std::fs::remove_dir_all(&card);
}
