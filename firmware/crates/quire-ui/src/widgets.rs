//! The component library (brief §5): running head, rail, side labels, rows, tiles,
//! setting rows, dialog and working cards, empty states, covers, bars.

use alloc::string::String;
use alloc::vec::Vec;
use quire_gfx::{draw_text, measure_text, BitmapRef, BlitMode, Frame, Ink, Pattern, Rect, TextStyle};

use crate::icons::{self, Icon};
use crate::text::{centered_baseline, draw_centered, draw_label, draw_right, ellipsis, label_style, line_h, small_caps, wrap};
use crate::theme::*;
use crate::{Key, KeyEvent};

/// Content x inset used by titled screens.
pub const INSET: i32 = 32;

/// Y where content starts under a running head.
pub const CONTENT_TOP: i32 = 92;

/// Y of the running head's 4 px rule.
pub const HEAD_RULE_Y: i32 = 74;

/// Y of every empty state's serif line (one height on every screen).
pub const EMPTY_Y: i32 = CONTENT_TOP + 148;

/// Running head for a screen: serif title, optional right-hand mono text, 4 px rule.
/// The title sits on a baseline that keeps its descenders clear of the rule.
pub fn running_head(f: &mut Frame, title: &str, right: Option<&str>) {
    let font = quire_fonts::ui::title();
    let w = f.width() as i32;
    let base = HEAD_RULE_Y - 8 - font.below();
    let mut avail = w - 2 * INSET;
    if let Some(r) = right {
        let mf = quire_fonts::ui::mono();
        draw_right(f, mf, w - INSET, base, r, TextStyle::INK);
        avail -= measure_text(mf, r, TextStyle::INK) + 20;
    }
    draw_text(f, font, INSET, base, &ellipsis(font, title, avail), TextStyle::INK);
    f.fill_rect(Rect::new(INSET, HEAD_RULE_Y, (w - 2 * INSET) as u32, RULE_HEAVY), Ink::Black);
}

/// The reading page's running head: book title left, chapter right, 18 px small caps.
pub fn reading_head(f: &mut Frame, left: &str, right: &str, text_x: i32, text_right: i32, baseline: i32) {
    let font = quire_fonts::ui::label();
    let style = label_style(false);
    // Front matter often carries the book's own title as its heading: say it once.
    let right = if right.trim().eq_ignore_ascii_case(left.trim()) { "" } else { right };
    let r = small_caps(right);
    let rw = measure_text(font, &r, style);
    let avail = text_right - text_x - rw - 24;
    let l = ellipsis(font, &small_caps(left), avail.max(40));
    draw_text(f, font, text_x, baseline, &l, style);
    draw_text(f, font, text_right - rw, baseline, &r, style);
}

/// The edge-label rail at the bottom: four cells over the four keys.
pub fn rail(f: &mut Frame, labels: [&str; 4], focused: Option<usize>) {
    let w = f.width() as i32;
    let h = f.height() as i32;
    let y = h - RAIL_H;
    f.fill_rect(Rect::new(0, y, w as u32, RAIL_H as u32), Ink::White);
    f.fill_rect(Rect::new(0, y, w as u32, RULE_HEAVY), Ink::Black);
    let cell = w / 4;
    let font = quire_fonts::ui::mono();
    for (i, label) in labels.iter().enumerate() {
        let x = i as i32 * cell;
        let r = Rect::new(x, y + RULE_HEAVY as i32, cell as u32, (RAIL_H - RULE_HEAVY as i32) as u32);
        let inv = focused == Some(i);
        if inv {
            f.fill_rect(r, Ink::Black);
        }
        if i < 3 {
            f.fill_rect(Rect::new(x + cell - 1, y + RULE_HEAVY as i32, 1, (RAIL_H - RULE_HEAVY as i32) as u32), Ink::Black);
        }
        let cx = x + cell / 2;
        if label.is_empty() || *label == "—" {
            f.fill_rect(
                Rect::new(cx - 1, y + RULE_HEAVY as i32 + (RAIL_H - RULE_HEAVY as i32) / 2 - 1, 2, 2),
                if inv { Ink::White } else { Ink::Black },
            );
        } else {
            let text = ellipsis(font, label, cell - 8);
            draw_centered(
                f,
                font,
                cx,
                centered_baseline(font, y + RULE_HEAVY as i32, RAIL_H - RULE_HEAVY as i32),
                &text,
                TextStyle { inverted: inv, ..TextStyle::INK },
            );
        }
    }
}

/// Vertical positions of the two side labels (beside the Up and Down keys).
pub const SIDE_UP_Y: i32 = 500;
/// Down label top.
pub const SIDE_DOWN_Y: i32 = 592;
/// Side label box height.
pub const SIDE_H: i32 = 72;

/// A rotated edge label beside a side key: sentence-case mono (the rail's face) reading
/// upwards, no box, with a 2 px tick on the very edge marking the key. `y` is the key's
/// centre line. The compass and the power menu draw their side choices through this too,
/// so the same physical key is always labelled at the same height in the same face.
pub fn side_label(f: &mut Frame, y: i32, label: &str, inverted: bool) {
    // `y` identifies the key, and the key decides the edge: Up is left of the
    // screen, Down is right of it.
    let left = y < (SIDE_UP_Y + SIDE_H / 2 + SIDE_DOWN_Y + SIDE_H / 2) / 2;
    let (rot, r) = side_label_plate(f, y, label, left);
    f.fill_rect(r, if inverted { Ink::Black } else { Ink::White });
    paint_side_label(f, &rot, r, y, inverted, left);
}

