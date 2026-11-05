//! The text editor: syntax-highlighted skeleton text with a real-line-number gutter,
//! completion popup, hover help, Ctrl+click navigation and a find bar.

use crate::actions::Action;
use crate::analysis::{Ctx, DefKind};
use crate::schema::SCHEMA;
use crate::store::{placeholder_id, STORE};
use crate::syntax::{self, keyword_context, line_kind, parse_keyword, parse_params, LineKind, Palette, Sev};
use crate::tab::{CItem, Completion, DocTab, RefItem};
use crate::util::{self, norm, title_case};
use egui::{Color32, FontId, Key, Modifiers, Pos2, Sense, Vec2};

pub const GUTTER_BG: Color32 = Color32::from_rgb(247, 248, 250);

pub fn editor_ui(ui: &mut egui::Ui, tab: &mut DocTab, font_size: f32, acts: &mut Vec<Action>) {
    let font = FontId::monospace(font_size);
    tab.ensure_map();
    if tab.find.open {
        find_bar(ui, tab);
    }
    let edit_id = tab.edit_id;
    let has_focus = ui.memory(|m| m.has_focus(edit_id));

    // ---------- completion keys (consumed before the editor sees them)
    let mut comp_move: i32 = 0;
    let mut comp_accept = false;
    if tab.completion.is_some() && has_focus {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                comp_move = 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                comp_move = -1;
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                comp_accept = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                comp_move = 99;
            }
        });
    }
    if comp_move == 99 {
        tab.completion = None;
    } else if let Some(c) = tab.completion.as_mut() {
        if comp_move != 0 && !c.items.is_empty() {
            let n = c.items.len() as i32;
            c.sel = ((c.sel as i32 + comp_move).rem_euclid(n)) as usize;
        }
    }
    let manual_complete = has_focus && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space));
    if comp_accept {
        accept_completion(tab);
    }

    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let digits = util::fmt_int(tab.map.total.max(1)).len().max(4);
    let gutter_w = char_w * 0.88 * digits as f32 + 22.0;
    let an = tab.analysis.clone().filter(|a| a.ver == tab.doc.text.ver);
    let (ext, ext_key): (&[syntax::Diag], u64) = match an.as_deref() {
        Some(a) => (&a.diags, a.ver),
        None => (&[], 0),
    };
    let out = crate::codeview::show(ui, tab, &font, ext, ext_key, gutter_w);
    if tab.rect.is_some() {
        tab.completion = None;
    }
    if let Some(m) = tab.notice.take() {
        acts.push(Action::Toast(m));
    }

    // ---------- completion trigger
    let changed = out.changed;
    if changed || manual_complete || (tab.completion.is_some() && tab.completion.as_ref().map(|c| c.ver) != Some(tab.doc.text.ver)) {
        let typed_trigger = changed && {
            let b = tab.cursor.2;
            let prev = tab.doc.text.s[..b].chars().last();
            matches!(prev, Some(c) if c.is_ascii_alphanumeric() || matches!(c, '*' | '=' | ',' | '_' | '-' | '<' | ' '))
        };
        if typed_trigger || manual_complete || tab.completion.is_some() {
            let comp = compute_completion(tab, manual_complete);
            tab.completion = comp;
        }
    }
    if tab.completion.is_some() && !changed && !manual_complete {
        // close when the cursor moved away from the replaced token
        if let Some(c) = &tab.completion {
            if tab.cursor.2 < c.replace.0 || tab.cursor.2 > c.replace.1 + 1 || tab.sel.0 != tab.sel.1 {
                tab.completion = None;
            }
        }
    }
    if tab.completion.is_some() {
        if show_completion(ui, tab, &font) {
            accept_completion(tab);
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
    }

    // ---------- Ctrl+click: go to definition / open include / open data block
    let ctrl = ui.input(|i| i.modifiers.command);
    if out.response.clicked() && ctrl {
        go_to_definition(tab, acts);
    } else if out.response.clicked() && tab.sel.0 == tab.sel.1 {
        // single click on a placeholder line opens the data viewer
        let line = tab.cursor.0;
        if let Some((id, _)) = placeholder_id(tab.line_text(line)) {
            acts.push(Action::OpenViewer { bulk: id, row: None });
        }
    }
    // Shift+F12 first: egui ignores an extra Shift when matching plain F12
    let (sf12, f12) = if has_focus { ui.input_mut(|i| (i.consume_key(Modifiers::SHIFT, Key::F12), i.consume_key(Modifiers::NONE, Key::F12))) } else { (false, false) };
    // comments: Ctrl+Q toggle, Ctrl+K comment out, Ctrl+Shift+K uncomment (as in Notepad++)
    if has_focus {
        let (unc, com, tog) = ui.input_mut(|i| {
            let unc = i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::K);
            (unc, i.consume_key(Modifiers::COMMAND, Key::K), i.consume_key(Modifiers::COMMAND, Key::Q))
        });
        let mode = if unc {
            Some(crate::comment::Mode::Uncomment)
        } else if com {
            Some(crate::comment::Mode::Comment)
        } else if tog {
            Some(crate::comment::Mode::Toggle)
        } else {
            None
        };
        if let Some(mode) = mode {
            acts.push(Action::Comment { ranges: comment_target(tab), mode });
        }
    }
    if f12 {
        go_to_definition(tab, acts);
    }
    if sf12 {
        find_references(tab, acts);
    }

    // ---------- right-click menu
    if out.response.secondary_clicked() {
        if let Some(pos) = out.response.interact_pointer_pos() {
            let b = tab.geom.byte_at(tab, pos);
            let (a, z) = (tab.sel.0.min(tab.sel.1), tab.sel.0.max(tab.sel.1));
            if !(a != z && b >= a && b <= z) {
                set_cursor(tab, b, b);
            }
            tab.ensure_map();
            let line = tab.map.line_of_byte(b);
            tab.cursor = (line, b - tab.map.starts[line], tab.sel.1);
            tab.completion = None;
        }
    }
    out.response.context_menu(|ui| editor_menu(ui, tab, acts));
    let menu_open = out.response.context_menu_opened();

    // ---------- hover help (not while the right-click menu is open)
    if menu_open {
    } else if let Some(fb) = tab.force_hover {
        let r = tab.geom.pos_of(tab, fb);
        if let Some(content) = hover_content(tab, fb) {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), edit_id.with("hover"), egui::PopupAnchor::Position(r + Vec2::new(0.0, tab.geom.row_h + 4.0))).gap(4.0).show(|ui| {
                ui.set_max_width(460.0);
                content(ui);
            });
        }
    } else if let Some(pos) = out.response.hover_pos() {
        if ui.input(|i| i.pointer.is_still() && !i.pointer.any_down()) {
            let b = tab.geom.byte_at(tab, pos);
            if let Some(content) = hover_content(tab, b) {
                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), edit_id.with("hover"), egui::PopupAnchor::Pointer).gap(14.0).show(|ui| {
                    ui.set_max_width(460.0);
                    content(ui);
                });
            }
        }
    }
}

