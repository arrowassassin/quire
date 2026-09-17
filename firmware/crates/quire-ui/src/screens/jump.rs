//! 42 Jump: the whole device in one list, filtered by typing from the phone (or T9).
//! Also the empty home page (10-home-empty) when nothing is open.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use quire_gfx::{draw_text, Frame, Ink, Rect, TextStyle};

use crate::keyboard::KeyboardScreen;
use crate::text::{centered_baseline, draw_label, ellipsis, line_h};
use crate::theme::*;
use crate::widgets::{self, rail, row, ListNav, RowState};
use crate::{Action, Ctx, Env, Event, Key, KeyEvent, KeyKind, Refresh, Result_, Screen};

/// A Jump target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Open a book.
    Book(quire_library::BookId),
    /// A screen by name.
    Screen(&'static str),
}

/// A row in Jump.
#[derive(Clone, Debug)]
pub struct JumpRow {
    /// Title.
    pub title: String,
    /// Right-hand value.
    pub value: String,
    /// Where it goes.
    pub target: Target,
    /// Group: 0 recent, 1 everything, 2 apps.
    pub group: u8,
}

/// Everything Jump lists, in order: recent books, then features alphabetically.
pub fn all_rows<E: Env>(cx: &mut Ctx<E>) -> Vec<JumpRow> {
    let mut rows = Vec::new();
    let today = cx.today();
    for b in cx.lib.shelf().into_iter().take(4) {
        let v = if b.status == quire_library::Status::Finished {
            String::from("finished")
        } else {
            quire_library::time::fmt_duration(cx.stats.time_left_secs(b))
        };
        rows.push(JumpRow { title: b.title.clone(), value: v, target: Target::Book(b.id), group: 0 });
    }
    let (streak, _) = cx.stats.streaks(today);
    let mut features: Vec<JumpRow> = alloc::vec![
        JumpRow {
            title: "Library".into(),
            value: alloc::format!("{} books", cx.lib.books.iter().filter(|b| !b.missing).count()),
            target: Target::Screen("11-library"),
            group: 1
        },
        JumpRow { title: "Bookshop".into(), value: "free books".into(), target: Target::Screen("35-bookshop"), group: 1 },
        JumpRow { title: "Drop".into(), value: "from your phone".into(), target: Target::Screen("30-drop"), group: 1 },
        JumpRow { title: "Stats".into(), value: alloc::format!("{streak} day streak"), target: Target::Screen("60a-overview"), group: 1 },
        JumpRow { title: "Settings".into(), value: String::new(), target: Target::Screen("50-settings"), group: 1 },
        JumpRow { title: "Contents".into(), value: "this book".into(), target: Target::Screen("22-contents"), group: 1 },
        JumpRow { title: "Highlights".into(), value: String::new(), target: Target::Screen("28-highlights"), group: 1 },
        JumpRow { title: "Go to".into(), value: String::new(), target: Target::Screen("23-goto"), group: 1 },
        JumpRow {
            title: "Type".into(),
            value: alloc::format!("{} px", cx.settings.profile.size),
            target: Target::Screen("25-type"),
            group: 1
        },
        JumpRow { title: "Layout".into(), value: String::new(), target: Target::Screen("25-layout"), group: 1 },
        JumpRow { title: "Sleep screen".into(), value: cx.settings.sleep.name().into(), target: Target::Screen("44-picker"), group: 1 },
        JumpRow { title: "Wi-Fi".into(), value: wifi_value(cx), target: Target::Screen("31-wifi"), group: 1 },
        JumpRow { title: "Folders".into(), value: "SD card".into(), target: Target::Screen("13-folders"), group: 1 },
        JumpRow { title: "Catalogs".into(), value: "OPDS".into(), target: Target::Screen("32-opds"), group: 1 },
        JumpRow { title: "Calibre".into(), value: "wireless".into(), target: Target::Screen("33-calibre"), group: 1 },
        JumpRow { title: "Sync".into(), value: String::new(), target: Target::Screen("34-sync"), group: 1 },
        JumpRow { title: "Downloads".into(), value: String::new(), target: Target::Screen("39-downloads"), group: 1 },
        JumpRow { title: "Year in review".into(), value: String::new(), target: Target::Screen("61-yearinreview"), group: 1 },
        JumpRow {
            title: "Battery".into(),
            value: cx.env.battery().days_left.map(|d| alloc::format!("{d} days")).unwrap_or_else(|| "—".into()),
            target: Target::Screen("50-battery"),
            group: 1
        },
        JumpRow { title: "About".into(), value: cx.env.device().version.clone(), target: Target::Screen("50-about"), group: 1 },
        JumpRow { title: "Developer".into(), value: String::new(), target: Target::Screen("90-developer"), group: 1 },
    ];
    if !cx.settings.simple_mode {
        features.extend([
            JumpRow { title: "Apps".into(), value: String::new(), target: Target::Screen("70-apps"), group: 2 },
            JumpRow { title: "Games".into(), value: String::new(), target: Target::Screen("80-games"), group: 2 },
            JumpRow {
                title: "Clock".into(),
                value: quire_library::time::fmt_clock(cx.env.now(), cx.settings.clock_24h),
                target: Target::Screen("73-clock"),
                group: 2,
            },
            JumpRow { title: "Flashcards".into(), value: String::new(), target: Target::Screen("71-flashcards"), group: 2 },
            JumpRow { title: "News".into(), value: String::new(), target: Target::Screen("72-news"), group: 2 },
            JumpRow { title: "Weather".into(), value: weather_value(cx), target: Target::Screen("74-weather"), group: 2 },
            JumpRow { title: "Wikipedia".into(), value: String::new(), target: Target::Screen("77-wikipedia"), group: 2 },
            JumpRow { title: "Calculator".into(), value: String::new(), target: Target::Screen("78-calculator"), group: 2 },
            JumpRow { title: "Notes".into(), value: String::new(), target: Target::Screen("79-notes"), group: 2 },
            JumpRow { title: "Images".into(), value: String::new(), target: Target::Screen("75-images"), group: 2 },
            JumpRow { title: "Interactive fiction".into(), value: String::new(), target: Target::Screen("76-fiction"), group: 2 },
            JumpRow { title: "Sudoku".into(), value: String::new(), target: Target::Screen("80-sudoku"), group: 2 },
            JumpRow { title: "2048".into(), value: String::new(), target: Target::Screen("80-2048"), group: 2 },
            JumpRow { title: "Minesweeper".into(), value: String::new(), target: Target::Screen("80-minesweeper"), group: 2 },
            JumpRow { title: "Chess".into(), value: String::new(), target: Target::Screen("80-chess"), group: 2 },
            JumpRow { title: "Wordle".into(), value: String::new(), target: Target::Screen("80-wordle"), group: 2 },
        ]);
    }
    features.sort_by_key(|r| (r.group, r.title.to_lowercase()));
    rows.extend(features);
    rows
}