/// The rotated text of a side label and the rect it occupies, centred on `y` and kept
/// between the running head and the rail.
fn side_label_plate(f: &Frame, y: i32, label: &str, left: bool) -> (String, Rect) {
    let font = quire_fonts::ui::mono();
    let text = ellipsis(font, label, SIDE_H + 28);
    let tw = measure_text(font, &text, TextStyle::INK) + 8;
    let th = line_h(font) + 2;
    let w = f.width() as i32;
    let h = f.height() as i32;
    // A label belongs against the key it names, and the two keys are on opposite
    // edges of the screen.
    let x = if left { 10 } else { w - 10 - th };
    let top = (y - tw / 2).clamp(CONTENT_TOP, h - RAIL_H - 6 - tw);
    (text, Rect::new(x, top, th as u32, tw as u32))
}

/// Draw a side label's text over its (already filled) plate and the key tick. Reading
/// bottom to top, like a spine: the glyphs are blitted transposed straight into the
/// frame, with the baseline on the column at `r.x + 1 + ascent` and the pen starting
/// 4 px in from the bottom of the plate.
fn paint_side_label(f: &mut Frame, text: &str, r: Rect, key_y: i32, inverted: bool, left: bool) {
    let font = quire_fonts::ui::mono();
    let w = f.width() as i32;
    let style = TextStyle { inverted, ..TextStyle::INK };
    quire_gfx::draw_text_ccw(f, font, r.x + 1 + font.ascent(), r.bottom() - 5, text, style);
    // Tick at the edge the key is on, centred on it.
    let tick_x = if left { 2 } else { w - 4 };
    f.fill_rect(Rect::new(tick_x, key_y - 12, 2, 24), Ink::Black);
}

/// Side labels beside Up and Down (only when the side keys act). `boxed` is kept for
/// callers but the labels are always drawn open; a focused side key inverts instead.
/// Two labels too long to both sit on their key centres are eased apart by the same
/// amount each, so neither runs into the other.
pub fn side_labels(f: &mut Frame, up: Option<&str>, down: Option<&str>, boxed: bool) {
    let _ = boxed;
    let (uy, dy) = (SIDE_UP_Y + SIDE_H / 2, SIDE_DOWN_Y + SIDE_H / 2);
    // The X3 carries one side key either side of the screen, so the labels never
    // share an edge and cannot run into one another.
    if let Some(u) = up {
        let (t, r) = side_label_plate(f, uy, u, true);
        f.fill_rect(r, Ink::White);
        paint_side_label(f, &t, r, uy, false, true);
    }
    if let Some(d) = down {
        let (t, r) = side_label_plate(f, dy, d, false);
        f.fill_rect(r, Ink::White);
        paint_side_label(f, &t, r, dy, false, false);
    }
}

/// State of a list row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// Plain.
    Normal,
    /// Inverted.
    Focused,
    /// 4 px left bar.
    Selected,
    /// Under a dot screen.
    Disabled,
}

/// A list row: title (26 px, or 22 px with a subtitle), optional subtitle, right value in mono.
pub fn row(f: &mut Frame, y: i32, h: i32, title: &str, subtitle: Option<&str>, value: Option<&str>, state: RowState) {
    let w = f.width() as i32;
    let r = Rect::new(0, y, w as u32, h as u32);
    let inv = state == RowState::Focused;
    f.fill_rect(r, if inv { Ink::Black } else { Ink::White });
    let style = TextStyle { inverted: inv, ..TextStyle::INK };
    let vfont = quire_fonts::ui::mono();
    let mut right = w - ROW_PAD;
    if let Some(v) = value {
        let vw = measure_text(vfont, v, style);
        draw_text(f, vfont, w - ROW_PAD - vw, centered_baseline(vfont, y, h), v, style);
        right = w - ROW_PAD - vw - 16;
    }
    let avail = right - ROW_PAD;
    match subtitle {
        None => {
            let font = quire_fonts::ui::list_title();
            draw_text(f, font, ROW_PAD, centered_baseline(font, y, h), &ellipsis(font, title, avail), style);
        }
        Some(sub) => {
            let font = quire_fonts::ui::body();
            let sfont = quire_fonts::ui::label();
            let total = font.ascent() + font.below() + 4 + sfont.ascent() + sfont.below();
            let top = y + (h - total) / 2;
            draw_text(f, font, ROW_PAD, top + font.ascent(), &ellipsis(font, title, avail), style);
            draw_text(f, sfont, ROW_PAD, top + font.ascent() + font.below() + 4 + sfont.ascent(), &ellipsis(sfont, sub, avail), style);
        }
    }
    if state == RowState::Selected {
        f.fill_rect(Rect::new(0, y, 4, h as u32), Ink::Black);
    }
    f.fill_rect(Rect::new(0, y + h - 1, w as u32, 1), Ink::Black);
    if state == RowState::Disabled {
        f.screen_rect(Rect::new(0, y, w as u32, (h - 1) as u32), DISABLED);
    }
}

