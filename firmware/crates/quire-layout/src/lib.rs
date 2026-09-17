//! Quire layout engine.
//!
//! Input: a QTX chapter stream and a typography [`Profile`]. Output: [`Page`]s of draw
//! items for the frame, plus a page index so "go to 43 %" and the Spine are cheap.
//! Positions are `(paragraph byte offset, word index, syllable part)`, so they survive a
//! change of font size: only the page boundaries move, never what the reader was reading.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub mod lines;
pub mod page;
pub mod para;
pub mod render;

pub use page::{build_index, chars_before, pos_at_chars, DrawItem, Page, Paginator, Pos};
pub use render::{render_page, ImageSource, NoImages};

use quire_fonts::{Family, Style};
use quire_gfx::Rect;
use serde::{Deserialize, Serialize};

/// Paragraph separation style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ParaStyle {
    /// First-line indent, no gap (book style).
    #[default]
    Indent,
    /// Half a line of space between paragraphs, no indent.
    Space,
}

/// Text alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Align {
    /// Justified with hyphenation (book default).
    #[default]
    Justify,
    /// Ragged right.
    Left,
}

/// Everything the text block obeys (brief §4 and screen 25).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Reading face.
    pub family: Family,
    /// Reading size in pixels (one of `quire_fonts::READING_SIZES`).
    pub size: u16,
    /// Line height in percent of the size: 130, 145 or 160.
    pub line_height_pct: u16,
    /// Outer margin in pixels, 16–48 in steps of 8.
    pub margin: u16,
    /// Alignment.
    pub align: Align,
    /// Hyphenate when justifying.
    pub hyphenate: bool,
    /// Hyphenation language.
    pub lang: Lang,
    /// Paragraph style.
    pub para_style: ParaStyle,
    /// Three-line drop cap on the first paragraph of a chapter.
    pub drop_caps: bool,
    /// Dilate glyphs by a pixel.
    pub darker: bool,
    /// Reserve the 12 px Spine strip inside the right margin.
    pub spine: bool,
    /// Reserve 36 px for a running head.
    pub running_head: bool,
}

/// Hyphenation languages compiled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Lang {
    /// English.
    #[default]
    English,
    /// German.
    German,
    /// French.
    French,
    /// Spanish.
    Spanish,
    /// Italian.
    Italian,
    /// Dutch.
    Dutch,
    /// Portuguese.
    Portuguese,
    /// Russian.
    Russian,
    /// No hyphenation patterns.
    None,
}

impl Lang {
    fn hypher(self) -> Option<hypher::Lang> {
        Some(match self {
            Lang::English => hypher::Lang::English,
            Lang::German => hypher::Lang::German,
            Lang::French => hypher::Lang::French,
            Lang::Spanish => hypher::Lang::Spanish,
            Lang::Italian => hypher::Lang::Italian,
            Lang::Dutch => hypher::Lang::Dutch,
            Lang::Portuguese => hypher::Lang::Portuguese,
            // No Cyrillic strikes ship, so Russian text falls back to the glyph box and
            // its 200 KB of patterns are left out of the image; it wraps without hyphens.
            Lang::Russian | Lang::None => return None,
        })
    }
    /// From a BCP-47 / ISO 639 prefix ("en", "de-DE").
    pub fn from_code(code: &str) -> Lang {
        let c = code.get(..2).unwrap_or("");
        match c {
            "en" => Lang::English,
            "de" => Lang::German,
            "fr" => Lang::French,
            "es" => Lang::Spanish,
            "it" => Lang::Italian,
            "nl" => Lang::Dutch,
            "pt" => Lang::Portuguese,
            "ru" => Lang::Russian,
            _ => Lang::None,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            family: Family::Literata,
            // 26 px reads small on the X3's panel; 28 is the next size with a
            // real baked strike, so it costs nothing to render.
            size: 28,
            line_height_pct: 145,
            margin: 32,
            align: Align::Justify,
            hyphenate: true,
            lang: Lang::English,
            para_style: ParaStyle::Indent,
            drop_caps: true,
            // E-ink gives up contrast to the paper: a one-pixel dilation is the
            // difference between grey text and black text on this panel.
            darker: true,
            spine: true,
            running_head: true,
        }
    }
}

/// Width of the Spine strip itself.
pub const SPINE_W: u32 = 12;
/// Clear space between the text block and the Spine, so a justified line never touches it.
pub const SPINE_GUTTER: u32 = 10;
/// Height reserved for the running head.
pub const RUNNING_HEAD_H: u32 = 36;