fn accept_completion(tab: &mut DocTab) {
    if let Some(c) = tab.completion.take() {
        if let Some(it) = c.items.get(c.sel) {
            let (a, b) = c.replace;
            if b <= tab.doc.text.s.len() && a <= b && tab.doc.text.s.is_char_boundary(a) && tab.doc.text.s.is_char_boundary(b) {
                tab.doc.text.replace_range(a..b, &it.insert);
                tab.doc.text.break_group();
                let pos = a + it.cursor.unwrap_or(it.insert.len());
                set_cursor(tab, pos, pos);
                tab.scroll_req = Some((pos, false));
                tab.ensure_map();
            }
        }
    }
}

/// Right-click menu of the editor.
fn editor_menu(ui: &mut egui::Ui, tab: &mut DocTab, acts: &mut Vec<Action>) {
    ui.set_min_width(250.0);
    if let Some(a) = selection_split_action(tab) {
        if ui.button("Save selection as *Include file…").on_hover_text("The selected lines (whole lines, any number of blocks) go into a new file").clicked() {
            acts.push(a);
            ui.close();
        }
        ui.separator();
    }
    let line = tab.cursor.0;
    let lt = tab.line_text(line).to_string();
    let stem = tab.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string();
    let split = tab.current_analysis().and_then(|an| an.block_at_line(line).map(|bi| crate::outline::block_split_action(an, bi, &stem)));
    let block_norm = tab.current_analysis().and_then(|an| an.block_at_line(line).map(|bi| an.blocks[bi].norm.clone())).unwrap_or_default();
    if let Some((id, _)) = placeholder_id(&lt) {
        if let Some(bk) = STORE.get(id) {
            ui.label(egui::RichText::new(format!("{} · {} lines · {}", bk.what(), util::fmt_int(bk.lines), util::fmt_bytes(bk.size()))).small().weak());
            if ui.add_enabled(split.is_some(), egui::Button::new("Save as *Include file…")).on_disabled_hover_text("Waiting for the analysis…").clicked() {
                acts.push(split.clone().unwrap());
                ui.close();
            }
            if ui.button("Open in data viewer").clicked() {
                acts.push(Action::OpenViewer { bulk: id, row: None });
                ui.close();
            }
            let small = crate::viewer::can_expand(&bk);
            if ui.button(if small { "Edit as text" } else { "Edit as text… (too large)" }).clicked() {
                acts.push(Action::ExpandBulk { bulk: id });
                ui.close();
            }
            ui.separator();
        }
    } else if split.is_some() && !block_norm.starts_with("END") {
        if block_norm == "INCLUDE" {
            if ui.button("Open include file").clicked() {
                go_to_definition(tab, acts);
                ui.close();
            }
        } else if ui.button("Save block as *Include file…").clicked() {
            acts.push(split.clone().unwrap());
            ui.close();
        }
        ui.separator();
    }
    let many = tab.sel.0 != tab.sel.1;
    let what = if many { "selected lines" } else { "line" };
    if ui.add(egui::Button::new(format!("Comment out {what} (**)")).shortcut_text("Ctrl+K")).clicked() {
        acts.push(Action::Comment { ranges: comment_target(tab), mode: crate::comment::Mode::Comment });
        ui.close();
    }
    if ui.add(egui::Button::new(format!("Uncomment {what}")).shortcut_text("Ctrl+Shift+K")).clicked() {
        acts.push(Action::Comment { ranges: comment_target(tab), mode: crate::comment::Mode::Uncomment });
        ui.close();
    }
    ui.separator();
    if ui.add(egui::Button::new("Go to definition").shortcut_text("F12")).clicked() {
        go_to_definition(tab, acts);
        ui.close();
    }
    if ui.add(egui::Button::new("Find references").shortcut_text("Shift+F12")).clicked() {
        find_references(tab, acts);
        ui.close();
    }
}