/// A row with a thumbnail (88 px): thumb 48 × 72 at the left.
pub fn row_thumb(f: &mut Frame, y: i32, thumb: Option<BitmapRef<'_>>, title: &str, subtitle: &str, value: Option<&str>, state: RowState) {
    let w = f.width() as i32;
    let h = ROW_THUMB_H;
    let inv = state == RowState::Focused;
    f.fill_rect(Rect::new(0, y, w as u32, h as u32), if inv { Ink::Black } else { Ink::White });
    let tr = Rect::new(ROW_PAD, y + 8, 48, 72);
    match thumb {
        Some(bm) => {
            // Fit the thumbnail into 48 × 72 by nearest-neighbour sampling.
            for yy in 0..72 {
                for xx in 0..48 {
                    let sx = (xx as u32 * bm.w) / 48;
                    let sy = (yy as u32 * bm.h) / 72;
                    if bm.get(sx, sy) {
                        f.set(tr.x + xx, tr.y + yy, if inv { Ink::White } else { Ink::Black });
                    }
                }
            }
        }
        None => {
            f.pattern_rect(tr, Pattern::Hatch { pitch: 6 });
        }
    }
    f.stroke_rect(tr, 1, if inv { Ink::White } else { Ink::Black });
    let style = TextStyle { inverted: inv, ..TextStyle::INK };
    let vfont = quire_fonts::ui::label();
    let mut right = w - ROW_PAD;
    if let Some(v) = value {
        let vw = measure_text(vfont, v, style);
        draw_text(f, vfont, w - ROW_PAD - vw, centered_baseline(vfont, y, h), v, style);
        right = w - ROW_PAD - vw - 16;
    }
    let x = ROW_PAD + 48 + 16;
    let font = quire_fonts::ui::list_title();
    let sfont = quire_fonts::ui::label();
    let top = y + (h - (font.ascent() + font.below() + 6 + sfont.ascent() + sfont.below())) / 2;
    draw_text(f, font, x, top + font.ascent(), &ellipsis(font, title, right - x), style);
    draw_text(f, sfont, x, top + font.ascent() + font.below() + 6 + sfont.ascent(), &ellipsis(sfont, subtitle, right - x), style);
    if state == RowState::Selected {
        f.fill_rect(Rect::new(0, y, 4, h as u32), Ink::Black);
    }
    f.fill_rect(Rect::new(0, y + h - 1, w as u32, 1), Ink::Black);
    if state == RowState::Disabled {
        f.screen_rect(Rect::new(0, y, w as u32, (h - 1) as u32), DISABLED);
    }
}

/// A section label row (18 px small caps) with a hairline beneath.
pub fn section_label(f: &mut Frame, y: i32, text: &str) -> i32 {
    draw_label(f, ROW_PAD, y + 22, text, false);
    y + 32
}

/// Paged focus over `n` items with `per_page` rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListNav {
    /// Focused index.
    pub focus: usize,
    /// Items.
    pub n: usize,
    /// Rows per page.
    pub per_page: usize,
}

impl ListNav {
    /// New.
    pub fn new(n: usize, per_page: usize) -> Self {
        ListNav { focus: 0, n, per_page: per_page.max(1) }
    }
    /// Current page.
    pub fn page(&self) -> usize {
        self.focus / self.per_page
    }
    /// Pages.
    pub fn pages(&self) -> usize {
        self.n.div_ceil(self.per_page).max(1)
    }
    /// Indexes on the current page.
    pub fn visible(&self) -> core::ops::Range<usize> {
        let s = self.page() * self.per_page;
        s..(s + self.per_page).min(self.n)
    }
    /// Apply a key: Up/Down move focus, Left/Right page. Returns true when handled.
    pub fn key(&mut self, ev: KeyEvent) -> bool {
        if self.n == 0 {
            return matches!(ev.key, Key::Up | Key::Down | Key::Left | Key::Right);
        }
        match ev.key {
            Key::Up => self.focus = if self.focus == 0 { self.n - 1 } else { self.focus - 1 },
            Key::Down => self.focus = (self.focus + 1) % self.n,
            Key::Left => {
                if self.page() == 0 {
                    self.focus = (self.pages() - 1) * self.per_page;
                } else {
                    self.focus -= self.per_page;
                }
            }
            Key::Right => {
                if self.page() + 1 >= self.pages() {
                    self.focus = 0;
                } else {
                    self.focus = (self.focus + self.per_page).min(self.n - 1);
                }
            }
            _ => return false,
        }
        true
    }
    /// Clamp after the item count changed.
    pub fn set_n(&mut self, n: usize) {
        self.n = n;
        if n == 0 {
            self.focus = 0;
        } else if self.focus >= n {
            self.focus = n - 1;
        }
    }
}

/// Rows that fit between `top` and the rail.
pub fn rows_between(top: i32, bottom: i32, row_h: i32) -> usize {
    ((bottom - top) / row_h).max(1) as usize
}