fn wifi_value<E: Env>(cx: &Ctx<E>) -> String {
    match cx.env.wifi() {
        crate::WifiState::Connected { ssid, .. } => ssid,
        crate::WifiState::Hotspot { .. } => String::from("hotspot"),
        crate::WifiState::Connecting(s) => alloc::format!("joining {s}"),
        _ => String::from("off"),
    }
}

fn weather_value<E: Env>(cx: &mut Ctx<E>) -> String {
    cx.env.net().weather().map(|(t, _, _)| alloc::format!("{t}°")).unwrap_or_default()
}

/// Open a Jump target.
pub fn open_target<E: Env>(cx: &mut Ctx<E>, t: &Target) -> Action<E> {
    match t {
        Target::Book(id) => Action::Open(*id),
        Target::Screen(name) => match *name {
            "11-library" => Action::Replace(Box::new(super::library::LibraryScreen::new())),
            "35-bookshop" => Action::Replace(Box::new(super::bookshop::BookshopHome::new())),
            "30-drop" => Action::Replace(Box::new(super::drop::DropScreen::new())),
            "60a-overview" => Action::Replace(Box::new(super::stats::Overview::new())),
            "61-yearinreview" => Action::Replace(Box::new(super::stats::YearInReview::new())),
            "50-settings" => Action::Replace(Box::new(super::settings::SettingsHome::new())),
            "50-battery" => Action::Replace(Box::new(super::settings::BatteryScreen::new())),
            "50-about" => Action::Replace(Box::new(super::settings::About::new())),
            "22-contents" if cx.reader.is_some() => Action::Replace(Box::new(super::contents::Contents::new())),
            "28-highlights" if cx.reader.is_some() => Action::Replace(super::highlights::boxed()),
            "23-goto" if cx.reader.is_some() => Action::Replace(Box::new(super::goto::GoTo::new())),
            "25-type" => Action::Replace(Box::new(super::typeset::TypeScreen::new())),
            "25-layout" => Action::Replace(Box::new(super::typeset::LayoutScreen::new())),
            "44-picker" => Action::Replace(Box::new(super::sleep::Picker::new())),
            "31-wifi" => Action::Replace(Box::new(super::wifi::WifiScreen::new())),
            "13-folders" => Action::Replace(Box::new(super::folders::Folders::new())),
            "32-opds" => Action::Replace(Box::new(super::bookshop::OpdsScreen::new())),
            "33-calibre" => Action::Replace(Box::new(super::bookshop::CalibreScreen::new())),
            "34-sync" => Action::Replace(Box::new(super::bookshop::SyncScreen::new())),
            "39-downloads" => Action::Replace(Box::new(super::bookshop::Downloads::new())),
            "90-developer" => Action::Replace(Box::new(super::settings::Developer::new())),
            "70-apps" => Action::Replace(Box::new(super::apps::AppsList::new())),
            "80-games" => Action::Replace(Box::new(super::games::GamesList::new())),
            other => match super::apps::open_app(other).or_else(|| super::games::open_game(other)) {
                Some(s) => Action::Replace(s),
                None => Action::Pop,
            },
        },
    }
}