/// "Save as *Include file" for the selected text, extended to whole lines.
pub fn selection_split_action(tab: &mut DocTab) -> Option<Action> {
    let (a, b) = (tab.sel.0.min(tab.sel.1), tab.sel.0.max(tab.sel.1));
    if a == b {
        return None;
    }
    tab.ensure_map();
    let s = &tab.doc.text.s;
    let l0 = tab.map.line_of_byte(a);
    let mut l1 = tab.map.line_of_byte(b);
    if tab.map.starts[l1] == b && l1 > l0 {
        l1 -= 1; // selection ends at the start of a line: that line is not included
    }
    let mut start = tab.map.starts[l0];
    let mut end = tab.map.starts.get(l1 + 1).copied().unwrap_or(s.len());
    // keep *Part / *Step / … and their *End lines together
    let mut notes = Vec::new();
    if let Some(an) = tab.current_analysis() {
        let (a2, b2, n2) = crate::outline::balance_span(an, start, end);
        if (a2, b2) != (start, end) && b2 > a2 {
            start = a2;
            end = b2;
            notes = n2;
        }
    }
    let l0 = tab.map.line_of_byte(start);
    let l1 = tab.map.line_of_byte(end.saturating_sub(1).max(start));
    let n = l1 - l0 + 1;
    let real: u64 = (l0..=l1).map(|l| tab.map.real.get(l + 1).copied().unwrap_or(tab.map.total + 1) - tab.map.real[l]).sum();
    let stem = tab.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string();
    let mut label = if real as usize != n {
        format!("the selected {} lines ({} lines in the file, data blocks included)", util::fmt_int(n as u64), util::fmt_int(real))
    } else {
        format!("the selected {} lines", util::fmt_int(n as u64))
    };
    if !notes.is_empty() {
        label.push_str(&format!(" — {}", notes.join("; ")));
    }
    Some(Action::SplitBlock { ranges: vec![(start, end)], suggested: format!("{stem}_selection.inp"), label })
}

/// Byte ranges whose lines a comment command works on: the selection (rectangular selections:
/// their lines) or the cursor line.
pub fn comment_target(tab: &DocTab) -> Vec<(usize, usize)> {
    if let Some(r) = &tab.rect {
        let (l0, l1) = (r.anchor.0.min(r.head.0), r.anchor.0.max(r.head.0));
        let s = tab.map.starts.get(l0).copied().unwrap_or(0);
        let e = tab.map.starts.get(l1).map(|x| x + 1).unwrap_or(s).min(tab.doc.text.s.len()).max(s);
        return vec![(s, e)];
    }
    vec![(tab.sel.0.min(tab.sel.1), tab.sel.0.max(tab.sel.1))]
}

pub fn set_cursor(tab: &mut DocTab, a: usize, b: usize) {
    tab.sel = (a, b);
    crate::codeview::clamp_sel(tab);
    tab.pref_col = None;
}

// ------------------------------------------------------------------ completion

fn word_start(s: &str, b: usize, extra: &[char]) -> usize {
    let mut i = b;
    while i > 0 {
        let c = s[..i].chars().last().unwrap();
        if c.is_ascii_alphanumeric() || extra.contains(&c) {
            i -= c.len_utf8();
        } else {
            break;
        }
    }
    i
}