/// A cover grid cell: 152 × 228 in a 2 px frame (6 px on focus) plus two lines beneath.
#[allow(clippy::too_many_arguments)]
pub fn cover_cell(
    f: &mut Frame,
    x: i32,
    y: i32,
    thumb: Option<BitmapRef<'_>>,
    title: &str,
    author: &str,
    focused: bool,
    percent: Option<u8>,
) {
    let r = Rect::new(x, y, COVER_W, COVER_H);
    match thumb {
        Some(bm) => {
            f.fill_rect(r, Ink::White);
            let ox = x + (COVER_W as i32 - bm.w as i32) / 2;
            let oy = y + (COVER_H as i32 - bm.h as i32) / 2;
            f.blit(ox, oy, bm, BlitMode::Or);
        }
        None => typographic_cover(f, r, title, author),
    }
    f.stroke_rect(r, if focused { 6 } else { 2 }, Ink::Black);
    let tfont = quire_fonts::ui::body();
    let afont = quire_fonts::ui::label();
    draw_text(f, tfont, x, y + COVER_H as i32 + 8 + tfont.ascent(), &ellipsis(tfont, title, COVER_W as i32), TextStyle::INK);
    let tag = match percent {
        Some(p) if p > 0 && p < 100 => alloc::format!("{p}%"),
        Some(100) => String::from("finished"),
        _ => String::new(),
    };
    let line2 = match (author.is_empty(), tag.is_empty()) {
        (false, false) => alloc::format!("{author} · {tag}"),
        (true, false) => tag,
        _ => String::from(author),
    };
    draw_text(
        f,
        afont,
        x,
        y + COVER_H as i32 + 8 + tfont.ascent() + tfont.below() + 4 + afont.ascent(),
        &ellipsis(afont, &line2, COVER_W as i32),
        TextStyle::INK,
    );
}

/// The typographic cover: a white title plate over a 6 px hatch. The plate scales with
/// the cell: body text in a full cover, the label face in a small one, and only as many
/// lines (and the author) as the cell has room for.
pub fn typographic_cover(f: &mut Frame, r: Rect, title: &str, author: &str) {
    f.fill_rect(r, Ink::White);
    f.pattern_rect(r, Pattern::Hatch { pitch: 6 });
    let afont = quire_fonts::ui::label();
    let small = r.w < 140;
    // A small cell keeps its plate close to the hatch edge so a nine-letter word still
    // fits on one line in the label face.
    let pad = if small { 4 } else { 10 };
    let plate_w = r.w as i32 - 2 * pad;
    let text_w = plate_w - 2 * pad;
    let mut tfont = if small { afont } else { quire_fonts::ui::body() };
    if title.split_whitespace().any(|w| measure_text(tfont, w, TextStyle::INK) > text_w) {
        tfont = afont;
    }
    // A word wider than the plate is shortened with an ellipsis rather than broken
    // mid-word, so the plate never shows a stray syllable on its own line.
    let title: String = title
        .split_whitespace()
        .map(|w| if measure_text(tfont, w, TextStyle::INK) > text_w { ellipsis(tfont, w, text_w) } else { String::from(w) })
        .collect::<Vec<_>>()
        .join(" ");
    let lh = line_h(tfont);
    let alh = line_h(afont);
    let top = r.y + (r.h as i32 / 6).min(40).max(pad);
    let room = r.bottom() - pad - top - 2 * pad;
    let mut lines = wrap(tfont, &title, text_w);
    let want_author = !author.is_empty();
    let author_h = if want_author { 6 + alh } else { 0 };
    // Lines that fit with the author; otherwise without it; never fewer than one.
    let mut with_author = want_author && (room - author_h) / lh >= 1;
    let max_lines = if with_author { (room - author_h) / lh } else { room / lh }.clamp(1, 4) as usize;
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let t = alloc::format!("{last}…");
            *last = ellipsis(tfont, &t, text_w);
        }
    }
    if with_author && lines.len() as i32 * lh + author_h > room {
        with_author = false;
    }
    let ph = 2 * pad + lines.len() as i32 * lh + if with_author { author_h } else { 0 };
    let plate = Rect::new(r.x + pad, top, plate_w as u32, ph as u32);
    f.fill_rect(plate, Ink::White);
    f.stroke_rect(plate, 1, Ink::Black);
    let cx = plate.x + plate.w as i32 / 2;
    let mut yy = plate.y + pad + tfont.ascent();
    for l in &lines {
        draw_centered(f, tfont, cx, yy, l, TextStyle::INK);
        yy += lh;
    }
    if with_author {
        let base = plate.y + pad + lines.len() as i32 * lh + 6 + afont.ascent();
        draw_centered(f, afont, cx, base, &ellipsis(afont, author, text_w), TextStyle::INK);
    }
}

