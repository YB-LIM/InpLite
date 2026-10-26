//! A virtualised code editor widget. Only the visible lines are highlighted, laid out and
//! painted, and edits touch only the changed bytes, so the cost of a frame or a keystroke does
//! not depend on the size of the text (egui's TextEdit lays out and copies the whole text).

use crate::syntax::{self, Diag, Palette, Sev};
use crate::tab::DocTab;
use crate::util;
use egui::text::CCursor;
use egui::{Align, Color32, CursorIcon, Event, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use std::sync::Arc;

pub const PAD_X: f32 = 6.0;

/// Screen geometry of the last frame (for hit testing, popups and the gutter).
#[derive(Clone)]
pub struct Geom {
    /// screen position of line 0, column 0
    pub origin: Pos2,
    pub row_h: f32,
    pub char_w: f32,
    /// first line that has a galley
    pub first: usize,
    pub galleys: Vec<Arc<egui::Galley>>,
    /// visible text area on screen
    pub view: Rect,
}

/// Per-tab cache of the highlighted visible window.
#[derive(Default)]
pub struct ViewCache {
    pub key: (u64, u32, u64, usize, usize),
    pub jobs: Vec<egui::text::LayoutJob>,
    /// live diagnostics of the highlighted window (current text version)
    pub diags: Vec<Diag>,
}

/// Middle-button autoscroll: press the middle button and move the mouse away from the press
/// point; the text scrolls faster the further away the pointer is. Holding and dragging scrolls
/// until the button is released; a short click keeps scrolling until the next click or Esc.
#[derive(Clone, Copy)]
pub struct AutoScroll {
    pub anchor: Pos2,
    /// the button has been released after a click: keep scrolling until the next click
    pub sticky: bool,
    /// the pointer moved away while the button was held (drag mode)
    pub moved: bool,
}

const AUTOSCROLL_DEAD: f32 = 10.0;

/// Rectangular selection: (line, display column) of the anchor and of the moving corner.
/// With equal columns it is a column of carets (typing inserts on every line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectSel {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl RectSel {
    /// (first line, last line, left column, right column)
    pub fn bounds(&self) -> (usize, usize, usize, usize) {
        (self.anchor.0.min(self.head.0), self.anchor.0.max(self.head.0), self.anchor.1.min(self.head.1), self.anchor.1.max(self.head.1))
    }
}

/// Byte offset of display column `col` in a line (clamped to the line end; inside a tab: before it).
fn off_at_col(line: &str, col: usize) -> usize {
    let mut c = 0;
    for (i, ch) in line.char_indices() {
        if c >= col {
            return i;
        }
        let w = if ch == '\t' { 4 } else { 1 };
        if c + w > col {
            return i;
        }
        c += w;
    }
    line.len()
}

