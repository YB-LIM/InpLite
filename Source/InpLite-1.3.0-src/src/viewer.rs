//! Data viewer for bulk blocks: virtual scrolling over millions of lines with aligned columns.

use crate::actions::Action;
use crate::store::{Bulk, SPARSE, STORE};
use crate::tab::DocTab;
use crate::util;
use egui::{Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};

/// Blocks up to this size may be loaded into the editor as text. Every keystroke re-indexes the
/// editable text, so much larger blocks would make typing slow and use a lot of memory (undo,
/// background checks); they stay on disk instead.
pub const EXPAND_MAX_LINES: u64 = 200_000;
pub const EXPAND_MAX_BYTES: u64 = 16 << 20;

pub fn can_expand(b: &Bulk) -> bool {
    b.lines <= EXPAND_MAX_LINES && b.size() <= EXPAND_MAX_BYTES
}

/// Why a block cannot be edited as text, and what to do instead.
pub fn expand_refusal(b: &Bulk) -> String {
    format!(
        "This data block has {} lines ({}). Blocks up to {} lines / {} can be edited as text; larger ones stay on disk so that typing stays fast and memory use stays low.\n\n\
         To change this data: save the block as an *Include file (the main file then only has an *Include line) and edit or regenerate that file, e.g. in your pre-processor. To look at it, use the data viewer and Go to (Ctrl+G).",
        util::fmt_int(b.lines),
        util::fmt_bytes(b.size()),
        util::fmt_int(EXPAND_MAX_LINES),
        util::fmt_bytes(EXPAND_MAX_BYTES)
    )
}

fn first_id(b: &Bulk, row: u64) -> Option<u64> {
    let l = b.line(row)?;
    util::parse_u64(l.split(|c| *c == b',').next()?)
}

/// Quick label lookup for sorted blocks (binary search over the line index). Never blocks:
/// returns None if the index is not ready or the label is not where a sorted block would have it.
pub fn find_label_fast(b: &Bulk, id: u64) -> Option<u64> {
    let sp = b.sparse.get()?.clone();
    let (mut lo, mut hi) = (0usize, sp.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        match first_id(b, mid as u64 * SPARSE) {
            Some(v) if v <= id => lo = mid,
            Some(_) => hi = mid,
            None => lo = mid,
        }
    }
    let start = lo as u64 * SPARSE;
    (start..(start + SPARSE * 2).min(b.lines)).find(|&r| first_id(b, r) == Some(id))
}

/// Full scan (for unsorted blocks). Slow on big blocks: run it in a background thread.
pub fn find_label_scan(b: &Bulk, id: u64) -> Option<u64> {
    let mut r = 0;
    for line in b.bytes().split(|c| *c == b'\n') {
        if util::parse_u64(line.split(|c| *c == b',').next().unwrap_or(&[])) == Some(id) {
            return Some(r);
        }
        r += 1;
    }
    None
}

/// Blocking lookup (fast path, then full scan). For background threads only.
pub fn find_label(b: &Bulk, id: u64) -> Option<u64> {
    let _ = b.sparse_blocking();
    find_label_fast(b, id).or_else(|| find_label_scan(b, id))
}