/// Poster tiles: 44 px numeral over an 18 px small-cap label, 2-column grid with 2 px rules.
pub fn poster_tiles(f: &mut Frame, x: i32, y: i32, w: i32, tiles: &[(String, String)], cols: usize) -> i32 {
    let cols = cols.max(1);
    let cw = w / cols as i32;
    let nfont = quire_fonts::ui::poster();
    let lfont = quire_fonts::ui::label();
    let th = 24 + nfont.ascent() + nfont.below() + 4 + line_h(lfont) + 20;
    let rows = tiles.len().div_ceil(cols);
    for (i, (value, label)) in tiles.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        let tx = x + c as i32 * cw;
        let ty = y + r as i32 * th;
        // Column 0 sits on the margin like the title and rule above it; later columns
        // stand 20 px off their rule.
        let pad = if c == 0 { 0 } else { 20 };
        let nf = if measure_text(nfont, value, TextStyle::INK) > cw - pad - 12 { quire_fonts::ui::title() } else { nfont };
        draw_text(f, nf, tx + pad, ty + 24 + nfont.ascent(), value, TextStyle::INK);
        let label = ellipsis(lfont, &small_caps(label), cw - pad - 8);
        draw_label(f, tx + pad, ty + 24 + nfont.ascent() + nfont.below() + 4 + lfont.ascent(), &label, false);
        if c + 1 < cols && i + 1 < tiles.len() {
            f.fill_rect(Rect::new(tx + cw - 1, ty, RULE, th as u32), Ink::Black);
        }
        if r + 1 < rows {
            f.fill_rect(Rect::new(tx, ty + th - 1, cw as u32, RULE), Ink::Black);
        }
    }
    y + rows as i32 * th
}

/// A single poster numeral with its label, centred on `cx`.
pub fn poster_centered(f: &mut Frame, cx: i32, y: i32, value: &str, label: &str, hero: bool) -> i32 {
    let nfont = if hero { quire_fonts::ui::hero() } else { quire_fonts::ui::poster() };
    let lfont = quire_fonts::ui::label();
    draw_centered(f, nfont, cx, y + nfont.ascent(), value, TextStyle::INK);
    let ly = y + nfont.ascent() + nfont.below() + 6 + lfont.ascent();
    draw_centered(f, lfont, cx, ly, &small_caps(label), label_style(false));
    ly + lfont.below()
}

/// The value part of a setting row.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    /// A toggle.
    Toggle(bool),
    /// A stepper showing text ("26").
    Stepper(String),
    /// A choice showing text ("Justified ›").
    Choice(String),
    /// A slider: position 0–1000 and a value text.
    Slider(u16, String),
    /// Plain text (a status or a navigation row).
    Text(String),
    /// Navigation chevron only.
    Nav,
}

/// A setting row.
pub fn setting_row(f: &mut Frame, y: i32, h: i32, title: &str, value: &SettingValue, state: RowState) {
    let w = f.width() as i32;
    let inv = state == RowState::Focused;
    f.fill_rect(Rect::new(0, y, w as u32, h as u32), if inv { Ink::Black } else { Ink::White });
    let style = TextStyle { inverted: inv, ..TextStyle::INK };
    let font = quire_fonts::ui::body();
    let ink = if inv { Ink::White } else { Ink::Black };
    let paper = if inv { Ink::Black } else { Ink::White };
    let mut avail = w - 2 * ROW_PAD;
    match value {
        SettingValue::Toggle(on) => {
            let r = Rect::new(w - ROW_PAD - 56, y + (h - 28) / 2, 56, 28);
            f.stroke_rect(r, 2, ink);
            let kx = if *on { r.right() - 2 - 24 } else { r.x + 2 };
            f.fill_rect(Rect::new(kx, r.y + 2, 24, 24), ink);
            avail -= 72;
        }
        SettingValue::Stepper(v) => {
            let mf = quire_fonts::ui::mono();
            let t = alloc::format!("‹ {v} ›");
            let tw = measure_text(mf, &t, style);
            draw_text(f, mf, w - ROW_PAD - tw, centered_baseline(mf, y, h), &t, style);
            avail -= tw + 16;
        }
        SettingValue::Choice(v) => {
            let lf = quire_fonts::ui::label();
            let t = alloc::format!("{v} ›");
            let tw = measure_text(lf, &t, style);
            draw_text(f, lf, w - ROW_PAD - tw, centered_baseline(lf, y, h), &t, style);
            avail -= tw + 16;
        }
        SettingValue::Slider(pos, v) => {
            let lf = quire_fonts::ui::label();
            let tw = measure_text(lf, v, style);
            draw_text(f, lf, w - ROW_PAD - tw, centered_baseline(lf, y, h), v, style);
            let bar = Rect::new(w - ROW_PAD - tw - 14 - 180, y + h / 2 - 1, 180, 2);
            f.fill_rect(bar, ink);
            let kx = bar.x + ((*pos as i32).min(1000) * (180 - 16)) / 1000;
            f.fill_rect(Rect::new(kx, bar.y - 7, 16, 16), ink);
            avail -= tw + 14 + 180 + 16;
        }
        SettingValue::Text(v) => {
            let mf = quire_fonts::ui::mono();
            let t = ellipsis(mf, v, (w - 2 * ROW_PAD) / 2);
            let tw = measure_text(mf, &t, style);
            draw_text(f, mf, w - ROW_PAD - tw, centered_baseline(mf, y, h), &t, style);
            avail -= tw + 16;
        }
        SettingValue::Nav => {
            icons::draw(f, Icon::ChevronRight, w - ROW_PAD - 24, y + (h - 24) / 2, ink);
            avail -= 40;
        }
    }
    let _ = paper;
    draw_text(f, font, ROW_PAD, centered_baseline(font, y, h), &ellipsis(font, title, avail), style);
    if state == RowState::Selected {
        f.fill_rect(Rect::new(0, y, 4, h as u32), Ink::Black);
    }
    f.fill_rect(Rect::new(0, y + h - 1, w as u32, 1), Ink::Black);
    if state == RowState::Disabled {
        f.screen_rect(Rect::new(0, y, w as u32, (h - 1) as u32), DISABLED);
    }
}