/// The page geometry derived from a profile and a frame size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Frame width.
    pub w: u32,
    /// Frame height.
    pub h: u32,
    /// The text block.
    pub text: Rect,
    /// Line height in pixels.
    pub line_h: i32,
}

impl Profile {
    /// Geometry for a frame of `w × h`.
    pub fn geometry(&self, w: u32, h: u32) -> Geometry {
        let m = self.margin as i32;
        let right_extra = if self.spine { (SPINE_W + SPINE_GUTTER) as i32 } else { 0 };
        let top_extra = if self.running_head { RUNNING_HEAD_H as i32 } else { 0 };
        let text =
            Rect::new(m, m + top_extra, (w as i32 - 2 * m - right_extra).max(64) as u32, (h as i32 - 2 * m - top_extra).max(64) as u32);
        Geometry { w, h, text, line_h: self.line_h() }
    }
    /// Line height in pixels.
    pub fn line_h(&self) -> i32 {
        ((self.size as u32 * self.line_height_pct as u32 + 50) / 100) as i32
    }
    /// The reading font for a style flag set.
    pub fn font(&self, flags: u8) -> &'static quire_gfx::Font {
        let bold = flags & quire_qtx::style::BOLD != 0;
        let italic = flags & quire_qtx::style::ITALIC != 0;
        if flags & quire_qtx::style::MONO != 0 {
            return quire_fonts::nearest(Family::Mono, Style::from_flags(bold, false), self.code_size());
        }
        if flags & (quire_qtx::style::SUP | quire_qtx::style::SUB) != 0 {
            return quire_fonts::nearest(self.family, Style::from_flags(bold, italic), self.small_size());
        }
        quire_fonts::nearest(self.family, Style::from_flags(bold, italic), self.size)
    }
    /// Monospace size: one reading step below the body size.
    pub fn code_size(&self) -> u16 {
        step_below(self.size)
    }
    /// Size for captions, superscripts: two steps below.
    pub fn small_size(&self) -> u16 {
        step_below(step_below(self.size))
    }
    /// Heading font: bold, one step above (level 1 two steps above).
    pub fn heading_font(&self, level: u8) -> &'static quire_gfx::Font {
        let size = if level <= 1 { step_above(step_above(self.size)) } else { step_above(self.size) };
        quire_fonts::nearest(self.family, Style::Bold, size)
    }
}