fn defs_named(tab: &DocTab, kind: DefKind) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(a) = &tab.analysis {
        for d in &a.defs {
            if d.kind == kind && !v.contains(&d.disp) {
                v.push(d.disp.clone());
            }
        }
    }
    v.sort_by_key(|s| s.to_ascii_uppercase());
    v
}

fn param_kind(p: &str) -> Option<DefKind> {
    Some(match p {
        "ELSET" => DefKind::Elset,
        "NSET" | "REFNODE" | "TIENSET" | "PINNSET" => DefKind::Nset,
        "MATERIAL" => DefKind::Material,
        "AMPLITUDE" => DefKind::Amplitude,
        "INTERACTION" => DefKind::Interaction,
        "SURFACE" => DefKind::Surface,
        "PART" => DefKind::Part,
        "INSTANCE" => DefKind::Instance,
        "ORIENTATION" => DefKind::Orientation,
        "CONTROLS" => DefKind::SectionControls,
        _ => return None,
    })
}

/// Completion at a byte offset (as if Ctrl+Space was pressed there).
pub fn complete_at(tab: &DocTab, _b: usize) -> Option<Completion> {
    compute_completion(tab, true)
}

fn compute_completion(tab: &DocTab, manual: bool) -> Option<Completion> {
    let s = &tab.doc.text.s;
    let b = tab.cursor.2.min(s.len());
    let line = tab.cursor.0;
    let ls = tab.map.starts.get(line).copied().unwrap_or(0).min(b);
    let prefix = &s[ls..b];
    let ver = tab.doc.text.ver;
    let t = prefix.trim_start();
    let mk = |items: Vec<CItem>, a: usize| -> Option<Completion> {
        if items.is_empty() {
            None
        } else {
            Some(Completion { items, sel: 0, replace: (a, b), ver, manual })
        }
    };
    // <parameter>
    if let Some(lt) = prefix.rfind('<') {
        if !prefix[lt..].contains('>') {
            let typed = prefix[lt + 1..].to_ascii_uppercase();
            let items = defs_named(tab, DefKind::Param)
                .into_iter()
                .filter(|n| n.to_ascii_uppercase().starts_with(&typed))
                .map(|n| CItem { label: format!("<{n}>"), insert: format!("{n}>"), detail: "parameter".into(), cursor: None })
                .take(50)
                .collect();
            return mk(items, ls + lt + 1);
        }
    }
    // keyword name
    if t.starts_with('*') && !t.starts_with("**") && !t.contains(',') {
        let typed = &t[1..];
        if typed.is_empty() && !manual {
            return None;
        }
        let a = b - typed.len();
        let items: Vec<CItem> = SCHEMA
            .complete(&norm(typed))
            .into_iter()
            .take(60)
            .map(|k| {
                let mut ins = title_case(&k.name);
                let mut cursor = None;
                if let Some(r) = k.required.first() {
                    let p = k.param(r).map(|p| p.name.to_ascii_lowercase()).unwrap_or_else(|| r.to_ascii_lowercase());
                    ins.push_str(&format!(", {p}="));
                    cursor = Some(ins.len());
                }
                CItem { label: format!("*{}", title_case(&k.name)), insert: ins, detail: k.summary.clone(), cursor }
            })
            .collect();
        // don't pop up if the only candidate is exactly what's typed
        if !manual && items.len() == 1 && norm(typed) == norm(&items[0].insert) {
            return None;
        }
        return mk(items, a);
    }
    // keyword parameters / values (keyword line or its continuation)
    let kind = line_kind(tab.line_text(line));
    let kwctx = if kind == LineKind::Keyword {
        parse_keyword(tab.line_text(line)).map(|k| (line, k))
    } else if kind == LineKind::Data && line > 0 {
        // continuation of a keyword line?
        let prev = tab.line_text(line - 1);
        if matches!(line_kind(prev), LineKind::Keyword | LineKind::Data) && prev.trim_end().ends_with(',') {
            keyword_context(s, &tab.map.starts, line - 1).filter(|(kl, _, k)| {
                // every line between keyword and here must end with a comma
                (*kl..line).all(|l| tab.line_text(l).trim_end().ends_with(',')) && !k.norm.is_empty()
            }).map(|(kl, _, k)| (kl, k))
        } else {
            None
        }
    } else {
        None
    };
    if let Some((_, kp)) = kwctx {
        let def = SCHEMA.get(&kp.norm)?;
        // current item after last comma (outside quotes)
        let mut inq = false;
        let mut last_comma = None;
        for (i, c) in prefix.char_indices() {
            if c == '"' {
                inq = !inq;
            } else if c == ',' && !inq {
                last_comma = Some(i);
            }
        }
        if kind == LineKind::Keyword && last_comma.is_none() {
            return None;
        }
        let item_start = last_comma.map(|c| c + 1).unwrap_or(0);
        let item = &prefix[item_start..];
        if let Some(eq) = item.find('=') {
            let pname = norm(&item[..eq]);
            let typed = item[eq + 1..].trim_start();
            let a = b - typed.len();
            let pd = def.param(&pname)?;
            let mut items: Vec<CItem> = pd
                .values
                .iter()
                .filter(|v| norm(v).starts_with(&norm(typed)))
                .map(|v| CItem { label: v.clone(), insert: v.clone(), detail: format!("{} value", pd.name), cursor: None })
                .collect();
            if items.is_empty() {
                if let Some(k) = param_kind(&pname) {
                    let k = if kp.norm == "INSTANCE" && pname == "PART" { DefKind::Part } else { k };
                    items = defs_named(tab, k)
                        .into_iter()
                        .filter(|n| n.to_ascii_uppercase().starts_with(&typed.to_ascii_uppercase()))
                        .map(|n| CItem { label: n.clone(), insert: n, detail: k.label().to_string(), cursor: None })
                        .take(80)
                        .collect();
                }
            }
            return mk(items, a);
        }
        let typed = item.trim_start();
        let a = b - typed.len();
        let present: Vec<String> = kp.params.iter().map(|p| p.norm.clone()).collect();
        let items: Vec<CItem> = def
            .params
            .iter()
            .filter(|p| !present.contains(&p.norm) || p.norm == norm(typed))
            .filter(|p| p.norm.starts_with(&norm(typed)))
            .map(|p| {
                let mut ins = p.name.to_ascii_lowercase();
                if !p.flag {
                    ins.push('=');
                }
                let lead = if typed.is_empty() && !item.starts_with(' ') { " " } else { "" };
                CItem {
                    label: p.name.to_ascii_lowercase(),
                    insert: format!("{lead}{ins}"),
                    detail: if def.required.contains(&p.norm) { format!("required · {}", first_sentence(&p.doc)) } else { first_sentence(&p.doc) },
                    cursor: None,
                }
            })
            .collect();
        let mut items = items;
        items.sort_by_key(|i| (!i.detail.starts_with("required"), i.label.clone()));
        return mk(items, a);
    }
    // data lines: set / surface names in the first field of loads & BCs
    if kind == LineKind::Data && !prefix.contains(',') {
        let (_, _, k) = keyword_context(s, &tab.map.starts, line)?;
        let dk = match k.norm.as_str() {
            "BOUNDARY" | "CLOAD" | "TEMPERATURE" => DefKind::Nset,
            "DLOAD" | "FILM" => DefKind::Elset,
            "DSLOAD" | "TIE" | "CONTACTPAIR" | "DSFLUX" | "SFILM" => DefKind::Surface,
            _ => return None,
        };
        let typed = prefix.trim_start();
        if typed.is_empty() && !manual {
            return None;
        }
        let a = word_start(s, b, &['_', '-', '.']);
        let tu = s[a..b].to_ascii_uppercase();
        let items = defs_named(tab, dk)
            .into_iter()
            .filter(|n| n.to_ascii_uppercase().starts_with(&tu))
            .map(|n| CItem { label: n.clone(), insert: n, detail: dk.label().to_string(), cursor: None })
            .take(80)
            .collect();
        return mk(items, a);
    }
    None
}