/// The Jump screen.
pub struct Jump {
    query: String,
    nav: ListNav,
    rows: Vec<JumpRow>,
}

impl Jump {
    /// New.
    pub fn new() -> Self {
        Jump { query: String::new(), nav: ListNav::new(0, 1), rows: Vec::new() }
    }
    fn filtered(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        let mut hits: Vec<usize> =
            self.rows.iter().enumerate().filter(|(_, r)| q.is_empty() || r.title.to_lowercase().contains(&q)).map(|(i, _)| i).collect();
        if !q.is_empty() {
            // Word-prefix matches ("we" → Weather) come before matches inside a word (Minesweeper).
            hits.sort_by_key(|&i| (!self.rows[i].title.to_lowercase().split_whitespace().any(|w| w.starts_with(&q)), i));
        }
        hits
    }
}

impl Default for Jump {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for Jump {
    fn name(&self) -> &'static str {
        "42-jump"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        if self.rows.is_empty() {
            self.rows = all_rows(cx);
        }
        let w = f.width() as i32;
        // Search line.
        let fl = quire_fonts::ui::label();
        let ft = quire_fonts::ui::list_title();
        f.fill_rect(Rect::new(0, 64 - RULE as i32, w as u32, RULE), Ink::Black);
        if self.query.is_empty() {
            draw_text(f, ft, ROW_PAD, centered_baseline(ft, 0, 64), "", TextStyle::INK);
            f.fill_rect(Rect::new(ROW_PAD, 18, 2, 30), Ink::Black);
        } else {
            let end = draw_text(f, ft, ROW_PAD, centered_baseline(ft, 0, 64), &self.query, TextStyle::INK);
            f.fill_rect(Rect::new(end + 2, 18, 2, 30), Ink::Black);
        }
        let hint = "Type on your phone";
        let hw = quire_gfx::measure_text(fl, hint, TextStyle::INK);
        draw_text(f, fl, w - ROW_PAD - hw, centered_baseline(fl, 0, 64), hint, TextStyle::INK);
        let visible = self.filtered();
        let row_h = cx.settings.row_h();
        let top = 64;
        let per = widgets::rows_between(top, f.height() as i32 - RAIL_H, row_h);
        self.nav.per_page = per;
        self.nav.set_n(visible.len());
        let mut y = top;
        let mut last_group: Option<u8> = None;
        let mut shown = 0;
        for vi in self.nav.visible() {
            let r = &self.rows[visible[vi]];
            if last_group != Some(r.group) && self.query.is_empty() {
                let label = match r.group {
                    0 => "Recent",
                    1 => "Everything",
                    _ => "Apps and games",
                };
                if y + 32 + row_h > f.height() as i32 - RAIL_H {
                    break;
                }
                draw_label(f, ROW_PAD, y + 22, label, false);
                y += 32;
                last_group = Some(r.group);
            }
            if y + row_h > f.height() as i32 - RAIL_H {
                break;
            }
            row(f, y, row_h, &r.title, None, Some(&r.value), if vi == self.nav.focus { RowState::Focused } else { RowState::Normal });
            y += row_h;
            shown += 1;
        }
        let _ = shown;
        let pages = alloc::format!("{} / {}", self.nav.page() + 1, self.nav.pages());
        let _ = line_h(fl);
        rail(f, ["", "Close", "Open", &pages], None);
        Refresh::Du
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        if ev.is(Key::Back) {
            return Action::Pop;
        }
        if ev.is(Key::Confirm) {
            let visible = self.filtered();
            let Some(&i) = visible.get(self.nav.focus) else { return Action::Pop };
            let t = self.rows[i].target.clone();
            return open_target(cx, &t);
        }
        if ev.is_long(Key::Confirm) {
            return Action::Push(KeyboardScreen::t9("Jump", &self.query, "Search everything").boxed());
        }
        if self.nav.key(ev) {
            return Action::Redraw;
        }
        Action::None
    }
    fn event(&mut self, cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if let Event::PhoneText(t) = ev {
            cx.phone_text.take();
            self.query = t.clone();
            self.nav.focus = 0;
            return Action::Redraw;
        }
        Action::None
    }
    fn result(&mut self, _cx: &mut Ctx<E>, r: Result_) -> Action<E> {
        if let Result_::Text(t) = r {
            self.query = t;
            self.nav.focus = 0;
        }
        Action::Redraw
    }
}