pub fn viewer_ui(ui: &mut egui::Ui, tab: &mut DocTab, font_size: f32, acts: &mut Vec<Action>) {
    tab.ensure_map();
    let Some(v) = tab.viewer.as_mut() else {
        ui.label(egui::RichText::new("Click a data placeholder line (blue) or use the outline to open a large data block here.").weak());
        return;
    };
    let Some(b) = STORE.get(v.bulk) else {
        ui.label("Data block not available.");
        return;
    };
    let ph_line = tab.map.bulk_line.get(&b.id).copied();
    let real0 = ph_line.and_then(|l| tab.map.real.get(l).copied()).unwrap_or(1);
    // "Save as *Include" needs the keyword block that owns this data block
    let split_action = ph_line.and_then(|l| {
        let an = tab.current_analysis()?;
        let bi = an.block_at_line(l)?;
        let stem = tab.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string();
        Some(crate::outline::block_split_action(an, bi, &stem))
    });
    let index_ready = b.ensure_sparse();
    let v = tab.viewer.as_mut().unwrap();
    // background label search finished?
    if let Some((id, rx)) = &v.search {
        match rx.try_recv() {
            Ok(Some(r)) => {
                v.sel = Some(r);
                v.top = r.saturating_sub(5);
                v.msg.clear();
                v.search = None;
            }
            Ok(None) => {
                v.msg = format!("label {id} not found in this block");
                v.search = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(60)),
            Err(_) => v.search = None,
        }
    }
    if !index_ready {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
    }
    let font = FontId::monospace(font_size);
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 3.0;
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));

    // header
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&b.kw_line).monospace().strong().color(crate::theme::t(crate::syntax::Palette::KEYWORD)));
        ui.label(egui::RichText::new(format!("{} lines · {} · lines {}–{}", util::fmt_int(b.lines), util::fmt_bytes(b.size()), util::fmt_int(real0), util::fmt_int(real0 + b.lines - 1))).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("×").on_hover_text("Close viewer").clicked() {
                close = true;
            }
            let small = can_expand(&b);
            let tip = if small { "Load this block into the editor as normal text".to_string() } else { "Too large to edit as text — click for details".to_string() };
            if ui.add(egui::Button::new(if small { "Edit as text" } else { "Edit as text…" })).on_hover_text(&tip).clicked() {
                acts.push(Action::ExpandBulk { bulk: b.id });
            }
            let can_split = split_action.is_some();
            if ui
                .add_enabled(can_split, egui::Button::new("Save as *Include…"))
                .on_hover_text("Write this keyword block (keyword line + data) to a separate file and replace it with an *Include line")
                .on_disabled_hover_text("Waiting for the analysis to finish…")
                .clicked()
            {
                if let Some(a) = &split_action {
                    acts.push(a.clone());
                }
            }
            let r = ui.add(egui::TextEdit::singleline(&mut v.goto).desired_width(120.0).hint_text("label or #line"));
            let go = (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("Go").clicked();
            ui.label("Go to");
            if go {
                let q = v.goto.trim();
                if let Some(ln) = q.strip_prefix('#') {
                    if let Ok(n) = ln.replace(',', "").parse::<u64>() {
                        if n >= real0 && n < real0 + b.lines {
                            v.sel = Some(n - real0);
                            v.top = (n - real0).saturating_sub(5);
                            v.msg.clear();
                        } else {
                            v.msg = "line outside this block".into();
                        }
                    }
                } else if let Ok(id) = q.replace(',', "").parse::<u64>() {
                    match find_label_fast(&b, id) {
                        Some(r) => {
                            v.sel = Some(r);
                            v.top = r.saturating_sub(5);
                            v.msg.clear();
                        }
                        None => {
                            // not where a sorted block would have it: full scan in the background
                            let (tx, rx) = std::sync::mpsc::channel();
                            let bb = b.clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(find_label(&bb, id));
                            });
                            v.search = Some((id, rx));
                            v.msg = "searching…".into();
                        }
                    }
                }
            }
            if !v.msg.is_empty() {
                let c = if v.search.is_some() { crate::theme::t(egui::Color32::GRAY) } else { crate::theme::t(crate::syntax::Palette::ERR) };
                ui.label(egui::RichText::new(&v.msg).color(c));
            }
        });
    });
    if close {
        tab.viewer = None;
        return;
    }
    ui.separator();

    // body
    let avail = ui.available_rect_before_wrap();
    let (rect, resp) = ui.allocate_exact_size(avail.size(), Sense::click_and_drag());
    let sb_w = 12.0;
    let body = Rect::from_min_max(rect.min, Pos2::new(rect.right() - sb_w, rect.bottom()));
    let vis = ((body.height() / row_h).floor() as u64).max(1);
    // until the line index is built only the first lines can be reached
    let reachable = if index_ready { b.lines } else { b.lines.min(4096) };
    let max_top = reachable.saturating_sub(vis);
    v.top = v.top.min(max_top);
    // input
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            let rows = (-dy / row_h * 1.0).round() as i64;
            v.top = (v.top as i64 + rows).clamp(0, max_top as i64) as u64;
        }
        // consumed, so the editor (which may have the keyboard focus) does not scroll as well
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::PageDown) {
                v.top = (v.top + vis).min(max_top);
            }
            if i.consume_key(egui::Modifiers::NONE, Key::PageUp) {
                v.top = v.top.saturating_sub(vis);
            }
            if i.consume_key(egui::Modifiers::COMMAND, Key::Home) {
                v.top = 0;
            }
            if i.consume_key(egui::Modifiers::COMMAND, Key::End) {
                v.top = max_top;
            }
        });
    }
    // scrollbar
    let track = Rect::from_min_max(Pos2::new(rect.right() - sb_w, rect.top()), rect.max);
    let thumb_h = (track.height() * vis as f32 / b.lines.max(1) as f32).clamp(24.0, track.height());
    let frac = if max_top == 0 { 0.0 } else { v.top as f64 / max_top as f64 };
    let thumb_y = track.top() + (track.height() - thumb_h) * frac as f32;
    let thumb = Rect::from_min_size(Pos2::new(track.left() + 2.0, thumb_y), Vec2::new(sb_w - 4.0, thumb_h));
    let press_in_track = ui.input(|i| i.pointer.press_origin()).map(|o| track.expand(3.0).contains(o)).unwrap_or(false);
    if let Some(pp) = resp.interact_pointer_pos() {
        if (resp.dragged() || resp.clicked()) && press_in_track {
            let f = ((pp.y - track.top() - thumb_h / 2.0) / (track.height() - thumb_h).max(1.0)).clamp(0.0, 1.0);
            v.top = (f as f64 * max_top as f64).round() as u64;
        } else if resp.clicked() && body.contains(pp) {
            let r = v.top + ((pp.y - body.top()) / row_h) as u64;
            if r < b.lines {
                v.sel = Some(r);
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(pp) = resp.interact_pointer_pos() {
            if body.contains(pp) {
                let r = v.top + ((pp.y - body.top()) / row_h) as u64;
                if r < b.lines {
                    v.sel = Some(r);
                }
            }
        }
    }
    let sel_line = v.sel.and_then(|r| b.line(r)).map(|l| String::from_utf8_lossy(l).into_owned());
    resp.context_menu(|ui| {
        if ui.add_enabled(split_action.is_some(), egui::Button::new("Save as *Include file…")).clicked() {
            if let Some(a) = &split_action {
                acts.push(a.clone());
            }
            ui.close();
        }
        if ui.button(if can_expand(&b) { "Edit as text" } else { "Edit as text… (too large)" }).clicked() {
            acts.push(Action::ExpandBulk { bulk: b.id });
            ui.close();
        }
        if let Some(l) = &sel_line {
            if ui.button("Copy line").clicked() {
                ui.ctx().copy_text(l.clone());
                ui.close();
            }
        }
    });
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, crate::theme::t(Color32::WHITE));
    p.rect_filled(track, 0.0, crate::theme::rgb(243, 244, 246));
    p.rect_filled(thumb, 4.0, crate::theme::rgb(190, 195, 205));

    // rows
    let rows: Vec<(u64, Vec<String>)> = (v.top..(v.top + vis + 1).min(b.lines))
        .map(|r| {
            let l = b.line(r).unwrap_or(&[]);
            let s = String::from_utf8_lossy(l);
            let mut fields: Vec<String> = s.split(',').map(|f| f.trim().to_string()).collect();
            if fields.last().map(|f| f.is_empty()).unwrap_or(false) && s.trim_end().ends_with(',') {
                fields.pop();
                if let Some(last) = fields.last_mut() {
                    last.push(',');
                }
            }
            (r, fields)
        })
        .collect();
    for (_, f) in &rows {
        for (i, x) in f.iter().enumerate() {
            if v.widths.len() <= i {
                v.widths.push(0);
            }
            v.widths[i] = v.widths[i].max(x.chars().count());
        }
    }
    let num_w = util::fmt_int(real0 + b.lines).len().max(5);
    let gutter = char_w * num_w as f32 + 16.0;
    p.rect_filled(Rect::from_min_max(body.min, Pos2::new(body.left() + gutter, body.bottom())), 0.0, crate::theme::t(crate::editor::GUTTER_BG));
    for (k, (r, fields)) in rows.iter().enumerate() {
        let y = body.top() + k as f32 * row_h;
        if v.sel == Some(*r) {
            p.rect_filled(Rect::from_min_size(Pos2::new(body.left(), y), Vec2::new(body.width(), row_h)), 0.0, crate::theme::rgb(255, 244, 196));
        } else if r % 2 == 1 {
            p.rect_filled(Rect::from_min_size(Pos2::new(body.left() + gutter, y), Vec2::new(body.width() - gutter, row_h)), 0.0, crate::theme::rgb(250, 251, 253));
        }
        p.text(Pos2::new(body.left() + gutter - 8.0, y + row_h / 2.0), egui::Align2::RIGHT_CENTER, util::fmt_int(real0 + r), FontId::monospace(font_size * 0.88), crate::theme::rgb(160, 164, 172));
        let mut x = body.left() + gutter + 8.0;
        for (i, f) in fields.iter().enumerate() {
            let w = v.widths.get(i).copied().unwrap_or(1).max(1) as f32 * char_w;
            let is_num = util::is_number(f.trim_end_matches(','));
            let color = if i == 0 { crate::theme::rgb(0, 84, 200) } else if is_num { crate::theme::rgb(40, 40, 46) } else { crate::theme::rgb(0, 120, 110) };
            if is_num {
                p.text(Pos2::new(x + w, y + row_h / 2.0), egui::Align2::RIGHT_CENTER, f, font.clone(), color);
            } else {
                p.text(Pos2::new(x, y + row_h / 2.0), egui::Align2::LEFT_CENTER, f, font.clone(), color);
            }
            x += w;
            if i + 1 < fields.len() {
                p.text(Pos2::new(x + 1.0, y + row_h / 2.0), egui::Align2::LEFT_CENTER, ",", font.clone(), crate::theme::rgb(170, 170, 170));
            }
            x += char_w * 2.5;
        }
    }
    p.line_segment([Pos2::new(body.left() + gutter, body.top()), Pos2::new(body.left() + gutter, body.bottom())], Stroke::new(1.0_f32, crate::theme::rgb(228, 230, 235)));
    if !index_ready {
        let r = Rect::from_min_size(Pos2::new(body.right() - 330.0, body.top() + 6.0), Vec2::new(320.0, 24.0));
        p.rect_filled(r, 4.0, crate::theme::rgb(255, 248, 220));
        p.text(r.center(), egui::Align2::CENTER_CENTER, "Indexing lines of this block… (one-time)", FontId::proportional(12.5), crate::theme::rgb(120, 90, 0));
    }
}