fn first_sentence(s: &str) -> String {
    let t = s.split(". ").next().unwrap_or(s);
    let mut t = t.to_string();
    if t.len() > 110 {
        let cut = util::floor_char_boundary(&t, 107);
        t.truncate(cut);
        t.push('…');
    }
    t
}

/// Draw the completion popup; clicking an item selects it, double-clicking (or Enter/Tab) inserts it.
/// Returns true when the selected item should be inserted.
fn show_completion(ui: &egui::Ui, tab: &mut DocTab, font: &FontId) -> bool {
    let Some(c) = tab.completion.as_ref() else { return false };
    let r = tab.geom.pos_of(tab, c.replace.0);
    let pos = Pos2::new(r.x - 4.0, r.y + tab.geom.row_h + 2.0);
    let n = c.items.len();
    let start = c.sel.saturating_sub(5).min(n.saturating_sub(12));
    let mut clicked: Option<usize> = None;
    let mut accept = false;
    let mut wheel: i32 = 0;
    egui::Area::new(tab.edit_id.with("completion")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
            ui.set_min_width(380.0);
            ui.set_max_width(560.0);
            for (i, it) in c.items.iter().enumerate().skip(start).take(12) {
                let sel = i == c.sel;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), font.size + 8.0), Sense::click());
                if sel {
                    ui.painter().rect_filled(rect, 3.0, crate::theme::rgb(214, 228, 252));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, crate::theme::rgb(236, 240, 247));
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
                if resp.double_clicked() {
                    clicked = Some(i);
                    accept = true;
                }
                ui.painter().text(rect.left_center() + Vec2::new(6.0, 0.0), egui::Align2::LEFT_CENTER, &it.label, font.clone(), crate::theme::t(Palette::KEYWORD));
                let lw = ui.fonts_mut(|f| f.layout_no_wrap(it.label.clone(), font.clone(), crate::theme::t(Color32::BLACK)).size().x);
                let room = (rect.width() - lw - 24.0).max(0.0);
                let mut d = it.detail.clone();
                let max_chars = (room / (font.size * 0.55)) as usize;
                if d.chars().count() > max_chars && max_chars > 3 {
                    d = d.chars().take(max_chars - 1).collect::<String>() + "…";
                } else if max_chars <= 3 {
                    d.clear();
                }
                ui.painter().text(
                    rect.right_center() - Vec2::new(6.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    d,
                    FontId::proportional(font.size * 0.85),
                    crate::theme::rgb(120, 124, 132),
                );
            }
            if n > 12 {
                ui.label(egui::RichText::new(format!("{} of {} · ↑↓ Enter / double-click", c.sel + 1, n)).small().weak());
            }
            if ui.ui_contains_pointer() {
                let dy = ui.input(|i| i.smooth_scroll_delta.y);
                if dy.abs() > 0.5 {
                    wheel = if dy > 0.0 { -1 } else { 1 };
                }
            }
        });
    });
    if let Some(c) = tab.completion.as_mut() {
        if let Some(i) = clicked {
            c.sel = i;
        } else if wheel != 0 && n > 0 {
            c.sel = (c.sel as i32 + wheel).clamp(0, n as i32 - 1) as usize;
        }
    }
    accept
}

