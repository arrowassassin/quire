//! 02 first run: language, time, "This is your reader", add books.

use alloc::boxed::Box;
use alloc::string::String;
use quire_gfx::{draw_text, Frame, Ink, Rect, TextStyle};

use crate::spine::{self, SpineModel};
use crate::text::{draw_label, line_h, wrap};
use crate::theme::*;
use crate::widgets::{self, rail, running_head, setting_row, RowState, SettingValue};
use crate::{Action, Ctx, Env, Key, KeyEvent, KeyKind, Refresh, Result_, Screen, SysRequest};

/// The first-run wizard.
pub struct FirstRun {
    page: usize,
    focus: usize,
}

impl FirstRun {
    /// New.
    pub fn new() -> Self {
        FirstRun { page: 0, focus: 0 }
    }
}

impl Default for FirstRun {
    fn default() -> Self {
        Self::new()
    }
}

const LANGS: [(&str, &str); 4] = [("en", "English"), ("de", "Deutsch"), ("fr", "Français"), ("es", "Español")];

impl<E: Env> Screen<E> for FirstRun {
    fn name(&self) -> &'static str {
        match self.page {
            0 => "02-firstrun-language",
            1 => "02-firstrun-time",
            2 => "02-firstrun-reader",
            _ => "02-firstrun-books",
        }
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let w = f.width() as i32;
        let fb = quire_fonts::ui::body();
        let fl = quire_fonts::ui::label();
        match self.page {
            0 => {
                running_head(f, "Welcome", Some("1 / 4"));
                let mut y = widgets::CONTENT_TOP;
                draw_text(f, fb, widgets::INSET, y + fb.ascent(), "Choose a language for the reader.", TextStyle::INK);
                y += line_h(fb) + 12;
                for (i, (code, name)) in LANGS.iter().enumerate() {
                    let v = if cx.settings.language == *code { "✓" } else { "" };
                    widgets::row(f, y, ROW_H, name, None, Some(v), if self.focus == i { RowState::Focused } else { RowState::Normal });
                    y += ROW_H;
                }
                rail(f, ["", "", "Choose", "Next"], None);
            }
            1 => {
                running_head(f, "Time", Some("2 / 4"));
                let mut y = widgets::CONTENT_TOP;
                for l in
                    wrap(fb, "Set the clock so your reading stats have days. Wi-Fi sets it automatically later.", w - 2 * widgets::INSET)
                {
                    draw_text(f, fb, widgets::INSET, y + fb.ascent(), &l, TextStyle::INK);
                    y += line_h(fb);
                }
                y += 16;
                let now = cx.env.now();
                let day = quire_library::time::day_of(now);
                setting_row(f, y, ROW_H, "Date", &SettingValue::Text(quire_library::time::fmt_date_year(day)), RowState::Normal);
                y += ROW_H;
                setting_row(
                    f,
                    y,
                    ROW_H,
                    "Time",
                    &SettingValue::Text(quire_library::time::fmt_clock(now, cx.settings.clock_24h)),
                    RowState::Focused,
                );
                rail(f, ["", "Back", "Set", "Next"], None);
            }
            2 => {
                running_head(f, "This is your reader", Some("3 / 4"));
                let spec = Rect::new(widgets::INSET, widgets::CONTENT_TOP, (w - 2 * widgets::INSET - 24) as u32, 220);
                super::typeset::draw_specimen(f, spec, &cx.settings.profile);
                let m = SpineModel { total: 412, chapters: alloc::vec![40, 90, 160, 250, 330], current: 122 };
                spine::draw(f, Rect::new(w - MARGIN + 6, widgets::CONTENT_TOP, SPINE_W as u32, 220), &m, Ink::Black);
                let mut y = spec.bottom() + 20;
                let lines = [
                    "The book is the home screen: wake and you are on your page.",
                    "Labels sit where the keys are. Back opens Home; Confirm opens the compass.",
                    "The strip at the right is the Spine: how much is under your thumb. Hold Right to skim it.",
                    "Progress is time, not percent.",
                    "Type on your phone whenever a keyboard appears.",
                    "Long-press Back anywhere for Jump: everything in one list.",
                ];
                for l in lines {
                    for ll in wrap(fl, l, w - 2 * widgets::INSET) {
                        draw_text(f, fl, widgets::INSET, y + fl.ascent(), &ll, TextStyle::INK);
                        y += line_h(fl);
                    }
                    y += 4;
                }
                rail(f, ["", "Back", "", "Next"], None);
            }
            _ => {
                running_head(f, "Add books", Some("4 / 4"));
                let mut y = widgets::CONTENT_TOP;
                for l in wrap(
                    fb,
                    "The card starts empty. Get free books from the Bookshop, or drop your own from your phone.",
                    w - 2 * widgets::INSET,
                ) {
                    draw_text(f, fb, widgets::INSET, y + fb.ascent(), &l, TextStyle::INK);
                    y += line_h(fb);
                }
                y += 16;
                draw_label(f, widgets::INSET, y + fl.ascent(), "Free in the Bookshop", false);
                y += line_h(fl) + 6;
                for (t, a, h) in super::bookshop::START_HERE {
                    widgets::row(f, y, ROW_H, t, Some(a), Some(h), RowState::Normal);
                    y += ROW_H;
                }
                y += 16;
                let url = super::drop::drop_url(cx);
                let code = crate::qr::Qr::encode(&url);
                let qr = code.as_ref().map(|q| q.size_px(4)).unwrap_or(0);
                if let Some(q) = &code {
                    q.draw(f, widgets::INSET, y, 4);
                }
                draw_text(f, fb, widgets::INSET + qr + 12, y + 30, &url.replace("http://", ""), TextStyle::INK);
                // Beside a QR of unknown width, so this wraps into whatever is left
                // rather than running off the edge.
                let hx = widgets::INSET + qr + 12;
                let mut hy = y + 30 + line_h(fb);
                for l in wrap(fl, "Drop: press Up, then scan and drag books onto the page.", w - hx - widgets::INSET) {
                    draw_text(f, fl, hx, hy, &l, TextStyle::INK);
                    hy += line_h(fl);
                }
                // "Start reading" does not fit a rail cell.
                rail(f, ["", "Back", "Bookshop", "Start"], None);
            }
        }
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind != KeyKind::Press {
            return Action::None;
        }
        match (self.page, ev.key) {
            (0, Key::Up) => {
                self.focus = (self.focus + LANGS.len() - 1) % LANGS.len();
                Action::Redraw
            }
            (0, Key::Down) => {
                self.focus = (self.focus + 1) % LANGS.len();
                Action::Redraw
            }
            (0, Key::Confirm) => {
                cx.settings.language = LANGS[self.focus].0.into();
                cx.settings.bookshop_language = LANGS[self.focus].0.into();
                self.page = 1;
                Action::Redraw
            }
            (1, Key::Confirm) => Action::Push(Box::new(TimePicker::new())),
            (3, Key::Back) | (2, Key::Back) | (1, Key::Back) | (2, Key::Left) | (1, Key::Left) => {
                self.page -= 1;
                Action::Redraw
            }
            (3, Key::Right) => self.finish(cx),
            // Slot 2 of the rail is Confirm, and it says Bookshop.
            (3, Key::Confirm) => {
                self.finish(cx);
                Action::Push(Box::new(super::bookshop::BookshopHome::new()))
            }
            (_, Key::Right) if self.page < 3 => {
                self.page += 1;
                Action::Redraw
            }
            (3, Key::Left) => {
                self.page = 2;
                Action::Redraw
            }
            (3, Key::Up) => {
                self.finish(cx);
                Action::Push(Box::new(super::drop::DropScreen::new()))
            }
            (3, Key::Down) => {
                self.finish(cx);
                Action::Push(Box::new(super::bookshop::BookshopHome::new()))
            }
            _ => Action::None,
        }
    }
    fn result(&mut self, _cx: &mut Ctx<E>, _r: Result_) -> Action<E> {
        Action::Redraw
    }
}