/// Width in display columns.
fn width(t: &str) -> usize {
    t.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// Change every line of the rectangle with `f(line, left byte, right byte)` (placeholder lines of
/// data blocks are left alone); one undo step.
fn rect_edit(tab: &mut DocTab, f: &dyn Fn(usize, &str, usize, usize) -> Option<String>) -> bool {
    let Some(r) = tab.rect else { return false };
    let (l0, l1, c0, c1) = r.bounds();
    let l1 = l1.min(tab.map.starts.len().saturating_sub(1));
    let start = tab.map.starts[l0];
    let end = line_span(tab, l1).1;
    let mut out = String::with_capacity(end - start + 64);
    let mut changed = false;
    for l in l0..=l1 {
        let (ls, le) = line_span(tab, l);
        let lt = &tab.doc.text.s[ls..le];
        if l > l0 {
            out.push('\n');
        }
        if lt.starts_with(crate::store::PH_PREFIX) {
            out.push_str(lt);
            continue;
        }
        let (a, b) = (off_at_col(lt, c0), off_at_col(lt, c1));
        match f(l - l0, lt, a, b) {
            Some(n) => {
                changed |= n != lt;
                out.push_str(&n);
            }
            None => out.push_str(lt),
        }
    }
    if changed {
        tab.doc.text.replace_range(start..end, &out);
        tab.doc.text.break_group();
        tab.ensure_map();
    }
    changed
}

/// Text of the rectangle (one line per row).
fn rect_text(tab: &DocTab) -> String {
    let Some(r) = tab.rect else { return String::new() };
    let (l0, l1, c0, c1) = r.bounds();
    let mut v = Vec::new();
    for l in l0..=l1.min(tab.map.starts.len().saturating_sub(1)) {
        let (ls, le) = line_span(tab, l);
        let lt = &tab.doc.text.s[ls..le];
        v.push(lt[off_at_col(lt, c0)..off_at_col(lt, c1)].to_string());
    }
    v.join("\n")
}

/// Typing / deleting / pasting with a rectangular selection. Returns true if handled.
fn rect_key(tab: &mut DocTab, ev: &Event, ui: &egui::Ui) -> bool {
    let Some(r) = tab.rect else { return false };
    let (_, _, c0, c1) = r.bounds();
    let set_cols = |tab: &mut DocTab, c: usize| {
        if let Some(r) = tab.rect.as_mut() {
            r.anchor.1 = c;
            r.head.1 = c;
        }
    };
    match ev {
        Event::Text(t) => {
            let t: String = t.chars().filter(|c| !c.is_control() || *c == '\t').collect();
            if t.is_empty() {
                return true;
            }
            rect_edit(tab, &|_, lt, a, b| Some(format!("{}{}{}", &lt[..a], t, &lt[b..])));
            set_cols(tab, c0 + width(&t));
            true
        }
        Event::Paste(p) => {
            let p = p.replace("\r\n", "\n");
            let rows: Vec<&str> = p.trim_end_matches('\n').split('\n').collect();
            let (l0, l1, _, _) = r.bounds();
            let n = l1 - l0 + 1;
            if rows.len() == 1 || rows.len() == n {
                let w = rows.iter().map(|x| width(x)).max().unwrap_or(0);
                rect_edit(tab, &|i, lt, a, b| {
                    let ins = if rows.len() == 1 { rows[0] } else { rows[i] };
                    Some(format!("{}{}{}", &lt[..a], ins, &lt[b..]))
                });
                set_cols(tab, c0 + w);
            } else {
                rect_edit(tab, &|i, lt, a, b| rows.get(i).map(|ins| format!("{}{}{}", &lt[..a], ins, &lt[b..])));
                set_cols(tab, c0);
            }
            true
        }
        Event::Copy | Event::Cut => {
            ui.ctx().copy_text(rect_text(tab));
            if matches!(ev, Event::Cut) && c1 > c0 {
                rect_edit(tab, &|_, lt, a, b| Some(format!("{}{}", &lt[..a], &lt[b..])));
                set_cols(tab, c0);
            }
            true
        }
        Event::Key { key: Key::Backspace, pressed: true, .. } => {
            if c1 > c0 {
                rect_edit(tab, &|_, lt, a, b| Some(format!("{}{}", &lt[..a], &lt[b..])));
                set_cols(tab, c0);
            } else if c0 > 0 {
                rect_edit(tab, &|_, lt, a, _| {
                    if a == 0 || width(lt) < c0 {
                        return None;
                    }
                    let p = prev_boundary(lt, a);
                    Some(format!("{}{}", &lt[..p], &lt[a..]))
                });
                set_cols(tab, c0 - 1);
            }
            true
        }
        Event::Key { key: Key::Delete, pressed: true, .. } => {
            if c1 > c0 {
                rect_edit(tab, &|_, lt, a, b| Some(format!("{}{}", &lt[..a], &lt[b..])));
            } else {
                rect_edit(tab, &|_, lt, a, _| {
                    if a >= lt.len() {
                        return None;
                    }
                    Some(format!("{}{}", &lt[..a], &lt[next_boundary(lt, a)..]))
                });
            }
            set_cols(tab, c0);
            true
        }
        Event::Key { key: Key::Tab, pressed: true, modifiers, .. } if !modifiers.command => {
            rect_edit(tab, &|_, lt, a, b| Some(format!("{}\t{}", &lt[..a], &lt[b..])));
            set_cols(tab, c0 + 4);
            true
        }
        _ => false,
    }
}

/// Caret byte of the rectangle head (for the status bar and scrolling).
fn rect_head_byte(tab: &DocTab) -> usize {
    let Some(r) = tab.rect else { return tab.sel.1 };
    let l = r.head.0.min(tab.map.starts.len().saturating_sub(1));
    let (ls, le) = line_span(tab, l);
    ls + off_at_col(&tab.doc.text.s[ls..le], r.head.1)
}

pub struct ViewOut {
    pub response: egui::Response,
    /// the text was changed by typing this frame
    pub changed: bool,
}

// ---------------------------------------------------------------- text helpers

fn line_span(tab: &DocTab, l: usize) -> (usize, usize) {
    let st = &tab.map.starts;
    let s = st[l.min(st.len() - 1)];
    let e = st.get(l + 1).map(|x| x - 1).unwrap_or(tab.doc.text.s.len());
    (s, e.max(s))
}

fn prev_boundary(s: &str, i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    let mut j = i - 1;
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

fn next_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn char_before(s: &str, i: usize) -> Option<char> {
    s[..i].chars().next_back()
}

fn char_at(s: &str, i: usize) -> Option<char> {
    s[i..].chars().next()
}

/// Ctrl+Left target
fn word_left(s: &str, mut i: usize) -> usize {
    while let Some(c) = char_before(s, i) {
        if c == '\n' || is_word(c) {
            break;
        }
        i -= c.len_utf8();
    }
    if char_before(s, i) == Some('\n') && i > 0 {
        return i;
    }
    while let Some(c) = char_before(s, i) {
        if !is_word(c) {
            break;
        }
        i -= c.len_utf8();
    }
    i
}

/// Ctrl+Right target
fn word_right(s: &str, mut i: usize) -> usize {
    while let Some(c) = char_at(s, i) {
        if !is_word(c) {
            break;
        }
        i += c.len_utf8();
    }
    while let Some(c) = char_at(s, i) {
        if c == '\n' || is_word(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// Display column of byte offset `off` within a line (tabs count as 4, like egui's layout).
fn col_of(line: &str, off: usize) -> usize {
    line[..off.min(line.len())].chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// Byte offset within a line for a display column.
fn off_of_col(line: &str, col: usize) -> usize {
    let mut c = 0;
    for (i, ch) in line.char_indices() {
        let w = if ch == '\t' { 4 } else { 1 };
        if c + w > col {
            // closer to the left or right edge of this character?
            return if col - c >= w.div_ceil(2) && w > 1 { i + ch.len_utf8() } else { i };
        }
        c += w;
    }
    line.len()
}

impl Default for Geom {
    fn default() -> Self {
        Geom { origin: Pos2::ZERO, row_h: 0.0, char_w: 0.0, first: 0, galleys: Vec::new(), view: Rect::NOTHING }
    }
}

impl Geom {
    fn galley(&self, line: usize) -> Option<&Arc<egui::Galley>> {
        if line >= self.first {
            self.galleys.get(line - self.first)
        } else {
            None
        }
    }
    /// x offset (from the text origin) of byte `b`
    pub fn x_of(&self, tab: &DocTab, b: usize) -> f32 {
        let line = tab.map.line_of_byte(b);
        let (ls, le) = line_span(tab, line);
        let lt = &tab.doc.text.s[ls..le];
        let off = b.clamp(ls, le) - ls;
        match self.galley(line) {
            Some(g) => {
                let ci = lt[..util::floor_char_boundary(lt, off)].chars().count();
                g.pos_from_cursor(CCursor::new(ci)).min.x
            }
            None => col_of(lt, off) as f32 * self.char_w,
        }
    }
    /// top-left screen position of the caret at byte `b`
    pub fn pos_of(&self, tab: &DocTab, b: usize) -> Pos2 {
        let line = tab.map.line_of_byte(b);
        Pos2::new(self.origin.x + self.x_of(tab, b), self.origin.y + line as f32 * self.row_h)
    }
    pub fn line_at_y(&self, tab: &DocTab, y: f32) -> usize {
        let l = ((y - self.origin.y) / self.row_h.max(1.0)).floor();
        (l.max(0.0) as usize).min(tab.map.starts.len().saturating_sub(1))
    }
    /// byte offset under a screen position
    pub fn byte_at(&self, tab: &DocTab, p: Pos2) -> usize {
        if self.row_h <= 0.0 {
            return 0;
        }
        let line = self.line_at_y(tab, p.y);
        let (ls, le) = line_span(tab, line);
        let lt = &tab.doc.text.s[ls..le];
        let x = p.x - self.origin.x;
        match self.galley(line) {
            Some(g) => {
                let cc = g.cursor_from_pos(Vec2::new(x, g.size().y * 0.5));
                ls + lt.char_indices().nth(cc.index).map(|(i, _)| i).unwrap_or(lt.len())
            }
            None => ls + off_of_col(lt, (x / self.char_w.max(1.0)).round().max(0.0) as usize),
        }
    }
}

// ---------------------------------------------------------------- editing

fn ordered(sel: (usize, usize)) -> (usize, usize) {
    (sel.0.min(sel.1), sel.0.max(sel.1))
}

/// Placeholder lines of data blocks can be moved or deleted as whole lines, but their text
/// cannot be edited (the data stays on disk; editing the line would detach it).
fn edit_allowed(tab: &DocTab, a: usize, b: usize, with: &str) -> bool {
    let s = &tab.doc.text.s;
    let l0 = tab.map.line_of_byte(a.saturating_sub(1));
    let l1 = tab.map.line_of_byte(b);
    for l in l0..=l1 {
        let (ls, le) = line_span(tab, l);
        if !s[ls..le].starts_with(crate::store::PH_PREFIX) {
            continue;
        }
        let whole = (a <= ls && b > le) || (a < ls && b >= le && le == s.len());
        if whole || b < ls || a > le {
            continue;
        }
        if a == b && a == ls && (with.is_empty() || with.ends_with('\n')) {
            continue;
        }
        if a == b && a == le && (with.is_empty() || with.starts_with('\n')) {
            continue;
        }
        return false;
    }
    true
}

fn replace_sel(tab: &mut DocTab, with: &str, typing: bool, now: f64) {
    let (a, b) = ordered(tab.sel);
    if !edit_allowed(tab, a, b, with) {
        tab.notice = Some("Data block lines cannot be edited as text. Open the block in the data viewer (click), use Edit as text, or delete the whole line.".into());
        return;
    }
    if typing {
        tab.doc.text.type_range(a..b, with, now);
    } else {
        tab.doc.text.replace_range(a..b, with);
        tab.doc.text.break_group();
    }
    let p = a + with.len();
    tab.sel = (p, p);
}

/// Keep the selection on char boundaries inside the text (the text may have been changed
/// by an operation outside the editor).
pub fn clamp_sel(tab: &mut DocTab) {
    let s = &tab.doc.text.s;
    tab.sel.0 = util::floor_char_boundary(s, tab.sel.0.min(s.len()));
    tab.sel.1 = util::floor_char_boundary(s, tab.sel.1.min(s.len()));
}

/// Apply the keyboard / clipboard events of this frame. Returns (text changed, cursor moved).
fn handle_events(ui: &egui::Ui, tab: &mut DocTab, page_lines: usize) -> (bool, bool) {
    let events = ui.input(|i| i.events.clone());
    let now = ui.input(|i| i.time);
    let mut changed = false;
    let mut moved = false;
    for ev in events {
        let ver0 = tab.doc.text.ver;
        let sel0 = tab.sel;
        if tab.rect.is_some() {
            tab.ensure_map();
            let rect0 = tab.rect;
            if let Event::Key { key, pressed: true, modifiers: m, .. } = &ev {
                if m.alt && m.shift && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight) {
                    rect_move(tab, *key);
                    moved = true;
                    continue;
                }
                if matches!(key, Key::Escape) {
                    let b = rect_head_byte(tab);
                    tab.rect = None;
                    tab.sel = (b, b);
                    moved = true;
                    continue;
                }
            }
            if rect_key(tab, &ev, ui) {
                if tab.doc.text.ver != ver0 {
                    changed = true;
                    tab.ensure_map();
                }
                if tab.rect != rect0 {
                    moved = true;
                }
                let b = rect_head_byte(tab);
                tab.sel = (b, b);
                continue;
            }
            match &ev {
                // moving the cursor (and Ctrl+… commands) ends the rectangular selection; keys that
                // type text arrive as Text events and are handled above
                Event::Key { pressed: true, key, modifiers: m, .. }
                    if m.command || matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End | Key::PageUp | Key::PageDown | Key::Enter) =>
                {
                    tab.rect = None;
                }
                Event::Key { .. } => continue,
                _ => {}
            }
        } else if let Event::Key { key, pressed: true, modifiers: m, .. } = &ev {
            if m.alt && m.shift && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight) {
                tab.ensure_map();
                let head = tab.sel.1;
                let line = tab.map.line_of_byte(head);
                let (ls, le) = line_span(tab, line);
                let col = col_of(&tab.doc.text.s[ls..le], head - ls);
                tab.rect = Some(RectSel { anchor: (line, col), head: (line, col) });
                rect_move(tab, *key);
                moved = true;
                continue;
            }
        }
        match ev {
            Event::Text(t) => {
                let t: String = t.chars().filter(|c| !c.is_control() || *c == '\t').collect();
                if !t.is_empty() {
                    replace_sel(tab, &t, true, now);
                }
            }
            Event::Ime(egui::ImeEvent::Commit(t)) => {
                if !t.is_empty() {
                    replace_sel(tab, &t, true, now);
                }
            }
            Event::Paste(t) => {
                let t = t.replace("\r\n", "\n").replace('\r', "\n");
                replace_sel(tab, &t, false, now);
            }
            Event::Copy | Event::Cut => {
                let (a, b) = ordered(tab.sel);
                if a < b {
                    ui.ctx().copy_text(tab.doc.text.s[a..b].to_string());
                    if matches!(ev, Event::Cut) {
                        replace_sel(tab, "", false, now);
                    }
                }
            }
            Event::Key { key, pressed: true, modifiers: m, .. } => {
                tab.ensure_map();
                key_event(tab, key, m, page_lines, now);
            }
            _ => {}
        }
        if tab.doc.text.ver != ver0 {
            changed = true;
            tab.ensure_map();
        }
        if tab.sel != sel0 {
            moved = true;
        }
    }
    (changed, moved)
}

fn rect_move(tab: &mut DocTab, key: Key) {
    let n = tab.map.starts.len();
    if let Some(r) = tab.rect.as_mut() {
        match key {
            Key::ArrowUp => r.head.0 = r.head.0.saturating_sub(1),
            Key::ArrowDown => r.head.0 = (r.head.0 + 1).min(n.saturating_sub(1)),
            Key::ArrowLeft => r.head.1 = r.head.1.saturating_sub(1),
            Key::ArrowRight => r.head.1 += 1,
            _ => {}
        }
    }
    let b = rect_head_byte(tab);
    tab.sel = (b, b);
}

fn key_event(tab: &mut DocTab, key: Key, m: egui::Modifiers, page_lines: usize, now: f64) {
    let len = tab.doc.text.s.len();
    let (a, b) = ordered(tab.sel);
    let head = tab.sel.1;
    let cmd = m.command;
    // cursor movement: returns the new head
    let mut target: Option<usize> = None;
    let mut keep_col = false;
    {
        let s = &tab.doc.text.s;
        let line = tab.map.line_of_byte(head);
        let (ls, le) = line_span(tab, line);
        let n = tab.map.starts.len();
        let vertical = |tab: &DocTab, to_line: usize, col: usize| -> usize {
            let (s2, e2) = line_span(tab, to_line);
            s2 + off_of_col(&tab.doc.text.s[s2..e2], col)
        };
        let col_now = tab.pref_col.unwrap_or_else(|| col_of(&s[ls..le], head - ls));
        match key {
            Key::ArrowLeft => {
                target = Some(if a != b && !m.shift { a } else if cmd { word_left(s, head) } else { prev_boundary(s, head) });
            }
            Key::ArrowRight => {
                target = Some(if a != b && !m.shift { b } else if cmd { word_right(s, head) } else { next_boundary(s, head) });
            }
            Key::ArrowUp if !cmd => {
                keep_col = true;
                target = Some(if line == 0 { 0 } else { vertical(tab, line - 1, col_now) });
            }
            Key::ArrowDown if !cmd => {
                keep_col = true;
                target = Some(if line + 1 >= n { len } else { vertical(tab, line + 1, col_now) });
            }
            Key::PageUp => {
                keep_col = true;
                target = Some(vertical(tab, line.saturating_sub(page_lines), col_now));
            }
            Key::PageDown => {
                keep_col = true;
                target = Some(vertical(tab, (line + page_lines).min(n - 1), col_now));
            }
            Key::Home => {
                target = Some(if cmd {
                    0
                } else {
                    let lt = &s[ls..le];
                    let indent = ls + (lt.len() - lt.trim_start().len());
                    if head == indent { ls } else { indent }
                });
            }
            Key::End => target = Some(if cmd { len } else { le }),
            _ => {}
        }
    }
    if let Some(t) = target {
        tab.sel = if m.shift { (tab.sel.0, t) } else { (t, t) };
        if keep_col {
            if tab.pref_col.is_none() {
                let line = tab.map.line_of_byte(head);
                let (ls, le) = line_span(tab, line);
                tab.pref_col = Some(col_of(&tab.doc.text.s[ls..le], head - ls));
            }
        } else {
            tab.pref_col = None;
        }
        tab.doc.text.break_group();
        return;
    }
    tab.pref_col = None;
    let (sel_before, ver_before) = (tab.sel, tab.doc.text.ver);
    match key {
        Key::Backspace => {
            if a == b && a > 0 {
                let s = &tab.doc.text.s;
                let from = if cmd { word_left(s, a) } else { prev_boundary(s, a) };
                tab.sel = (from, a);
            }
            replace_sel(tab, "", true, now);
        }
        Key::Delete => {
            if a == b && a < len {
                let s = &tab.doc.text.s;
                let to = if cmd { word_right(s, a) } else { next_boundary(s, a) };
                tab.sel = (a, to);
            }
            replace_sel(tab, "", true, now);
        }
        Key::Enter if !cmd => replace_sel(tab, "\n", true, now),
        Key::Tab if !cmd && !m.shift => replace_sel(tab, "\t", true, now),
        Key::A if cmd => tab.sel = (0, len),
        Key::Z if cmd && !m.shift => {
            if let Some((x, y)) = tab.doc.text.undo() {
                tab.sel = (x, y);
            }
        }
        Key::Y if cmd => {
            if let Some((_, y)) = tab.doc.text.redo() {
                tab.sel = (y, y);
            }
        }
        Key::Z if cmd && m.shift => {
            if let Some((_, y)) = tab.doc.text.redo() {
                tab.sel = (y, y);
            }
        }
        Key::Escape => tab.sel = (head, head),
        _ => {}
    }
    if matches!(key, Key::Backspace | Key::Delete) && tab.doc.text.ver == ver_before {
        // refused (data block line): keep the cursor where it was
        tab.sel = sel_before;
    }
}

/// Select the word (set / surface names included) around byte `b`.
fn word_range(s: &str, b: usize) -> (usize, usize) {
    let mut a = b;
    while let Some(c) = char_before(s, a) {
        if !is_name(c) {
            break;
        }
        a -= c.len_utf8();
    }
    let mut e = b;
    while let Some(c) = char_at(s, e) {
        if !is_name(c) {
            break;
        }
        e += c.len_utf8();
    }
    if a == e && char_at(s, b).is_some_and(|c| c != '\n') {
        (b, next_boundary(s, b))
    } else {
        (a, e)
    }
}

// ---------------------------------------------------------------- widget

/// Show the editor for `tab`. `ext` are the analysis diagnostics for the current text version
/// (sorted by start offset), `ext_key` identifies them.
pub fn show(ui: &mut egui::Ui, tab: &mut DocTab, font: &FontId, ext: &[Diag], ext_key: u64, gutter_w: f32) -> ViewOut {
    let id = tab.edit_id;
    tab.ensure_map();
    clamp_sel(tab);
    let (char_w, row_h) = ui.fonts_mut(|f| (f.glyph_width(font, '0'), f.row_height(font).ceil() + 1.0));
    let full = ui.available_rect_before_wrap();
    let page_lines = ((full.height() / row_h) as usize).saturating_sub(2).max(1);

    // Escape ends a rectangular selection (egui also takes the keyboard focus away on Escape)
    if tab.rect.is_some() && ui.input(|i| i.key_pressed(Key::Escape)) {
        let b = rect_head_byte(tab);
        tab.rect = None;
        tab.sel = (b, b);
    }
    // ---- keyboard (only while focused)
    let mut focused = ui.memory(|m| m.has_focus(id));
    let mut changed = false;
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
        });
        let (c, moved) = handle_events(ui, tab, page_lines);
        changed = c;
        if changed || moved {
            tab.scroll_req = Some((tab.sel.1, false));
        }
    }

    // ---- pending jump (outline, problems, go to, find …)
    if let Some(j) = tab.jump.take() {
        tab.ensure_map();
        let line = j.line.min(tab.map.starts.len().saturating_sub(1));
        let s = tab.map.starts[line];
        let (a, b) = j.sel.unwrap_or((s, s));
        tab.sel = (a, b);
        clamp_sel(tab);
        tab.pref_col = None;
        tab.doc.text.break_group();
        tab.scroll_req = Some((tab.sel.0, true));
        if j.focus {
            ui.memory_mut(|m| m.request_focus(id));
            focused = true;
        }
    }

    let text_rect = Rect::from_min_max(Pos2::new(full.left() + gutter_w, full.top()), full.max);

    // ---- middle-button autoscroll
    let (ptr_now, mid_pressed, mid_released, any_other_press, dt) = ui.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.button_pressed(egui::PointerButton::Middle),
            i.pointer.button_released(egui::PointerButton::Middle),
            i.pointer.button_pressed(egui::PointerButton::Primary) || i.pointer.button_pressed(egui::PointerButton::Secondary),
            i.stable_dt.min(0.1),
        )
    });
    let mut swallow_click = false;
    let mut forced_offset: Option<Vec2> = None;
    let over_text = ptr_now.is_some_and(|p| text_rect.contains(p)) && ui.ctx().layer_id_at(ptr_now.unwrap()).is_none_or(|l| l.order == egui::Order::Background);
    match tab.autoscroll {
        None if mid_pressed && over_text => {
            tab.autoscroll = Some(AutoScroll { anchor: ptr_now.unwrap(), sticky: false, moved: false });
        }
        Some(mut a) => {
            let stop_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
            if (a.sticky && (mid_pressed || any_other_press)) || stop_key {
                tab.autoscroll = None;
                swallow_click = any_other_press;
            } else {
                if let Some(p) = ptr_now {
                    if (p - a.anchor).length() > 6.0 {
                        a.moved = true;
                    }
                }
                if mid_released {
                    if a.moved {
                        tab.autoscroll = None;
                    } else {
                        a.sticky = true;
                        tab.autoscroll = Some(a);
                    }
                } else {
                    tab.autoscroll = Some(a);
                }
            }
            if let (Some(a), Some(p)) = (tab.autoscroll, ptr_now) {
                let g = &tab.geom;
                if g.row_h > 0.0 {
                    let off = Vec2::new(g.view.left() - (g.origin.x - PAD_X), g.view.top() - g.origin.y);
                    let speed = |d: f32| if d.abs() <= AUTOSCROLL_DEAD { 0.0 } else { d.signum() * (d.abs() - AUTOSCROLL_DEAD) * 9.0 };
                    let d = p - a.anchor;
                    let v = Vec2::new(speed(d.x), speed(d.y));
                    if v != Vec2::ZERO {
                        forced_offset = Some((off + v * dt).max(Vec2::ZERO));
                    }
                }
                ui.ctx().set_cursor_icon(CursorIcon::AllScroll);
                ui.ctx().request_repaint();
            }
        }
        _ => {}
    }
    let gutter_rect = Rect::from_min_max(full.min, Pos2::new(full.left() + gutter_w, full.bottom()));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(Align::Min)));
    let n = tab.map.starts.len();
    let content = Vec2::new(tab.map.max_cols as f32 * char_w + PAD_X * 2.0 + 80.0, n as f32 * row_h + (full.height() * 0.5).max(row_h));
    // the theme is part of the key: the cached layout jobs contain colours
    let font_key = (font.size * 10.0) as u32 | ((crate::theme::is_dark() as u32) << 31);
    let mut resp_opt = None;
    let mut sa = egui::ScrollArea::both().id_salt(id.with("scroll")).auto_shrink([false, false]).animated(false);
    if let Some(o) = forced_offset {
        sa = sa.scroll_offset(o);
    }
    sa.show_viewport(&mut child, |ui, vp| {
        ui.set_min_size(content);
        let origin_ui = ui.max_rect().min;
        let rect = Rect::from_min_size(origin_ui, content.max(ui.max_rect().size()));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let origin = origin_ui + Vec2::new(PAD_X, 0.0);
        let first = ((vp.min.y / row_h).floor().max(0.0) as usize).min(n.saturating_sub(1));
        let last = ((vp.max.y / row_h).ceil().max(0.0) as usize).min(n.saturating_sub(1));

        // ---- highlight + lay out the visible lines only
        let key = (tab.doc.text.ver, font_key, ext_key, first, last);
        if tab.view.key != key {
            let (jobs, diags) = syntax::highlight_lines(&tab.doc.text.s, &tab.map.starts, first, last, font, ext);
            tab.view = ViewCache { key, jobs, diags };
        }
        let galleys: Vec<Arc<egui::Galley>> = ui.fonts_mut(|f| tab.view.jobs.iter().map(|j| f.layout_job(j.clone())).collect());
        tab.geom = Geom { origin, row_h, char_w, first, galleys, view: ui.clip_rect() };

        // ---- mouse
        let ptr = ui.input(|i| i.pointer.interact_pos());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }
        let pressed = ui.input(|i| i.pointer.primary_pressed()) && resp.contains_pointer() && !swallow_click && tab.autoscroll.is_none();
        if pressed {
            // the press position, not the latest pointer position (events may arrive batched)
            if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(ptr) {
                let b = tab.geom.byte_at(tab, p);
                let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
                if alt {
                    // Alt+drag (also Alt+Shift+drag): rectangular selection
                    let line = tab.geom.line_at_y(tab, p.y);
                    let col = ((p.x - tab.geom.origin.x) / tab.geom.char_w.max(1.0)).round().max(0.0) as usize;
                    tab.rect = Some(match (shift, tab.rect) {
                        (true, Some(r)) => RectSel { anchor: r.anchor, head: (line, col) },
                        _ => RectSel { anchor: (line, col), head: (line, col) },
                    });
                } else {
                    tab.rect = None;
                }
                tab.sel = if shift && !alt { (tab.sel.0, b) } else { (b, b) };
                tab.pref_col = None;
                tab.doc.text.break_group();
                ui.memory_mut(|m| m.request_focus(id));
                focused = true;
            }
        } else if resp.dragged_by(egui::PointerButton::Primary) {
            if let Some(p) = ptr {
                let b = tab.geom.byte_at(tab, p);
                if tab.rect.is_some() {
                    let line = tab.geom.line_at_y(tab, p.y);
                    let col = ((p.x - tab.geom.origin.x) / tab.geom.char_w.max(1.0)).round().max(0.0) as usize;
                    if let Some(r) = tab.rect.as_mut() {
                        r.head = (line, col);
                    }
                    let hb = rect_head_byte(tab);
                    tab.sel = (hb, hb);
                } else if b != tab.sel.1 {
                    tab.sel.1 = b;
                }
                if !tab.geom.view.contains(p) {
                    tab.scroll_req = Some((b, false));
                    ui.ctx().request_repaint();
                }
            }
        }
        if resp.triple_clicked() {
            let line = tab.map.line_of_byte(tab.sel.1);
            let (ls, _) = line_span(tab, line);
            let next = tab.map.starts.get(line + 1).copied().unwrap_or(tab.doc.text.s.len());
            tab.sel = (ls, next);
        } else if resp.double_clicked() {
            let (a, e) = word_range(&tab.doc.text.s, tab.sel.1);
            tab.sel = (a, e);
        }
        // ---- scroll requests
        if let Some((b, center)) = tab.scroll_req.take() {
            let p = tab.geom.pos_of(tab, b.min(tab.doc.text.s.len()));
            let r = Rect::from_min_size(p, Vec2::new(char_w.max(2.0), row_h));
            if center {
                ui.scroll_to_rect(r.expand2(Vec2::new(char_w * 8.0, 0.0)), Some(Align::Center));
            } else {
                ui.scroll_to_rect(r.expand2(Vec2::new(char_w * 6.0, row_h)), None);
            }
            ui.ctx().request_repaint();
        }

        // ---- paint
        let painter = ui.painter();
        let clip = ui.clip_rect();
        let (sa, sb) = ordered(tab.sel);
        let head_line = tab.map.line_of_byte(tab.sel.1);
        if focused && (first..=last).contains(&head_line) {
            let y = origin.y + head_line as f32 * row_h;
            painter.rect_filled(Rect::from_min_max(Pos2::new(clip.left(), y), Pos2::new(clip.right(), y + row_h)), 0.0, crate::theme::rgb(242, 246, 253));
        }
        let sel_bg = if focused { crate::theme::rgb(190, 214, 250) } else { crate::theme::rgb(220, 226, 236) };
        if let Some(r) = tab.rect {
            let (l0, l1, c0, c1) = r.bounds();
            for li in l0.max(first)..=l1.min(last) {
                let y = origin.y + li as f32 * row_h;
                let x0 = origin.x + c0 as f32 * char_w;
                let x1 = origin.x + c1 as f32 * char_w;
                if c1 > c0 {
                    painter.rect_filled(Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1, y + row_h)), 0.0, sel_bg);
                }
                if focused {
                    let xc = origin.x + r.head.1 as f32 * char_w;
                    painter.line_segment([Pos2::new(xc, y + 1.0), Pos2::new(xc, y + row_h - 1.0)], Stroke::new(1.5_f32, crate::theme::rgb(20, 30, 60)));
                }
            }
        }
        for (k, g) in tab.geom.galleys.iter().enumerate() {
            let li = first + k;
            let y = origin.y + li as f32 * row_h;
            let (ls, le) = line_span(tab, li);
            if sa < sb && sa <= le && sb > ls {
                let x0 = if sa > ls { tab.geom.x_of(tab, sa) } else { 0.0 };
                let x1 = if sb <= le { tab.geom.x_of(tab, sb) } else { g.size().x + char_w * 0.6 };
                painter.rect_filled(Rect::from_min_max(Pos2::new(origin.x + x0, y), Pos2::new(origin.x + x1.max(x0 + 1.0), y + row_h)), 0.0, sel_bg);
            }
            painter.galley(Pos2::new(origin.x, y + ((row_h - g.size().y) * 0.5).floor()), g.clone(), crate::theme::t(Palette::DATA));
        }
        if focused && tab.rect.is_none() && (first..=last).contains(&head_line) {
            let p = tab.geom.pos_of(tab, tab.sel.1);
            painter.line_segment([Pos2::new(p.x, p.y + 1.0), Pos2::new(p.x, p.y + row_h - 1.0)], Stroke::new(2.0_f32, crate::theme::rgb(20, 30, 60)));
            // IME / on-screen keyboard position
            ui.ctx().output_mut(|o| {
                o.mutable_text_under_cursor = resp.hovered();
            });
        }
        // autoscroll anchor mark
        if let Some(a) = tab.autoscroll {
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, id.with("autoscroll")));
            let c = crate::theme::rgb(70, 80, 100);
            p.circle(a.anchor, 13.0, Color32::from_white_alpha(230), Stroke::new(1.0_f32, c));
            for (dx, dy) in [(0.0, -1.0), (0.0, 1.0), (-1.0, 0.0), (1.0, 0.0)] {
                let tip = a.anchor + Vec2::new(dx, dy) * 9.0;
                let base = a.anchor + Vec2::new(dx, dy) * 4.5;
                let side = Vec2::new(-dy, dx) * 3.5;
                p.add(egui::Shape::convex_polygon(vec![tip, base + side, base - side], c, Stroke::NONE));
            }
            p.circle_filled(a.anchor, 1.8, c);
        }
        resp_opt = Some(resp);
    });
    let response = resp_opt.expect("scroll area body runs");
    ui.allocate_rect(full, Sense::hover());

    // ---- gutter
    paint_gutter(ui, tab, gutter_rect, ext, font);

    // ---- status: cursor line / column
    let head = tab.sel.1;
    let line = tab.map.line_of_byte(head);
    let (ls, le) = line_span(tab, line);
    let col = tab.doc.text.s[ls..head.clamp(ls, le)].chars().count();
    tab.cursor = (line, col, head);
    ViewOut { response, changed }
}