// ------------------------------------------------------------------ navigation

/// Token under byte offset `b`: (start, end, text)
fn token_at(s: &str, b: usize) -> Option<(usize, usize)> {
    let is_tok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '<' | '>');
    let b = util::floor_char_boundary(s, b.min(s.len()));
    let mut a = b;
    while a > 0 {
        let c = s[..a].chars().last().unwrap();
        if is_tok(c) {
            a -= c.len_utf8();
        } else {
            break;
        }
    }
    let mut e = b;
    while e < s.len() {
        let c = s[e..].chars().next().unwrap();
        if is_tok(c) {
            e += c.len_utf8();
        } else {
            break;
        }
    }
    if a < e { Some((a, e)) } else { None }
}

/// Determine name + candidate kinds at the cursor.
fn name_at_cursor(tab: &DocTab, b: usize) -> Option<(String, Vec<DefKind>, Ctx)> {
    let s = &tab.doc.text.s;
    let (a, e) = token_at(s, b)?;
    let mut name = s[a..e].to_string();
    let line = tab.map.line_of_byte(a);
    let ctx = tab
        .current_analysis()
        .or(tab.analysis.as_deref())
        .and_then(|an| an.block_at_line(line).map(|bi| an.blocks[bi].ctx.clone()))
        .unwrap_or_default();
    if name.starts_with('<') && name.ends_with('>') {
        name = name[1..name.len() - 1].to_string();
        return Some((name, vec![DefKind::Param], ctx));
    }
    let lt = tab.line_text(line);
    let ls = tab.map.starts[line];
    let all = vec![DefKind::Nset, DefKind::Elset, DefKind::Surface, DefKind::Material, DefKind::Amplitude, DefKind::Interaction, DefKind::Orientation, DefKind::Instance, DefKind::Part, DefKind::Step];
    if line_kind(lt) == LineKind::Keyword || (line > 0 && tab.line_text(line - 1).trim_end().ends_with(',')) {
        // keyword (continuation) line: look at the parameter name left of '='
        let from = if line_kind(lt) == LineKind::Keyword { lt.find(',').map(|c| c + 1).unwrap_or(lt.len()) } else { 0 };
        let (ps, _, _, _) = parse_params(lt, from);
        for p in ps {
            if let Some((vs, ve)) = p.value {
                if ls + vs <= a && e <= ls + ve {
                    let n = name.trim_matches('"').to_string();
                    let k = param_kind(&p.norm).map(|k| vec![k]).unwrap_or(all.clone());
                    return Some((n, k, ctx));
                }
            }
        }
        return None;
    }
    Some((name.trim_matches('"').to_string(), all, ctx))
}