/// The empty home page: nothing open yet, three Start-here picks and the Drop QR.
pub fn draw_empty_home<E: Env>(cx: &mut Ctx<E>, f: &mut Frame) {
    let w = f.width() as i32;
    let ft = quire_fonts::ui::title();
    let fb = quire_fonts::ui::body();
    let fl = quire_fonts::ui::label();
    let x = widgets::INSET;
    let cw = w - 2 * x;
    let fm = quire_fonts::ui::mono();
    let mut y = 40;
    draw_text(f, ft, x, y + ft.ascent(), "Nothing open yet", TextStyle::INK);
    y += ft.ascent() + ft.below() + 8;
    for l in crate::text::wrap(fb, "Three good places to start, free from the Bookshop.", cw) {
        draw_text(f, fb, x, y + fb.ascent(), &l, TextStyle::INK);
        y += line_h(fb);
    }
    y += 24;
    draw_label(f, x, y + fl.ascent(), "Free in the Bookshop", false);
    y += line_h(fl) + 8;
    // Title and author on the margin, the hours in mono against the rule's end.
    for (t, a, v) in super::bookshop::START_HERE.iter().take(3) {
        f.fill_rect(Rect::new(x, y, cw as u32, 1), Ink::Black);
        let vw = quire_gfx::measure_text(fm, v, TextStyle::INK);
        draw_text(f, fb, x, y + 12 + fb.ascent(), &ellipsis(fb, t, cw - vw - 24), TextStyle::INK);
        draw_text(f, fm, w - x - vw, y + 12 + fb.ascent(), v, TextStyle::INK);
        draw_text(f, fl, x, y + 12 + line_h(fb) + fl.ascent(), &ellipsis(fl, a, cw - vw - 24), TextStyle::INK);
        y += 12 + line_h(fb) + line_h(fl) + 12;
    }
    f.fill_rect(Rect::new(x, y, cw as u32, 1), Ink::Black);
    y += 28;
    draw_label(f, x, y + fl.ascent(), "Or drop your own", false);
    y += line_h(fl) + 8;
    // The address exists only while the reader is serving it. Drop turns the
    // radio on; until then there is no page for a QR code to point at.
    if matches!(cx.env.wifi(), crate::WifiState::Connected { .. } | crate::WifiState::Hotspot { .. }) {
        let url = super::drop::drop_url(cx);
        let code = crate::qr::Qr::encode(&url);
        let qr = code.as_ref().map(|q| q.size_px(4)).unwrap_or(0);
        if let Some(q) = &code {
            q.draw(f, x, y, 4);
        }
        draw_text(f, quire_fonts::ui::list_title(), x + qr + 12, y + 30, &url.replace("http://", ""), TextStyle::INK);
        draw_text(f, fl, x + qr + 12, y + 30 + line_h(fl) + 6, "Scan, then drag books onto the page.", TextStyle::INK);
    } else {
        for l in crate::text::wrap(fl, "Open Drop and the reader puts its own address here to scan.", cw) {
            draw_text(f, fl, x, y + fl.ascent(), &l, TextStyle::INK);
            y += line_h(fl);
        }
    }
    // Drop and Stats are the side keys, which the rail has no cell for; without
    // these the page offers Drop and names no way to reach it.
    widgets::side_labels(f, Some("Drop"), Some("Stats"), true);
    // Confirm opens the library, so that is what the cell says. It read "Get",
    // which belongs to a book you are looking at in the Bookshop.
    rail(f, ["Library", "Close", "Library", "Bookshop"], None);
}