impl FirstRun {
    fn finish<E: Env>(&mut self, cx: &mut Ctx<E>) -> Action<E> {
        cx.settings.first_run_done = true;
        let _ = cx.settings.save(cx.env.fs());
        // Open the sample (or any) book and go to the reading page.
        let id = cx.lib.current.or_else(|| cx.lib.shelf().first().map(|b| b.id));
        match id {
            Some(id) => Action::Open(id),
            None => Action::ToReader,
        }
    }
}

/// A date and time picker with steppers.
pub struct TimePicker {
    fields: [i32; 5],
    focus: usize,
    init: bool,
}

impl TimePicker {
    /// New, at the current time.
    pub fn new() -> Self {
        TimePicker { fields: [2026, 1, 1, 12, 0], focus: 3, init: false }
    }
    fn ts(&self) -> u32 {
        let day = quire_library::time::from_civil(self.fields[0] as u16, self.fields[1] as u8, self.fields[2] as u8);
        day as u32 * quire_library::time::DAY + self.fields[3] as u32 * 3600 + self.fields[4] as u32 * 60
    }
}

impl Default for TimePicker {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for TimePicker {
    fn name(&self) -> &'static str {
        "02-time-picker"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        if !self.init {
            let now = cx.env.now();
            let day = quire_library::time::day_of(now);
            let (y, m, d) = quire_library::time::civil(day);
            self.fields = [
                y as i32,
                m as i32,
                d as i32,
                quire_library::time::hour_of(now) as i32,
                (quire_library::time::minute_of_day(now) % 60) as i32,
            ];
            self.init = true;
        }
        running_head(f, "Set the time", None);
        let names = ["Year", "Month", "Day", "Hour", "Minute"];
        let mut y = widgets::CONTENT_TOP;
        for (i, n) in names.iter().enumerate() {
            let v = match i {
                1 => String::from(quire_library::time::month_name(self.fields[1] as u8)),
                _ => alloc::format!("{:02}", self.fields[i]),
            };
            setting_row(f, y, ROW_H, n, &SettingValue::Stepper(v), if self.focus == i { RowState::Focused } else { RowState::Normal });
            y += ROW_H;
        }
        rail(f, ["", "Cancel", "Set", ""], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        match ev.key {
            Key::Back => Action::PopWith(Result_::Cancel),
            Key::Up => {
                self.focus = (self.focus + 4) % 5;
                Action::Redraw
            }
            Key::Down => {
                self.focus = (self.focus + 1) % 5;
                Action::Redraw
            }
            Key::Left | Key::Right => {
                let d = if ev.key == Key::Left { -1 } else { 1 };
                let (lo, hi) = match self.focus {
                    0 => (2024, 2099),
                    1 => (1, 12),
                    2 => (1, quire_library::time::days_in_month(self.fields[0] as u16, self.fields[1] as u8) as i32),
                    3 => (0, 23),
                    _ => (0, 59),
                };
                let v = self.fields[self.focus] + d;
                self.fields[self.focus] = if v < lo {
                    hi
                } else if v > hi {
                    lo
                } else {
                    v
                };
                Action::Redraw
            }
            Key::Confirm => {
                let ts = self.ts();
                cx.env.request(SysRequest::SetTime(ts));
                Action::PopWith(Result_::Choice(0))
            }
            Key::Power => Action::None,
        }
    }
}

/// Unused helper kept for symmetry with other screens.
pub fn boxed() -> Box<FirstRun> {
    Box::new(FirstRun::new())
}

/// A one-line "sample book" title used when nothing else is present.
pub const SAMPLE_TITLE: &str = "Quire · a short guide";

/// Placeholder to keep `String` imported for future copy.
#[allow(dead_code)]
fn _s() -> String {
    String::new()
}