pub fn go_to_definition(tab: &mut DocTab, acts: &mut Vec<Action>) {
    let b = tab.cursor.2;
    let line = tab.cursor.0;
    let lt = tab.line_text(line).to_string();
    if let Some((id, _)) = placeholder_id(&lt) {
        acts.push(Action::OpenViewer { bulk: id, row: None });
        return;
    }
    if line_kind(&lt) == LineKind::Keyword {
        if let Some(kp) = parse_keyword(&lt) {
            if kp.norm == "INCLUDE" {
                if let Some(p) = kp.params.iter().find(|p| p.norm == "INPUT").and_then(|p| p.value_str(&lt)) {
                    let path = crate::analysis::resolve_include(&tab.doc.dir(), p);
                    acts.push(Action::OpenFile(path, None));
                    return;
                }
            }
        }
    }
    let Some((name, kinds, ctx)) = name_at_cursor(tab, b) else { return };
    let Some(an) = tab.analysis.clone() else { return };
    let defs = an.find_defs(&kinds, &name, &ctx);
    match defs.first() {
        Some(d) => {
            if let Some((p, l)) = &d.file {
                acts.push(Action::OpenFile(p.clone(), Some(*l)));
            } else {
                let (s, e) = (d.start, d.end);
                tab.jump_to_line(d.line, Some((s, e)));
            }
            if defs.len() > 1 {
                acts.push(Action::Toast(format!("'{}' is defined {} times; showing the first", name, defs.len())));
            }
        }
        None => acts.push(Action::Toast(format!("No definition found for '{name}'"))),
    }
}

pub fn find_references(tab: &mut DocTab, acts: &mut Vec<Action>) {
    let b = tab.cursor.2;
    let Some((name, kinds, ctx)) = name_at_cursor(tab, b) else {
        acts.push(Action::Toast("Find references: put the cursor on a name (set, surface, material, amplitude, instance, <parameter>…), e.g. the value of elset= or the first field of a *Boundary line.".into()));
        return;
    };
    let Some(an) = tab.analysis.clone() else {
        acts.push(Action::Toast("The model is still being analysed; try again in a moment.".into()));
        return;
    };
    let mut items = Vec::new();
    let defs = an.find_defs(&kinds, &name, &ctx);
    let kind = defs.first().map(|d| d.kind).unwrap_or(kinds[0]);
    for d in &defs {
        items.push(RefItem {
            label: format!("definition · {} '{}'", d.kind.label(), d.disp),
            line: d.line,
            start: d.start,
            end: d.end,
            file: d.file.clone(),
            is_def: true,
        });
    }
    for r in an.find_refs(kind, &name) {
        items.push(RefItem { label: format!("{} '{}'", r.kind.label(), r.disp), line: r.line, start: r.start, end: r.end, file: None, is_def: false });
    }
    tab.refs_title = format!("References to '{}' ({} found)", name, items.len());
    tab.refs = items;
    acts.push(Action::ShowRefs);
}

// ------------------------------------------------------------------ hover

type HoverFn = Box<dyn FnOnce(&mut egui::Ui)>;

