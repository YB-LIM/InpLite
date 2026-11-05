//! A read-only, virtualised line view for very long texts (log files, compare panes): only the
//! visible lines are formatted and painted, the first visible line is an integer (no float
//! precision problems with millions of lines), lines can be selected and copied.

use crate::util;
use egui::{Color32, CursorIcon, Event, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};

/// Look of one line.
#[derive(Clone, Default)]
pub struct LineStyle {
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub strong: bool,
    /// character ranges with a stronger background (e.g. changed characters)
    pub spans: Vec<(usize, usize, Color32)>,
}

/// What a line view shows.
pub trait LineSource {
    fn count(&self) -> u64;
    /// text of line `i` (tabs are shown as 4 blanks) and its look
    fn line(&self, i: u64) -> (String, LineStyle);
    /// number shown in the gutter (None: no number, e.g. a filler line in a compare view)
    fn number(&self, i: u64) -> Option<u64> {
        Some(i + 1)
    }
    /// longest line in characters (horizontal scroll range)
    fn max_cols(&self) -> usize;
    /// largest number in the gutter (its width)
    fn max_number(&self) -> u64 {
        self.count()
    }
    /// marks on the scroll bar: (line, colour)
    fn marks(&self) -> &[(u64, Color32)] {
        &[]
    }
}

#[derive(Clone, Default)]
pub struct LineViewState {
    /// first visible line
    pub top: u64,
    /// horizontal scroll in pixels
    pub left: f32,
    /// selected lines (anchor, head)
    pub sel: Option<(u64, u64)>,
    /// keep the last line visible (log monitoring)
    pub follow: bool,
    /// fractional wheel scrolling
    acc: f32,
    /// scroll to make this line visible (centered) in the next frame
    pub reveal: Option<u64>,
    dragging: bool,
}

impl LineViewState {
    pub fn sel_range(&self) -> Option<(u64, u64)> {
        self.sel.map(|(a, b)| (a.min(b), a.max(b)))
    }
    pub fn go(&mut self, line: u64) {
        self.sel = Some((line, line));
        self.reveal = Some(line);
        self.follow = false;
    }
}

pub struct LineViewOut {
    pub response: egui::Response,
    pub double_clicked: Option<u64>,
    /// lines that fit in the view
    pub page: u64,
}

pub fn expand_tabs(s: &str) -> String {
    if s.contains('\t') { s.replace('\t', "    ") } else { s.to_string() }
}