/// A full-width dialog card with two actions on Back and Confirm. Returns the card rect.
pub fn dialog(f: &mut Frame, title: &str, body: &str, cancel: &str, confirm: &str) -> Rect {
    let w = f.width() as i32;
    let font_t = quire_fonts::ui::title();
    let font_b = quire_fonts::ui::body();
    let inner = w - 2 * MARGIN - 2 * 28;
    let lines = wrap(font_b, body, inner);
    let lh = line_h(font_b);
    let h = 28 + font_t.ascent() + font_t.below() + 12 + lines.len() as i32 * lh + 24 + RAIL_H + 2;
    let y = (f.height() as i32 - RAIL_H - h) / 2;
    let card = Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, h as u32);
    f.fill_rect(card, Ink::White);
    f.stroke_rect(card, 2, Ink::Black);
    draw_text(f, font_t, card.x + 28, card.y + 28 + font_t.ascent(), &ellipsis(font_t, title, inner), TextStyle::INK);
    let mut yy = card.y + 28 + font_t.ascent() + font_t.below() + 12;
    for l in &lines {
        draw_text(f, font_b, card.x + 28, yy + font_b.ascent(), l, TextStyle::INK);
        yy += lh;
    }
    // The card's own rail: Cancel on Back, the action on Confirm.
    let ry = card.bottom() - RAIL_H - 2;
    f.fill_rect(Rect::new(card.x + 2, ry, card.w - 4, RULE_HEAVY), Ink::Black);
    let cell = (card.w as i32 - 4) / 4;
    let mono = quire_fonts::ui::mono();
    for i in 0..4 {
        let x = card.x + 2 + i * cell;
        let label = match i {
            1 => cancel,
            2 => confirm,
            _ => "",
        };
        if i < 3 {
            f.fill_rect(Rect::new(x + cell - 1, ry + RULE_HEAVY as i32, 1, (RAIL_H - RULE_HEAVY as i32) as u32), Ink::Black);
        }
        let cx = x + cell / 2;
        if label.is_empty() {
            f.fill_rect(Rect::new(cx - 1, ry + RULE_HEAVY as i32 + (RAIL_H - RULE_HEAVY as i32) / 2 - 1, 2, 2), Ink::Black);
        } else {
            draw_centered(f, mono, cx, centered_baseline(mono, ry + RULE_HEAVY as i32, RAIL_H - RULE_HEAVY as i32), label, TextStyle::INK);
        }
    }
    card
}

/// A stepped progress bar: 10 px steps with 2 px gaps inside a 2 px frame, 16 px tall.
pub fn stepped_bar(f: &mut Frame, r: Rect, permille: u32) {
    f.fill_rect(r, Ink::White);
    f.stroke_rect(r, 2, Ink::Black);
    let inner = Rect::new(r.x + 4, r.y + 4, r.w.saturating_sub(8), r.h.saturating_sub(8));
    let fill_w = (inner.w as u64 * permille.min(1000) as u64 / 1000) as i32;
    let mut x = inner.x;
    while x < inner.x + fill_w {
        let step = 10.min(inner.x + fill_w - x);
        f.fill_rect(Rect::new(x, inner.y, step as u32, inner.h), Ink::Black);
        x += 12;
    }
}

/// A working card: title, subtitle, stepped bar, mono status.
pub fn working_card(f: &mut Frame, title: &str, subtitle: &str, permille: u32, status: &str) -> Rect {
    let w = f.width() as i32;
    let ft = quire_fonts::ui::title();
    let fb = quire_fonts::ui::body();
    let fl = quire_fonts::ui::label();
    let h = 28 + ft.ascent() + ft.below() + 8 + line_h(fb) + 20 + 16 + 10 + line_h(fl) + 28;
    let y = (f.height() as i32 - RAIL_H - h) / 2;
    let card = Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, h as u32);
    f.fill_rect(card, Ink::White);
    f.stroke_rect(card, 2, Ink::Black);
    let inner = card.w as i32 - 56;
    draw_text(f, ft, card.x + 28, card.y + 28 + ft.ascent(), &ellipsis(ft, title, inner), TextStyle::INK);
    let mut yy = card.y + 28 + ft.ascent() + ft.below() + 8;
    draw_text(f, fb, card.x + 28, yy + fb.ascent(), &ellipsis(fb, subtitle, inner), TextStyle::INK);
    yy += line_h(fb) + 20;
    stepped_bar(f, Rect::new(card.x + 28, yy, inner as u32, 16), permille);
    yy += 16 + 10;
    draw_text(f, quire_fonts::ui::mono(), card.x + 28, yy + fl.ascent(), &ellipsis(quire_fonts::ui::mono(), status, inner), TextStyle::INK);
    card
}