fn hover_content(tab: &DocTab, b: usize) -> Option<HoverFn> {
    let s = &tab.doc.text.s;
    if b >= s.len() {
        return None;
    }
    let line = tab.map.line_of_byte(b);
    let lt = tab.line_text(line).to_string();
    let ls = tab.map.starts[line];
    // diagnostics under the pointer
    let hit = |d: &&syntax::Diag| d.start <= b && b < d.end.max(d.start + 1);
    let mut msgs: Vec<(Sev, String)> = if tab.view.key.0 == tab.doc.text.ver { tab.view.diags.iter().filter(hit).map(|d| (d.sev, d.msg.clone())).collect() } else { Vec::new() };
    if let Some(a) = tab.current_analysis() {
        let from = a.diags.partition_point(|d| d.start + 300 < b);
        msgs.extend(a.diags[from..].iter().take_while(|d| d.start <= b).filter(hit).map(|d| (d.sev, d.msg.clone())));
    }
    if let Some((id, _)) = placeholder_id(&lt) {
        let bk = STORE.get(id)?;
        return Some(Box::new(move |ui: &mut egui::Ui| {
            ui.label(egui::RichText::new(&bk.kw_line).monospace().strong());
            ui.label(format!("{} lines · {} kept on disk (not loaded into the editor)", util::fmt_int(bk.lines), util::fmt_bytes(bk.size())));
            ui.label(egui::RichText::new("Click to open in the data viewer").weak());
        }));
    }
    let mut kw_help: Option<(&'static crate::schema::KeywordDef, Option<String>)> = None;
    if line_kind(&lt) == LineKind::Keyword {
        if let Some(kp) = parse_keyword(&lt) {
            let star = lt.find('*').unwrap_or(0);
            let rel = b - ls;
            if let Some(def) = SCHEMA.get(&kp.norm) {
                if rel >= star && rel <= kp.name.1 {
                    kw_help = Some((def, None));
                } else {
                    for p in &kp.params {
                        if rel >= p.name.0 && rel < p.name.1 {
                            kw_help = Some((def, Some(p.norm.clone())));
                        }
                    }
                }
            }
        }
    }
    if msgs.is_empty() && kw_help.is_none() {
        return None;
    }
    Some(Box::new(move |ui: &mut egui::Ui| {
        for (sev, m) in &msgs {
            let c = if *sev == Sev::Error { crate::theme::t(Palette::ERR) } else { crate::theme::t(Palette::WARN) };
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("●").color(c));
                ui.label(m);
            });
        }
        if let Some((def, p)) = kw_help {
            if !msgs.is_empty() {
                ui.separator();
            }
            match p.and_then(|p| def.param(&p)) {
                Some(pd) => {
                    ui.label(egui::RichText::new(format!("{}  (*{})", pd.name, def.name)).monospace().strong().color(crate::theme::t(Palette::PARAM)));
                    if def.required.contains(&pd.norm) {
                        ui.label(egui::RichText::new("required").small().color(crate::theme::t(Palette::ERR)));
                    }
                    if !pd.values.is_empty() {
                        ui.label(egui::RichText::new(format!("values: {}", pd.values.join(", "))).small());
                    }
                    if !pd.doc.is_empty() {
                        ui.label(&pd.doc);
                    }
                }
                None => {
                    ui.label(egui::RichText::new(format!("*{}", def.name)).monospace().strong().color(crate::theme::t(Palette::KEYWORD)));
                    ui.label(&def.summary);
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(format!("{} · {} · Level: {}", def.product_label(), def.type_label(), def.level.join(", "))).small().weak());
                    if !def.required.is_empty() {
                        let r: Vec<String> = def.required.iter().map(|r| def.param(r).map(|p| p.name.clone()).unwrap_or(r.clone())).collect();
                        ui.label(egui::RichText::new(format!("Required: {}", r.join(", "))).small());
                    }
                    ui.label(egui::RichText::new(format!("Keywords Guide p.{}", def.page)).small().weak());
                }
            }
        }
    }))
}

// ------------------------------------------------------------------ find

fn find_bar(ui: &mut egui::Ui, tab: &mut DocTab) {
    let mut go: i32 = 0;
    ui.horizontal(|ui| {
        ui.label("Find");
        let r = ui.add(egui::TextEdit::singleline(&mut tab.find.query).desired_width(260.0).hint_text("text in the editor"));
        if tab.find.focus {
            r.request_focus();
            tab.find.focus = false;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            go = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
            r.request_focus();
        }
        ui.checkbox(&mut tab.find.case, "Aa");
        if ui.button("▲").on_hover_text("Previous (Shift+Enter)").clicked() {
            go = -1;
        }
        if ui.button("▼").on_hover_text("Next (Enter)").clicked() {
            go = 1;
        }
        let key = (tab.doc.text.ver, tab.find.query.clone(), tab.find.case);
        if tab.find.counted_for != key {
            tab.find.count = if tab.find.query.is_empty() {
                0
            } else if tab.find.case {
                tab.doc.text.s.matches(&tab.find.query).count()
            } else {
                tab.doc.text.s.to_ascii_lowercase().matches(&tab.find.query.to_ascii_lowercase()).count()
            };
            tab.find.counted_for = key;
        }
        ui.label(egui::RichText::new(format!("{} matches", tab.find.count)).weak());
        ui.label(egui::RichText::new("(data blocks on disk are not searched; use Go to ID)").small().weak());
        if ui.button("×").clicked() {
            tab.find.open = false;
        }
    });
    if go != 0 && !tab.find.query.is_empty() {
        let s = &tab.doc.text.s;
        let (hay, needle) = if tab.find.case { (s.clone(), tab.find.query.clone()) } else { (s.to_ascii_lowercase(), tab.find.query.to_ascii_lowercase()) };
        let cur = tab.cursor.2;
        let pos = if go > 0 {
            hay[(cur + 1).min(hay.len())..].find(&needle).map(|p| p + cur + 1).or_else(|| hay.find(&needle))
        } else {
            hay[..cur.min(hay.len())].rfind(&needle).or_else(|| hay.rfind(&needle))
        };
        if let Some(p) = pos {
            let line = tab.map.line_of_byte(p);
            tab.jump = Some(crate::tab::Jump { line, sel: Some((p, p + needle.len())), focus: false });
        }
    }
    ui.separator();
}