fn step_below(size: u16) -> u16 {
    let s = quire_fonts::READING_SIZES;
    let i = s.iter().position(|&x| x >= size).unwrap_or(s.len() - 1);
    s[i.saturating_sub(1)]
}
fn step_above(size: u16) -> u16 {
    let s = quire_fonts::READING_SIZES;
    let i = s.iter().position(|&x| x >= size).unwrap_or(s.len() - 1);
    s[(i + 1).min(s.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use page::{build_index, chars_before, pos_at_chars, DrawItem, Paginator, Pos};
    use quire_qtx::{ParaKind, Token, Writer};

    const FIXTURE: &str = include_str!("../fixtures/middlemarch.txt");

    fn chapter() -> Vec<u8> {
        let mut w = Writer::new();
        w.push(&Token::ChapterTitle { number: Some("1".to_string()), title: Some("Miss Brooke".to_string()) });
        for para in FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
            w.para(ParaKind::Body);
            w.text(para);
        }
        w.finish()
    }

    fn geom(p: &Profile) -> Geometry {
        p.geometry(quire_gfx::PANEL_W, quire_gfx::PANEL_H)
    }

    fn page_text(page: &page::Page) -> String {
        let mut s = String::new();
        for it in &page.items {
            if let DrawItem::Text { text, .. } = it {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(text);
            }
        }
        s
    }

    /// The Spine strip and its gutter are clear of text at every size and margin.
    #[test]
    fn text_never_touches_the_spine() {
        let qtx = chapter();
        for size in quire_fonts::READING_SIZES {
            for margin in [16u16, 32, 48] {
                let p = Profile { size, margin, ..Profile::default() };
                let g = geom(&p);
                let spine_left = quire_gfx::PANEL_W as i32 - margin as i32 - SPINE_W as i32;
                let pg = Paginator::new(&qtx, p, g);
                let mut pos = Pos::START;
                for _ in 0..4 {
                    let Some(page) = pg.page_from(pos) else { break };
                    for it in &page.items {
                        if let DrawItem::Text { x, font, text, .. } = it {
                            let w = quire_gfx::measure_text(font, text, quire_gfx::TextStyle::INK);
                            assert!(
                                x + w <= spine_left,
                                "size {size} margin {margin}: text reaches {} but the spine starts at {spine_left}",
                                x + w
                            );
                        }
                    }
                    match page.next {
                        Some(n) => pos = n,
                        None => break,
                    }
                }
            }
        }
    }

    /// A three-line drop cap occupies three lines and the text beside it clears it.
    #[test]
    fn drop_cap_spans_three_lines_without_collision() {
        let qtx = chapter();
        let p = Profile::default();
        let g = geom(&p);
        let page = Paginator::new(&qtx, p, g).page_from(Pos::START).unwrap();
        let (cap_x, cap_base, cap_font, ch) = page
            .items
            .iter()
            .find_map(|i| if let DrawItem::DropCap { x, y, font, ch } = i { Some((*x, *y, *font, *ch)) } else { None })
            .expect("a drop cap");
        let glyph = cap_font.glyph(ch).expect("cap glyph");
        let cap_right = cap_x + glyph.bitmap.w as i32;
        let cap_top = cap_base - glyph.top as i32;
        // Three lines tall, within a line's tolerance.
        assert!(
            (glyph.bitmap.h as i32 - 3 * g.line_h).abs() <= g.line_h / 2,
            "cap is {} px, three lines is {}",
            glyph.bitmap.h,
            3 * g.line_h
        );
        // The first three lines of text start to the right of the cap; later lines do not.
        let mut by_baseline: alloc::collections::BTreeMap<i32, i32> = Default::default();
        for it in &page.items {
            if let DrawItem::Text { x, y, .. } = it {
                let e = by_baseline.entry(*y).or_insert(i32::MAX);
                *e = (*e).min(*x);
            }
        }
        let beside: Vec<i32> = by_baseline.iter().filter(|(y, _)| **y >= cap_top && **y <= cap_base + 2).map(|(_, x)| *x).collect();
        assert!(beside.len() >= 3, "three lines should sit beside the cap, found {}", beside.len());
        for x in &beside {
            assert!(*x >= cap_right, "line at x {x} overlaps the cap ending at {cap_right}");
        }
    }

    #[test]
    fn geometry_reserves_spine_and_running_head() {
        let p = Profile::default();
        let g = geom(&p);
        assert_eq!(g.text.x, 32);
        assert_eq!(g.text.y, 32 + RUNNING_HEAD_H as i32);
        // 528 - 2*32 margins - 12 spine - 10 gutter
        assert_eq!(g.text.w, 528 - 64 - SPINE_W - SPINE_GUTTER);
        assert_eq!(g.line_h, 41); // 28 * 1.45, rounded
    }

    #[test]
    fn paginates_without_losing_or_repeating_text() {
        let qtx = chapter();
        let p = Profile::default();
        let g = geom(&p);
        let pg = Paginator::new(&qtx, p, g);
        let index = build_index(&qtx, p, g);
        assert!(index.len() >= 2, "fixture should need several pages, got {}", index.len());

        // Every page advances, and positions are strictly increasing.
        for w in index.windows(2) {
            assert!(w[1] > w[0], "index must advance: {:?} -> {:?}", w[0], w[1]);
        }

        // Concatenating the pages reproduces the source words in order, once each.
        let mut seen = String::new();
        for &start in &index {
            let page = pg.page_from(start).expect("page");
            seen.push(' ');
            seen.push_str(&page_text(&page));
        }
        let strip = |s: &str| -> Vec<String> {
            s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect()
        };
        let got = strip(&seen).join(" ").replace('-', "");
        for probe in ["miss brooke had that kind of beauty", "wadded with stupidity", "shade of coquetry in its arrangements"] {
            let flat = probe.replace(' ', "");
            let hay = got.replace(' ', "");
            assert_eq!(hay.matches(&flat).count(), 1, "{probe:?} should appear exactly once");
        }
    }

    #[test]
    fn lines_stay_inside_the_text_block() {
        let qtx = chapter();
        for size in quire_fonts::READING_SIZES {
            for lh in [130u16, 145, 160] {
                let p = Profile { size, line_height_pct: lh, ..Profile::default() };
                let g = geom(&p);
                let pg = Paginator::new(&qtx, p, g);
                let mut pos = Pos::START;
                for _ in 0..6 {
                    let Some(page) = pg.page_from(pos) else { break };
                    for it in &page.items {
                        match it {
                            DrawItem::Text { x, y, font, text, .. } => {
                                let w = quire_gfx::measure_text(font, text, quire_gfx::TextStyle::INK);
                                assert!(*x >= g.text.x - 2, "size {size}: x {x} left of block {}", g.text.x);
                                assert!(x + w <= g.text.right() + 4, "size {size} lh {lh}: line overflows: {} > {}", x + w, g.text.right());
                                assert!(*y <= g.text.bottom() + font.ascent(), "size {size}: baseline below block");
                            }
                            DrawItem::Image { rect, .. } => {
                                assert!(rect.right() <= g.text.right() + 1 && rect.bottom() <= g.text.bottom() + 1);
                            }
                            _ => {}
                        }
                    }
                    match page.next {
                        Some(n) => pos = n,
                        None => break,
                    }
                }
            }
        }
    }

    #[test]
    fn justification_flushes_both_margins() {
        let qtx = chapter();
        let p = Profile::default();
        let g = geom(&p);
        let pg = Paginator::new(&qtx, p, g);
        let page = pg.page_from(Pos::START).unwrap();
        // Group text items by baseline; a justified interior line should end near the right edge.
        let mut by_line: alloc::collections::BTreeMap<i32, (i32, i32)> = Default::default();
        for it in &page.items {
            if let DrawItem::Text { x, y, font, text, .. } = it {
                let w = quire_gfx::measure_text(font, text, quire_gfx::TextStyle::INK);
                let e = by_line.entry(*y).or_insert((*x, x + w));
                e.0 = e.0.min(*x);
                e.1 = e.1.max(x + w);
            }
        }
        let full: Vec<_> = by_line.values().filter(|(l, r)| r - l > g.text.w as i32 / 2).collect();
        assert!(full.len() >= 3, "expected several full lines, got {}", full.len());
        let flush = full.iter().filter(|(_, r)| (g.text.right() - *r).abs() <= 6).count();
        assert!(flush * 2 >= full.len(), "most full lines should reach the right margin: {flush}/{}", full.len());
    }

    #[test]
    fn ragged_right_does_not_justify() {
        let qtx = chapter();
        let p = Profile { align: Align::Left, hyphenate: false, ..Profile::default() };
        let g = geom(&p);
        let page = Paginator::new(&qtx, p, g).page_from(Pos::START).unwrap();
        let mut ends: Vec<i32> = Vec::new();
        let mut by_line: alloc::collections::BTreeMap<i32, i32> = Default::default();
        for it in &page.items {
            if let DrawItem::Text { x, y, font, text, .. } = it {
                let w = quire_gfx::measure_text(font, text, quire_gfx::TextStyle::INK);
                let e = by_line.entry(*y).or_insert(0);
                *e = (*e).max(x + w);
            }
        }
        ends.extend(by_line.values().copied());
        let exact = ends.iter().filter(|&&e| (g.text.right() - e).abs() <= 2).count();
        assert!(exact <= 1, "ragged right should not flush: {exact} of {}", ends.len());
    }

    #[test]
    fn hyphenation_only_when_enabled() {
        let mut w = Writer::new();
        w.para(ParaKind::Body);
        w.text("The extraordinary transformation of incomprehensible responsibilities demonstrates unquestionable administrative complications throughout.");
        let qtx = w.finish();
        let on = Profile { size: 34, ..Profile::default() };
        let off = Profile { hyphenate: false, ..on };
        let count = |p: Profile| -> usize {
            let g = geom(&p);
            let page = Paginator::new(&qtx, p, g).page_from(Pos::START).unwrap();
            page.items.iter().filter(|it| matches!(it, DrawItem::Text { text, .. } if text.ends_with('-'))).count()
        };
        assert!(count(on) >= 1, "expected hyphenation at 34 px");
        assert_eq!(count(off), 0, "no hyphens when disabled");
    }

    #[test]
    fn position_is_stable_across_typography_changes() {
        let qtx = chapter();
        let small = Profile { size: 22, ..Profile::default() };
        let large = Profile { size: 34, ..Profile::default() };
        // Read to page 3 at 22 px, then re-open the same position at 34 px.
        let idx = build_index(&qtx, small, geom(&small));
        let pos = idx[2.min(idx.len() - 1)];
        let chars = chars_before(&qtx, pos);
        let page_small = Paginator::new(&qtx, small, geom(&small)).page_from(pos).unwrap();
        let page_large = Paginator::new(&qtx, large, geom(&large)).page_from(pos).unwrap();
        let first_word = |p: &page::Page| page_text(p).split_whitespace().next().map(|s| s.to_string()).unwrap_or_default();
        assert_eq!(first_word(&page_small), first_word(&page_large), "the same word starts the page at both sizes");
        // And a character offset round-trips to a position on the same paragraph.
        let back = pos_at_chars(&qtx, chars);
        assert_eq!(back.para, pos.para, "character offset maps back to the same paragraph");
    }

    #[test]
    fn drop_cap_only_on_the_chapter_opening() {
        let qtx = chapter();
        let p = Profile::default();
        let g = geom(&p);
        let pg = Paginator::new(&qtx, p, g);
        let first = pg.page_from(Pos::START).unwrap();
        let caps = first.items.iter().filter(|i| matches!(i, DrawItem::DropCap { .. })).count();
        assert_eq!(caps, 1, "one drop cap on the opening page");
        if let Some(next) = first.next {
            let second = pg.page_from(next).unwrap();
            assert_eq!(second.items.iter().filter(|i| matches!(i, DrawItem::DropCap { .. })).count(), 0);
        }
        // Disabled by profile.
        let p2 = Profile { drop_caps: false, ..p };
        let none = Paginator::new(&qtx, p2, geom(&p2)).page_from(Pos::START).unwrap();
        assert_eq!(none.items.iter().filter(|i| matches!(i, DrawItem::DropCap { .. })).count(), 0);
    }

    #[test]
    fn verse_preserves_line_breaks_and_code_uses_mono() {
        let mut w = Writer::new();
        w.para(ParaKind::Verse);
        w.text("Because I could not stop for Death –");
        w.push(&Token::Break);
        w.text("He kindly stopped for me –");
        w.push(&Token::Break);
        w.text("The Carriage held but just Ourselves –");
        w.para(ParaKind::Code);
        w.text("let spine = Spine::new(1408);");
        let qtx = w.finish();
        let p = Profile::default();
        let g = geom(&p);
        let page = Paginator::new(&qtx, p, g).page_from(Pos::START).unwrap();
        let mut baselines: Vec<i32> =
            page.items.iter().filter_map(|i| if let DrawItem::Text { y, .. } = i { Some(*y) } else { None }).collect();
        baselines.sort_unstable();
        baselines.dedup();
        assert!(baselines.len() >= 4, "verse keeps its own lines: {}", baselines.len());
        let mono_used = page.items.iter().any(|i| matches!(i, DrawItem::Text { style, .. } if style & quire_qtx::style::MONO != 0));
        assert!(mono_used, "code block is set in mono");
    }

    #[test]
    fn empty_and_degenerate_input_do_not_hang() {
        let p = Profile::default();
        let g = geom(&p);
        assert!(Paginator::new(&[], p, g).page_from(Pos::START).is_none());
        assert!(build_index(&[], p, g).is_empty());

        // A single word wider than the line must still make progress.
        let mut w = Writer::new();
        w.para(ParaKind::Body);
        w.text("Llanfairpwllgwyngyllgogerychwyrndrobwllllantysiliogogogoch");
        let qtx = w.finish();
        let idx = build_index(&qtx, Profile { size: 34, ..p }, g);
        assert!(!idx.is_empty() && idx.len() < 10, "no runaway pagination: {}", idx.len());

        // A stream of empty paragraphs terminates.
        let mut w = Writer::new();
        for _ in 0..50 {
            w.para(ParaKind::Body);
        }
        let qtx = w.finish();
        let idx = build_index(&qtx, p, g);
        assert!(idx.len() <= 2, "empty paragraphs collapse: {}", idx.len());
    }

    #[test]
    fn every_reading_size_and_margin_produces_sane_measure() {
        let qtx = chapter();
        for size in quire_fonts::READING_SIZES {
            for margin in [16u16, 24, 32, 40, 48] {
                let p = Profile { size, margin, ..Profile::default() };
                let g = geom(&p);
                let page = Paginator::new(&qtx, p, g).page_from(Pos::START).unwrap();
                assert!(page.lines >= 3, "size {size} margin {margin}: only {} lines", page.lines);
                assert!(page.chars > 40, "size {size} margin {margin}: only {} chars", page.chars);
            }
        }
    }
}