/// An empty state: a serif line, an 18 px hint (the rail shows the fixing action).
pub fn empty_state(f: &mut Frame, y: i32, line: &str, hint: &str) {
    let w = f.width() as i32;
    let ft = quire_fonts::ui::title();
    let fl = quire_fonts::ui::label();
    let cx = w / 2;
    let mut yy = y;
    for l in wrap(ft, line, w - 2 * INSET) {
        draw_centered(f, ft, cx, yy + ft.ascent(), &l, TextStyle::INK);
        yy += line_h(ft);
    }
    yy += 12;
    for l in wrap(fl, hint, w - 2 * INSET) {
        draw_centered(f, fl, cx, yy + fl.ascent(), &l, TextStyle::INK);
        yy += line_h(fl);
    }
}

/// Screen an area with the 50 % dot pattern (the page beneath an overlay).
pub fn screen(f: &mut Frame, r: Rect) {
    f.screen_rect(r, SCREENED);
}

/// A tab line: names with the active one inverted, page number at the right, 2 px rule
/// beneath. When the names do not fit, the strip scrolls so the active tab is visible
/// and a chevron marks the hidden side.
pub fn tabs(f: &mut Frame, y: i32, names: &[&str], active: usize, focused: bool, right: Option<&str>) -> i32 {
    let font = quire_fonts::ui::label();
    let w = f.width() as i32;
    let h = 32;
    let gap = 16;
    let mut limit = w - INSET;
    if let Some(r) = right {
        let rw = measure_text(quire_fonts::ui::mono(), r, TextStyle::INK);
        draw_right(f, quire_fonts::ui::mono(), w - INSET, centered_baseline(quire_fonts::ui::mono(), y, h), r, TextStyle::INK);
        limit -= rw + 16;
    }
    let labels: Vec<String> = names.iter().map(|n| small_caps(n)).collect();
    let widths: Vec<i32> = labels.iter().map(|t| measure_text(font, t, label_style(false))).collect();
    // Cells are 6 px wider than their text on each side: start so the first cell's left
    // edge sits on the INSET, and leave the chevron its own room when names overflow.
    let total: i32 = widths.iter().sum::<i32>() + gap * (names.len() as i32 - 1) + 12;
    if total > limit - INSET {
        limit -= 24;
    }
    // Scroll so the active tab is fully visible.
    let avail = limit - INSET - 12;
    let mut offset = 0;
    let active_end: i32 = widths.iter().take(active + 1).sum::<i32>() + gap * active as i32;
    if active_end > avail {
        offset = active_end - avail + 20;
    }
    let mut x = INSET + 6 - offset;
    let mut hidden_right = false;
    for (i, t) in labels.iter().enumerate() {
        let tw = widths[i];
        if x >= INSET - 6 && x + tw <= limit {
            let cell = Rect::new(x - 6, y, (tw + 12) as u32, h as u32);
            let inv = i == active;
            if inv {
                f.fill_rect(cell, Ink::Black);
                if focused {
                    f.stroke_rect(cell, 3, Ink::Black);
                }
            }
            draw_text(f, font, x, centered_baseline(font, y, h), t, label_style(inv));
        } else if x + tw > limit {
            hidden_right = true;
        }
        x += tw + gap;
    }
    let mf = quire_fonts::ui::mono();
    if offset > 0 {
        draw_text(f, mf, INSET - 22, centered_baseline(mf, y, h), "‹", TextStyle::INK);
    }
    if hidden_right {
        draw_right(f, mf, w - INSET, centered_baseline(mf, y, h), "›", TextStyle::INK);
    }
    f.fill_rect(Rect::new(INSET, y + h, (w - 2 * INSET) as u32, RULE), Ink::Black);
    y + h + RULE as i32
}

/// A battery and Wi-Fi status pair drawn at the top right (used by the Home layer and About).
pub fn status_icons(f: &mut Frame, x_right: i32, y: i32, battery: crate::Battery, wifi: bool) {
    let mut x = x_right - 24;
    icons::draw(f, Icon::Battery(icons::battery_level(battery.percent)), x, y, Ink::Black);
    if battery.charging {
        x -= 28;
        icons::draw(f, Icon::Charging, x, y, Ink::Black);
    }
    if wifi {
        x -= 28;
        icons::draw(f, Icon::Wifi, x, y, Ink::Black);
    }
}

/// A text field: 2 px rule beneath, caret, hint when empty.
pub fn text_field(f: &mut Frame, r: Rect, text: &str, hint: &str, focused: bool) {
    let font = quire_fonts::ui::list_title();
    f.fill_rect(r, Ink::White);
    let baseline = centered_baseline(font, r.y, r.h as i32);
    if text.is_empty() {
        // The caret stands before the hint, not on its first letter.
        let hx = r.x + 8 + if focused { 8 } else { 0 };
        draw_text(f, quire_fonts::ui::label(), hx, centered_baseline(quire_fonts::ui::label(), r.y, r.h as i32), hint, TextStyle::INK);
        if focused {
            f.fill_rect(Rect::new(r.x + 8, r.y + 8, 2, r.h.saturating_sub(16)), Ink::Black);
        }
    } else {
        let shown = tail_fit(font, text, r.w as i32 - 24);
        let end = draw_text(f, font, r.x + 8, baseline, &shown, TextStyle::INK);
        if focused {
            f.fill_rect(Rect::new(end + 2, r.y + 8, 2, r.h.saturating_sub(16)), Ink::Black);
        }
    }
    f.fill_rect(Rect::new(r.x, r.bottom() - 2, r.w, 2), Ink::Black);
}