fn paint_gutter(ui: &egui::Ui, tab: &DocTab, grect: Rect, ext: &[Diag], font: &FontId) {
    let g = &tab.geom;
    let painter = ui.painter_at(grect);
    painter.rect_filled(Rect::from_min_max(grect.min, Pos2::new(grect.right() - 4.0, grect.bottom())), 0.0, crate::theme::t(crate::editor::GUTTER_BG));
    painter.line_segment([Pos2::new(grect.right() - 4.0, grect.top()), Pos2::new(grect.right() - 4.0, grect.bottom())], Stroke::new(1.0_f32, crate::theme::rgb(228, 230, 235)));
    if g.galleys.is_empty() {
        return;
    }
    let first = g.first;
    let last = first + g.galleys.len() - 1;
    // severity per visible line: live checks of the window + analysis diagnostics
    let mut sev: Vec<Option<Sev>> = vec![None; g.galleys.len()];
    let mut mark = |line: usize, s: Sev| {
        if line >= first && line <= last {
            let e = &mut sev[line - first];
            *e = Some(e.map_or(s, |x: Sev| x.min(s)));
        }
    };
    for d in &tab.view.diags {
        mark(d.line, d.sev);
    }
    let lo = tab.map.starts[first];
    let hi = tab.map.starts.get(last + 1).copied().unwrap_or(usize::MAX);
    let from = ext.partition_point(|d| d.start < lo);
    for d in ext[from..].iter().take_while(|d| d.start < hi) {
        mark(d.line, d.sev);
    }
    let small = FontId::monospace(font.size * 0.88);
    for k in 0..g.galleys.len() {
        let i = first + k;
        let y = g.origin.y + i as f32 * g.row_h + g.row_h * 0.5;
        if y < grect.top() - g.row_h || y > grect.bottom() + g.row_h {
            continue;
        }
        let Some(&real) = tab.map.real.get(i) else { break };
        let is_ph = tab.map.real.get(i + 1).map(|n| n - real > 1).unwrap_or(false) || tab.line_text(i).starts_with(crate::store::PH_PREFIX);
        let color = if is_ph { crate::theme::t(Palette::PH_FG) } else { crate::theme::rgb(160, 164, 172) };
        painter.text(Pos2::new(grect.right() - 10.0, y), egui::Align2::RIGHT_CENTER, util::fmt_int(real), small.clone(), color);
        if let Some(s) = sev[k] {
            let c = if s == Sev::Error { crate::theme::t(Palette::ERR) } else { crate::theme::t(Palette::WARN) };
            painter.circle_filled(Pos2::new(grect.left() + 6.0, y), 3.2, c);
        }
    }
}