/// Show `src` in the remaining space of `ui`.
pub fn show(ui: &mut egui::Ui, id: egui::Id, st: &mut LineViewState, src: &dyn LineSource, font_size: f32) -> LineViewOut {
    let font = FontId::monospace(font_size);
    let (row_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font).ceil() + 2.0, f.glyph_width(&font, '0')));
    let n = src.count();
    let avail = ui.available_rect_before_wrap();
    let rect = Rect::from_min_size(avail.min, avail.size().max(Vec2::new(80.0, 40.0)));
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    ui.allocate_rect(rect, Sense::hover());
    let sb = 12.0;
    let digits = util::fmt_int(src.max_number().max(1)).len().max(4);
    let gutter = char_w * 0.88 * digits as f32 + 18.0;
    let content_w = src.max_cols() as f32 * char_w + 40.0;
    let body_w = rect.width() - sb - gutter;
    let hbar = content_w > body_w;
    let body = Rect::from_min_max(Pos2::new(rect.left() + gutter, rect.top()), Pos2::new(rect.right() - sb, rect.bottom() - if hbar { sb } else { 0.0 }));
    let vis = ((body.height() / row_h).floor() as u64).max(1);
    let max_top = n.saturating_sub(vis);
    let max_left = (content_w - body.width()).max(0.0);
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: true }));
    }

    // ---- wheel
    if resp.hovered() || resp.contains_pointer() {
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.y != 0.0 {
            st.acc -= d.y / row_h;
            let whole = st.acc.trunc();
            st.acc -= whole;
            let t = (st.top as i64 + whole as i64).clamp(0, max_top as i64) as u64;
            if t != st.top {
                st.top = t;
                st.follow = false;
            }
            if whole < 0.0 {
                st.follow = false;
            }
        }
        if d.x != 0.0 {
            st.left = (st.left - d.x).clamp(0.0, max_left);
        }
    }
    // ---- keyboard
    let mut copy = false;
    if focused {
        let evs = ui.input(|i| i.events.clone());
        for ev in evs {
            match ev {
                Event::Copy => copy = true,
                Event::Key { key, pressed: true, modifiers: m, .. } => {
                    let head = st.sel.map(|s| s.1).unwrap_or(st.top);
                    let mut target: Option<u64> = None;
                    match key {
                        Key::ArrowDown => target = Some((head + 1).min(n.saturating_sub(1))),
                        Key::ArrowUp => target = Some(head.saturating_sub(1)),
                        Key::PageDown => target = Some((head + vis).min(n.saturating_sub(1))),
                        Key::PageUp => target = Some(head.saturating_sub(vis)),
                        Key::Home if m.command => target = Some(0),
                        Key::End if m.command => {
                            target = Some(n.saturating_sub(1));
                        }
                        Key::Home => st.left = 0.0,
                        Key::End => st.left = max_left,
                        Key::ArrowLeft => st.left = (st.left - char_w * 4.0).max(0.0),
                        Key::ArrowRight => st.left = (st.left + char_w * 4.0).min(max_left),
                        Key::A if m.command => st.sel = Some((0, n.saturating_sub(1))),
                        Key::Escape => st.sel = None,
                        _ => {}
                    }
                    if let Some(t) = target {
                        st.sel = Some(if m.shift { (st.sel.map(|s| s.0).unwrap_or(head), t) } else { (t, t) });
                        if t < st.top {
                            st.top = t;
                        } else if t >= st.top + vis {
                            st.top = t + 1 - vis;
                        }
                        st.follow = key == Key::End && m.command;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(r) = st.reveal.take() {
        if r < st.top || r >= st.top + vis {
            st.top = r.saturating_sub(vis / 2);
        }
        st.follow = false;
    }
    if st.follow {
        st.top = max_top;
    }
    st.top = st.top.min(max_top);
    st.left = st.left.clamp(0.0, max_left);

    // ---- pointer
    let track = Rect::from_min_max(Pos2::new(rect.right() - sb, rect.top()), Pos2::new(rect.right(), body.bottom()));
    let htrack = Rect::from_min_max(Pos2::new(body.left(), body.bottom()), Pos2::new(body.right(), rect.bottom()));
    let thumb_h = (track.height() * vis as f32 / n.max(1) as f32).clamp(24.0, track.height());
    let press = ui.input(|i| i.pointer.press_origin());
    let in_track = press.is_some_and(|p| track.expand(2.0).contains(p));
    let in_htrack = hbar && press.is_some_and(|p| htrack.expand(2.0).contains(p));
    let line_at = |y: f32, top: u64| -> u64 { (top as i64 + ((y - body.top()) / row_h).floor() as i64).clamp(0, n.saturating_sub(1) as i64) as u64 };
    let mut double = None;
    if let Some(pp) = resp.interact_pointer_pos() {
        if in_track && (resp.dragged() || resp.clicked() || resp.drag_started()) {
            let f = ((pp.y - track.top() - thumb_h / 2.0) / (track.height() - thumb_h).max(1.0)).clamp(0.0, 1.0);
            st.top = (f as f64 * max_top as f64).round() as u64;
            st.follow = st.top == max_top && max_top > 0;
        } else if in_htrack && (resp.dragged() || resp.clicked() || resp.drag_started()) {
            let tw = (htrack.width() * body.width() / content_w).clamp(24.0, htrack.width());
            let f = ((pp.x - htrack.left() - tw / 2.0) / (htrack.width() - tw).max(1.0)).clamp(0.0, 1.0);
            st.left = f * max_left;
        } else if n > 0 {
            let shift = ui.input(|i| i.modifiers.shift);
            if resp.drag_started_by(egui::PointerButton::Primary) || (resp.clicked() && !st.dragging) {
                let l = line_at(press.unwrap_or(pp).y, st.top);
                st.sel = Some(if shift { (st.sel.map(|s| s.0).unwrap_or(l), l) } else { (l, l) });
                st.dragging = resp.drag_started();
                ui.memory_mut(|m| m.request_focus(id));
            } else if resp.dragged_by(egui::PointerButton::Primary) && st.dragging {
                let l = line_at(pp.y, st.top);
                if let Some(s) = st.sel.as_mut() {
                    s.1 = l;
                }
                // drag past the edges: scroll
                if pp.y < body.top() && st.top > 0 {
                    st.top -= 1;
                    st.follow = false;
                    ui.ctx().request_repaint();
                } else if pp.y > body.bottom() && st.top < max_top {
                    st.top += 1;
                    ui.ctx().request_repaint();
                }
            }
            if resp.secondary_clicked() {
                let l = line_at(pp.y, st.top);
                if !st.sel_range().is_some_and(|(a, b)| l >= a && l <= b) {
                    st.sel = Some((l, l));
                }
                ui.memory_mut(|m| m.request_focus(id));
            }
            if resp.double_clicked() {
                double = Some(line_at(pp.y, st.top));
            }
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        st.dragging = false;
    }
    if resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| body.contains(p)) {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    if copy {
        if let Some((a, b)) = st.sel_range() {
            let b = b.min(a + 2_000_000);
            let mut s = String::new();
            for i in a..=b.min(n.saturating_sub(1)) {
                if i > a {
                    s.push('\n');
                }
                s.push_str(&src.line(i).0);
            }
            ui.ctx().copy_text(s);
        }
    }

    // ---- paint
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, crate::theme::t(Color32::WHITE));
    p.rect_filled(Rect::from_min_max(rect.min, Pos2::new(body.left(), rect.bottom())), 0.0, crate::theme::t(crate::editor::GUTTER_BG));
    let sel = st.sel_range();
    let sel_bg = if focused { crate::theme::rgb(190, 214, 250) } else { crate::theme::rgb(214, 224, 240) };
    let first_col = (st.left / char_w).floor() as usize;
    let cols = (body.width() / char_w).ceil() as usize + 2;
    let x0 = body.left() + 6.0 - (st.left - first_col as f32 * char_w);
    let small = FontId::monospace(font_size * 0.88);
    let bp = ui.painter_at(body);
    for k in 0..=vis {
        let i = st.top + k;
        if i >= n {
            break;
        }
        let y = body.top() + k as f32 * row_h;
        let (text, style) = src.line(i);
        let row = Rect::from_min_size(Pos2::new(body.left(), y), Vec2::new(body.width(), row_h));
        if let Some(bg) = style.bg {
            bp.rect_filled(row, 0.0, bg);
        }
        if sel.is_some_and(|(a, b)| i >= a && i <= b) {
            if style.bg.is_some() {
                // keep the line colour visible under the selection
                bp.rect_filled(row, 0.0, sel_bg.gamma_multiply(0.45));
                bp.rect_stroke(row.shrink(0.5), 0.0, Stroke::new(1.0_f32, crate::theme::rgb(90, 130, 200)), egui::StrokeKind::Inside);
            } else {
                bp.rect_filled(row, 0.0, sel_bg);
            }
        }
        let shown: String = text.chars().skip(first_col).take(cols).collect();
        for &(a, b, c) in &style.spans {
            if b <= first_col || a >= first_col + cols {
                continue;
            }
            let xa = x0 + (a.max(first_col) - first_col) as f32 * char_w;
            let xb = x0 + (b.min(first_col + cols) - first_col) as f32 * char_w;
            bp.rect_filled(Rect::from_min_max(Pos2::new(xa, y + 1.0), Pos2::new(xb.max(xa + 2.0), y + row_h - 1.0)), 1.0, c);
        }
        let fg = style.fg.unwrap_or(crate::theme::t(crate::syntax::Palette::DATA));
        bp.text(Pos2::new(x0, y + row_h * 0.5), egui::Align2::LEFT_CENTER, &shown, font.clone(), fg);
        if style.strong {
            bp.text(Pos2::new(x0 + 0.6, y + row_h * 0.5), egui::Align2::LEFT_CENTER, &shown, font.clone(), fg);
        }
        if let Some(num) = src.number(i) {
            p.text(Pos2::new(body.left() - 8.0, y + row_h * 0.5), egui::Align2::RIGHT_CENTER, util::fmt_int(num), small.clone(), crate::theme::rgb(160, 164, 172));
        }
    }
    p.line_segment([Pos2::new(body.left() - 1.0, rect.top()), Pos2::new(body.left() - 1.0, rect.bottom())], Stroke::new(1.0_f32, crate::theme::rgb(228, 230, 235)));
    // vertical scroll bar with marks
    p.rect_filled(track, 0.0, crate::theme::rgb(243, 244, 246));
    for &(l, c) in src.marks() {
        let y = track.top() + track.height() * (l as f32 / n.max(1) as f32);
        p.rect_filled(Rect::from_min_size(Pos2::new(track.left() + 1.0, y), Vec2::new(sb - 2.0, 2.5)), 0.0, c);
    }
    let frac = if max_top == 0 { 0.0 } else { st.top as f64 / max_top as f64 };
    let ty = track.top() + (track.height() - thumb_h) * frac as f32;
    let thumb = Rect::from_min_size(Pos2::new(track.left() + 2.0, ty), Vec2::new(sb - 4.0, thumb_h));
    p.rect_filled(thumb, 4.0, crate::theme::rgba_t(150, 158, 172, 150));
    if hbar {
        p.rect_filled(htrack, 0.0, crate::theme::rgb(243, 244, 246));
        let tw = (htrack.width() * body.width() / content_w).clamp(24.0, htrack.width());
        let tx = htrack.left() + (htrack.width() - tw) * (st.left / max_left.max(1.0));
        p.rect_filled(Rect::from_min_size(Pos2::new(tx, htrack.top() + 2.0), Vec2::new(tw, sb - 4.0)), 4.0, crate::theme::rgba_t(150, 158, 172, 150));
    }
    if focused {
        p.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, crate::theme::rgb(200, 212, 235)), egui::StrokeKind::Inside);
    }
    let _ = Modifiers::NONE;
    LineViewOut { response: resp, double_clicked: double, page: vis }
}