/// The tail of `text` that fits `width` (for text fields).
pub fn tail_fit(font: &quire_gfx::Font, text: &str, width: i32) -> String {
    if measure_text(font, text, TextStyle::INK) <= width {
        return String::from(text);
    }
    let mut start = 0;
    let bytes = text.len();
    while start < bytes {
        start += 1;
        while start < bytes && !text.is_char_boundary(start) {
            start += 1;
        }
        if measure_text(font, &text[start..], TextStyle::INK) <= width {
            break;
        }
    }
    String::from(&text[start.min(bytes)..])
}

/// A 1-bit bar chart ("ink line"): solid bars, 2 px gaps, 2 px baseline, tallest labelled,
/// axis labels at first, middle and last.
pub fn ink_line(f: &mut Frame, r: Rect, values: &[u32], labels: (&str, &str, &str), unit: &str, hatched: Option<&[u32]>) {
    let n = values.len().max(1) as i32;
    let font = quire_fonts::ui::label();
    let mono = quire_fonts::ui::mono();
    let axis_h = line_h(mono) + 6;
    let label_h = line_h(font) + 4;
    let chart = Rect::new(r.x, r.y + label_h, r.w, r.h.saturating_sub((axis_h + label_h) as u32));
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let gap = 2;
    let bw = ((chart.w as i32 - gap * (n - 1)) / n).max(4);
    let base = chart.bottom() - 2;
    f.fill_rect(Rect::new(chart.x, base, chart.w, 2), Ink::Black);
    let mut tallest = (0usize, 0u32);
    for (i, v) in values.iter().enumerate() {
        let h = ((*v as u64 * (chart.h as u64 - 2)) / max as u64) as i32;
        let x = chart.x + i as i32 * (bw + gap);
        if h > 0 {
            f.fill_rect(Rect::new(x, base - h, bw as u32, h as u32), Ink::Black);
        }
        if let Some(hv) = hatched {
            if let Some(sv) = hv.get(i) {
                let sh = ((*sv as u64 * (chart.h as u64 - 2)) / max as u64) as i32;
                if sh > h {
                    f.pattern_rect(Rect::new(x, base - sh, bw as u32, (sh - h) as u32), Pattern::Hatch { pitch: 3 });
                }
            }
        }
        if *v > tallest.1 {
            tallest = (i, *v);
        }
    }
    if tallest.1 > 0 {
        let x = chart.x + tallest.0 as i32 * (bw + gap) + bw / 2;
        let t = alloc::format!("{}{unit}", tallest.1);
        let tw = measure_text(font, &t, TextStyle::INK);
        let lx = (x - tw / 2).clamp(r.x, r.right() - tw);
        draw_text(f, font, lx, r.y + font.ascent(), &t, TextStyle::INK);
    }
    let ay = base + 6 + mono.ascent();
    draw_text(f, mono, chart.x, ay, labels.0, TextStyle::INK);
    draw_centered(f, mono, chart.x + chart.w as i32 / 2, ay, labels.1, TextStyle::INK);
    draw_right(f, mono, chart.right(), ay, labels.2, TextStyle::INK);
}

/// A stepped line: one horizontal 2 px segment per sample, joined by verticals, over a
/// 2 px baseline (the brief's heap graph). The samples are scaled to the tallest.
pub fn step_line(f: &mut Frame, r: Rect, values: &[u32]) {
    let n = values.len();
    let base = r.bottom() - 2;
    f.fill_rect(Rect::new(r.x, base, r.w, 2), Ink::Black);
    if n == 0 {
        return;
    }
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let span = (r.h as i32 - 4).max(1);
    let seg = (r.w as i32 / n as i32).max(1);
    let mut prev_y: Option<i32> = None;
    for (i, v) in values.iter().enumerate() {
        let x = r.x + i as i32 * seg;
        let y = base - ((*v as u64 * span as u64) / max as u64) as i32;
        if let Some(py) = prev_y {
            let (top, h) = if py < y { (py, y - py) } else { (y, py - y) };
            if h > 0 {
                f.fill_rect(Rect::new(x, top, 2, h as u32 + 2), Ink::Black);
            }
        }
        let end = if i + 1 == n { r.right() } else { x + seg };
        f.fill_rect(Rect::new(x, y, (end - x).max(2) as u32, 2), Ink::Black);
        prev_y = Some(y);
    }
}

/// A calendar heat map cell fill for a goal fraction: 0 / 25 / 50 / 100 %.
pub fn heat_fill(f: &mut Frame, r: Rect, fraction: u8) {
    f.fill_rect(r, Ink::White);
    match fraction {
        0 => {}
        1..=37 => f.pattern_rect(r, Pattern::Sparse),
        38..=87 => f.pattern_rect(r, Pattern::Dots50),
        _ => f.fill_rect(r, Ink::Black),
    }
}
