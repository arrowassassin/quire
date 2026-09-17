//! On-device text entry: a QWERTY keyboard with a "Type on your phone" strip, and a
//! compact T9 variant for search. Pops with `Result_::Text`.

use alloc::boxed::Box;
use alloc::string::String;
use quire_gfx::{draw_text, Frame, Ink, Rect, TextStyle};

use crate::text::{centered_baseline, draw_centered, draw_label};
use crate::widgets::{self, running_head, text_field};
use crate::{Action, Ctx, Env, Event, Key, KeyEvent, KeyKind, Refresh, Result_, Screen};

const KEY_W: i32 = 48;
const KEY_H: i32 = 56;

const ROWS_LOWER: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const ROWS_UPPER: [&str; 3] = ["QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"];
const ROWS_SYM: [&str; 3] = ["1234567890", "@#$%&-+()", "*\"':;!?/"];
const T9: [&str; 12] = ["1.,?", "abc", "def", "ghi", "jkl", "mno", "pqrs", "tuv", "wxyz", "⇧", "0 ", "⌫"];

/// Keyboard layer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    Lower,
    Upper,
    Symbols,
}

/// The keyboard screen.
pub struct KeyboardScreen {
    title: String,
    text: String,
    hint: String,
    layer: Layer,
    /// Focus: (row, col); row 3 is the bottom action row.
    focus: (usize, usize),
    t9: bool,
    /// T9 multi-tap: (key index, taps, last millis).
    tap: Option<(usize, usize, u32)>,
    secret: bool,
}

impl KeyboardScreen {
    /// A keyboard for `title` with initial text.
    pub fn new(title: &str, initial: &str, hint: &str) -> Self {
        KeyboardScreen {
            title: title.into(),
            text: initial.into(),
            hint: hint.into(),
            layer: Layer::Lower,
            focus: (0, 0),
            t9: false,
            tap: None,
            secret: false,
        }
    }
    /// The compact T9 variant (search).
    pub fn t9(title: &str, initial: &str, hint: &str) -> Self {
        KeyboardScreen { t9: true, ..Self::new(title, initial, hint) }
    }
    /// Hide the text (passwords still show the last character).
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
    /// Boxed.
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }

    fn rows(&self) -> [&'static str; 3] {
        match self.layer {
            Layer::Lower => ROWS_LOWER,
            Layer::Upper => ROWS_UPPER,
            Layer::Symbols => ROWS_SYM,
        }
    }
    fn row_len(&self, row: usize) -> usize {
        if self.t9 {
            return if row < 4 { 3 } else { 4 };
        }
        match row {
            0..=2 => self.rows()[row].chars().count(),
            _ => 4,
        }
    }
    fn shown(&self) -> String {
        if self.secret && !self.text.is_empty() {
            let n = self.text.chars().count();
            let mut s: String = core::iter::repeat_n('•', n - 1).collect();
            s.push(self.text.chars().last().unwrap_or('•'));
            s
        } else {
            self.text.clone()
        }
    }
    fn activate<E: Env>(&mut self, cx: &mut Ctx<E>) -> Action<E> {
        let (r, c) = self.focus;
        if self.t9 {
            if r < 4 {
                let idx = r * 3 + c;
                let now = cx.env.millis();
                match idx {
                    9 => {
                        self.layer = if self.layer == Layer::Upper { Layer::Lower } else { Layer::Upper };
                        self.tap = None;
                    }
                    11 => {
                        self.text.pop();
                        self.tap = None;
                    }
                    _ => {
                        let set = T9[idx];
                        let chars: alloc::vec::Vec<char> = set.chars().collect();
                        match self.tap {
                            Some((k, taps, t)) if k == idx && now.wrapping_sub(t) < 900 => {
                                self.text.pop();
                                let ch = chars[(taps + 1) % chars.len()];
                                self.push_char(ch);
                                self.tap = Some((idx, taps + 1, now));
                            }
                            _ => {
                                self.push_char(chars[0]);
                                self.tap = Some((idx, 0, now));
                            }
                        }
                    }
                }
                return Action::Redraw;
            }
            return self.action_row(c);
        }
        if r < 3 {
            let ch = self.rows()[r].chars().nth(c).unwrap_or(' ');
            self.push_char(ch);
            if self.layer == Layer::Upper {
                self.layer = Layer::Lower;
            }
            return Action::Redraw;
        }
        self.action_row(c)
    }
    fn push_char(&mut self, ch: char) {
        let ch = if self.layer == Layer::Upper { ch.to_uppercase().next().unwrap_or(ch) } else { ch };
        if self.text.len() < 200 {
            self.text.push(ch);
        }
    }
    fn action_row<E: Env>(&mut self, c: usize) -> Action<E> {
        match c {
            0 => {
                self.layer = match self.layer {
                    Layer::Symbols => Layer::Lower,
                    _ => Layer::Symbols,
                };
                Action::Redraw
            }
            1 => {
                self.push_char(' ');
                Action::Redraw
            }
            2 => {
                self.text.pop();
                Action::Redraw
            }
            _ => Action::PopWith(Result_::Text(self.text.clone())),
        }
    }
}

impl<E: Env> Screen<E> for KeyboardScreen {
    fn name(&self) -> &'static str {
        "14-keyboard"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let w = f.width() as i32;
        running_head(f, &self.title, None);
        // Phone strip.
        let strip_y = widgets::CONTENT_TOP;
        let url = phone_url(cx);
        // Encode once, then draw (N2).
        let qr = crate::qr::Qr::encode(&url);
        let qr_size = qr.as_ref().map(|q| q.size_px(3)).unwrap_or(0);
        if let Some(q) = &qr {
            q.draw(f, w - INSET_X - qr_size, strip_y, 3);
        }
        draw_label(f, INSET_X, strip_y + 18, "Type on your phone", false);
        draw_text(f, quire_fonts::ui::mono(), INSET_X, strip_y + 44, &url, TextStyle::INK);
        let field_y = strip_y + qr_size.max(64) + 12;
        text_field(f, Rect::new(INSET_X, field_y, (w - 2 * INSET_X) as u32, 48), &self.shown(), &self.hint, true);
        // Keys.
        let top = field_y + 64;
        if self.t9 {
            let kw = 120;
            let kh = 64;
            let x0 = (w - 3 * kw - 2 * 8) / 2;
            for (i, t9) in T9.iter().enumerate() {
                let (r, c) = (i / 3, i % 3);
                let rect = Rect::new(x0 + c as i32 * (kw + 8), top + r as i32 * (kh + 8), kw as u32, kh as u32);
                let focused = self.focus == (r, c);
                f.stroke_rect(rect, 2, Ink::Black);
                if focused {
                    f.fill_rect(rect, Ink::Black);
                }
                let label = match i {
                    9 => {
                        if self.layer == Layer::Upper {
                            "abc"
                        } else {
                            "ABC"
                        }
                    }
                    10 => "0 ␣",
                    11 => "⌫",
                    _ => t9,
                };
                let font = quire_fonts::ui::list_title();
                let shown = if self.layer == Layer::Upper && i < 9 && i != 0 { label.to_uppercase() } else { String::from(label) };
                draw_centered(
                    f,
                    font,
                    rect.x + kw / 2,
                    centered_baseline(font, rect.y, kh),
                    &shown,
                    TextStyle { inverted: focused, ..TextStyle::INK },
                );
            }
            let ay = top + 4 * (kh + 8) + 8;
            let actions = ["?123", "Space", "Del", "✓"];
            let aw = (w - 2 * INSET_X - 3 * 8) / 4;
            for (c, a) in actions.iter().enumerate() {
                let rect = Rect::new(INSET_X + c as i32 * (aw + 8), ay, aw as u32, KEY_H as u32);
                let focused = self.focus == (4, c);
                f.stroke_rect(rect, 2, Ink::Black);
                if focused {
                    f.fill_rect(rect, Ink::Black);
                }
                // The check mark lives in the mono face only.
                let font = if *a == "✓" { quire_fonts::ui::mono() } else { quire_fonts::ui::body() };
                draw_centered(
                    f,
                    font,
                    rect.x + aw / 2,
                    centered_baseline(font, rect.y, KEY_H),
                    a,
                    TextStyle { inverted: focused, ..TextStyle::INK },
                );
            }
        } else {
            let rows = self.rows();
            for (r, row) in rows.iter().enumerate() {
                let n = row.chars().count() as i32;
                let x0 = (w - n * KEY_W) / 2;
                for (c, ch) in row.chars().enumerate() {
                    let rect = Rect::new(x0 + c as i32 * KEY_W, top + r as i32 * KEY_H, KEY_W as u32, KEY_H as u32);
                    let focused = self.focus == (r, c);
                    if focused {
                        f.fill_rect(rect, Ink::Black);
                    }
                    f.stroke_rect(rect, 1, Ink::Black);
                    let font = quire_fonts::ui::list_title();
                    let s: String = ch.into();
                    draw_centered(
                        f,
                        font,
                        rect.x + KEY_W / 2,
                        centered_baseline(font, rect.y, KEY_H),
                        &s,
                        TextStyle { inverted: focused, ..TextStyle::INK },
                    );
                }
            }
            let ay = top + 3 * KEY_H + 8;
            let actions = [if self.layer == Layer::Symbols { "abc" } else { "?123" }, "Space", "Del", "✓"];
            let widths = [96, 192, 96, 96];
            let total: i32 = widths.iter().sum::<i32>() + 3 * 8;
            let mut x = (w - total) / 2;
            for (c, a) in actions.iter().enumerate() {
                let rect = Rect::new(x, ay, widths[c] as u32, KEY_H as u32);
                let focused = self.focus == (3, c);
                if focused {
                    f.fill_rect(rect, Ink::Black);
                }
                f.stroke_rect(rect, 2, Ink::Black);
                let font = if *a == "✓" { quire_fonts::ui::mono() } else { quire_fonts::ui::body() };
                draw_centered(
                    f,
                    font,
                    rect.x + widths[c] / 2,
                    centered_baseline(font, rect.y, KEY_H),
                    a,
                    TextStyle { inverted: focused, ..TextStyle::INK },
                );
                x += widths[c] + 8;
            }
        }
        widgets::rail(f, ["Shift", "Back", "Type", "Done"], None);
        Refresh::Du
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        let rows = if self.t9 { 5 } else { 4 };
        match ev.key {
            Key::Back if ev.kind == KeyKind::Press => Action::PopWith(Result_::Cancel),
            Key::Right if ev.kind == KeyKind::Press => Action::PopWith(Result_::Text(self.text.clone())),
            Key::Left if ev.kind == KeyKind::Press => {
                self.layer = match self.layer {
                    Layer::Lower => Layer::Upper,
                    Layer::Upper => Layer::Lower,
                    Layer::Symbols => Layer::Lower,
                };
                Action::Redraw
            }
            Key::Confirm if ev.kind == KeyKind::Long => {
                self.text.pop();
                Action::Redraw
            }
            Key::Confirm => self.activate(cx),
            // The two side keys walk the keys in reading order rather than jumping a
            // whole row at a time. Moving sideways otherwise needs a long press of the
            // bottom keys, whose short press is already Shift and Done, so a reader
            // pressing the only keys that plainly move the cursor could go q, a, z and
            // no further along a row. Stepping one key at a time crosses the row ends
            // by itself, so both directions reach every key.
            Key::Up => {
                let (r, c) = self.focus;
                self.focus = if c > 0 {
                    (r, c - 1)
                } else {
                    let nr = if r == 0 { rows - 1 } else { r - 1 };
                    (nr, self.row_len(nr).max(1) - 1)
                };
                Action::Redraw
            }
            Key::Down => {
                let (r, c) = self.focus;
                self.focus = if c + 1 < self.row_len(r).max(1) {
                    (r, c + 1)
                } else {
                    ((r + 1) % rows, 0)
                };
                Action::Redraw
            }
            Key::Left | Key::Right => {
                // Long/repeat Left and Right move the focus along the row.
                let (r, c) = self.focus;
                let len = self.row_len(r).max(1);
                self.focus = (r, if ev.key == Key::Left { (c + len - 1) % len } else { (c + 1) % len });
                Action::Redraw
            }
            _ => Action::None,
        }
    }
    fn event(&mut self, cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if let Event::PhoneText(t) = ev {
            cx.phone_text.take();
            self.text = t.clone();
            return Action::Redraw;
        }
        Action::None
    }
}

const INSET_X: i32 = widgets::INSET;

/// The phone-typing URL for the current network state.
pub fn phone_url<E: Env>(cx: &Ctx<E>) -> String {
    match cx.env.wifi() {
        crate::WifiState::Connected { host, .. } => alloc::format!("http://{host}.local/type"),
        crate::WifiState::Hotspot { ip, .. } => alloc::format!("http://{ip}/type"),
        _ => alloc::format!("http://{}.local/type", cx.settings.hostname),
    }
}
