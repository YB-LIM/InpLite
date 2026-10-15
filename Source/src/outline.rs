//! Outline panel: model tree (parts, assembly, materials, steps, ...) built by the analysis.

use crate::actions::Action;
use crate::analysis::{Analysis, NodeKind, OutlineNode};
use crate::syntax::Palette;
use crate::store::STORE;
use crate::tab::DocTab;
use crate::util;
use std::sync::Arc;
use egui::{Color32, FontId, Pos2, Rect, Sense, Vec2};

/// One visible row of the outline (cached; rebuilt only when the tree, the open state or the
/// filter changes, so a 50,000-entry outline costs nothing per frame).
pub struct FlatRow {
    depth: usize,
    /// child indices from the root to the node
    path: Vec<u32>,
    key: String,
    open: bool,
}

#[derive(Default)]
pub struct OutlineCache {
    key: (usize, u64, usize, String),
    rows: Vec<FlatRow>,
}

fn flatten(nodes: &[OutlineNode], depth: usize, prefix: &str, path: &mut Vec<u32>, tab: &DocTab, filter: &str, out: &mut Vec<FlatRow>) {
    for (i, n) in nodes.iter().enumerate() {
        if !filter.is_empty() && !matches_filter(n, filter) {
            continue;
        }
        let key = format!("{prefix}/{}#{}", n.label, if n.children.is_empty() { n.line } else { 0 });
        let default_open = depth == 0 && n.children.len() <= 8 && n.kind == NodeKind::Step;
        let open = if !filter.is_empty() { true } else { *tab.outline_open.get(&key).unwrap_or(&default_open) };
        path.push(i as u32);
        out.push(FlatRow { depth, path: path.clone(), key: key.clone(), open });
        if open && !n.children.is_empty() {
            flatten(&n.children, depth + 1, &key, path, tab, filter, out);
        }
        path.pop();
    }
}

fn node_at<'a>(root: &'a [OutlineNode], path: &[u32]) -> Option<&'a OutlineNode> {
    let mut list = root;
    let mut node = None;
    for &i in path {
        let n = list.get(i as usize)?;
        node = Some(n);
        list = &n.children;
    }
    node
}

fn matches_filter(n: &OutlineNode, f: &str) -> bool {
    n.label.to_ascii_lowercase().contains(f) || n.children.iter().any(|c| matches_filter(c, f))
}

fn kind_badge(k: NodeKind) -> (&'static str, Color32) {
    match k {
        NodeKind::Part => ("P", Color32::from_rgb(40, 110, 220)),
        NodeKind::Assembly => ("A", Color32::from_rgb(130, 70, 190)),
        NodeKind::Instance => ("I", Color32::from_rgb(0, 140, 140)),
        NodeKind::Material => ("M", Color32::from_rgb(215, 120, 20)),
        NodeKind::Interaction => ("C", Color32::from_rgb(150, 100, 60)),
        NodeKind::Step => ("S", Color32::from_rgb(40, 150, 70)),
        NodeKind::Include => ("→", Color32::from_rgb(90, 90, 200)),
        NodeKind::Bulk => ("≡", Color32::from_rgb(70, 110, 170)),
        NodeKind::Block => ("", Color32::from_rgb(170, 175, 185)),
    }
}

/// byte range covered by a node (container: up to and including its *END block)
pub fn node_range(an: &Analysis, n: &OutlineNode) -> Option<(usize, usize)> {
    let bi = n.block?;
    let mut last = bi;
    fn walk(n: &OutlineNode, last: &mut usize) {
        if let Some(b) = n.block {
            *last = (*last).max(b);
        }
        for c in &n.children {
            walk(c, last);
        }
    }
    walk(n, &mut last);
    let end_kw = match n.kind {
        NodeKind::Part => Some("ENDPART"),
        NodeKind::Assembly => Some("ENDASSEMBLY"),
        NodeKind::Instance => Some("ENDINSTANCE"),
        NodeKind::Step => Some("ENDSTEP"),
        _ => None,
    };
    if let Some(ek) = end_kw {
        if let Some(nb) = an.blocks.get(last + 1) {
            if nb.norm == ek {
                last += 1;
            }
        }
    }
    Some((an.blocks[bi].start, an.blocks[last].end))
}

/// Mesh of a part (`part_block` = index of its *PART block) or, with `None`, of the model level of a
/// flat input file: every *NODE / *ELEMENT block plus the *NSET / *ELSET blocks that directly
/// follow them. Adjacent blocks are merged; the result is in file order.
pub fn mesh_ranges(an: &Analysis, part_block: Option<usize>) -> Vec<(usize, usize)> {
    let idx = scope_blocks(an, part_block);
    let mut sel = Vec::new();
    let mut in_run = false;
    for &i in &idx {
        match an.blocks[i].norm.as_str() {
            "NODE" | "ELEMENT" => {
                sel.push(i);
                in_run = true;
            }
            "NSET" | "ELSET" if in_run => sel.push(i),
            _ => in_run = false,
        }
    }
    merge_blocks(an, &sel)
}

/// Blocks of a part (between *PART and *END PART) or, with `None`, of the model level of a flat
/// input (not inside a part, assembly, instance or step).
pub fn scope_blocks(an: &Analysis, part_block: Option<usize>) -> Vec<usize> {
    match part_block {
        Some(pb) => {
            let mut v = Vec::new();
            for i in pb + 1..an.blocks.len() {
                if matches!(an.blocks[i].norm.as_str(), "ENDPART" | "PART" | "ASSEMBLY" | "STEP") {
                    break;
                }
                v.push(i);
            }
            v
        }
        None => (0..an.blocks.len())
            .filter(|&i| {
                let c = &an.blocks[i].ctx;
                c.part.is_none() && c.instance.is_none() && !c.assembly && c.step.is_none()
            })
            .collect(),
    }
}

/// Byte ranges of the given blocks (sorted), adjacent ones merged.
pub fn merge_blocks(an: &Analysis, sel: &[usize]) -> Vec<(usize, usize)> {
    let mut sel = sel.to_vec();
    sel.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in sel {
        let b = &an.blocks[i];
        match out.last_mut() {
            Some(l) if l.1 == b.start => l.1 = b.end,
            _ => out.push((b.start, b.end)),
        }
    }
    out
}

/// Blocks under an outline node (leaves of a group, or the node's own block).
pub fn leaf_blocks(n: &OutlineNode, out: &mut Vec<usize>) {
    if n.children.is_empty() {
        if let Some(b) = n.block {
            out.push(b);
        }
    }
    for c in &n.children {
        leaf_blocks(c, out);
    }
}

/// "Save as *Include file" request for one keyword block (keyword line + data), with a file name
/// like `model_PLATE_Element_SOLID.inp` (name, elset, nset or type of the block).
pub fn block_split_action(an: &Analysis, bi: usize, stem: &str) -> Action {
    let b = &an.blocks[bi];
    let mut parts = vec![stem.to_string()];
    if let Some((i, _)) = &b.ctx.instance {
        parts.push(i.clone());
    } else if let Some(p) = &b.ctx.part {
        parts.push(p.clone());
    }
    parts.push(b.disp.trim_start_matches('*').replace(' ', ""));
    for k in ["NAME", "ELSET", "NSET", "TYPE"] {
        if let Some(v) = b.param(k) {
            parts.push(v.trim_matches('"').to_string());
            break;
        }
    }
    let ctx = b.ctx.label();
    let label = if ctx.is_empty() { b.label() } else { format!("{} ({ctx})", b.label()) };
    Action::SplitBlock { ranges: vec![(b.start, b.end)], suggested: format!("{}.inp", sanitize(&parts.join("_"))), label }
}

pub fn sanitize(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let t = t.trim_matches('_').to_string();
    let mut o = String::new();
    for c in t.chars() {
        if !(c == '_' && o.ends_with('_')) {
            o.push(c);
        }
    }
    if o.is_empty() { "block".into() } else { o }
}

pub fn outline_ui(ui: &mut egui::Ui, tab: &mut DocTab, acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("OUTLINE").small().strong().color(Color32::from_rgb(110, 115, 125)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if tab.analysis.as_ref().map(|a| a.ver) != Some(tab.doc.text.ver) {
                ui.spinner();
            }
        });
    });
    ui.add(egui::TextEdit::singleline(&mut tab.outline_filter).hint_text("Filter (part, set, step…)").desired_width(f32::INFINITY));
    ui.add_space(4.0);
    let Some(an) = tab.analysis.clone() else {
        ui.label("Analyzing…");
        return;
    };
    let filter = tab.outline_filter.trim().to_ascii_lowercase();
    let ckey = (Arc::as_ptr(&an) as usize, tab.outline_rev, tab.outline_open.len(), filter.clone());
    if tab.outline_cache.key != ckey {
        let mut rows = Vec::new();
        flatten(&an.outline, 0, "", &mut Vec::new(), tab, &filter, &mut rows);
        tab.outline_cache = OutlineCache { key: ckey, rows };
    }
    let cache = std::mem::take(&mut tab.outline_cache);
    let rows = &cache.rows;
    let cur_block = an.block_at_line(tab.cursor.0);
    let row_h = 22.0;
    let font = FontId::proportional(13.0);
    let small = FontId::proportional(11.5);
    let stem = tab.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string();
    let mut toggles: Vec<(String, bool)> = Vec::new();
    let mut jump: Option<usize> = None;
    egui::ScrollArea::vertical().id_salt(tab.edit_id.with("outline")).auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
        for r in &rows[range] {
            let Some(n) = node_at(&an.outline, &r.path) else { continue };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            let p = ui.painter();
            let selected = n.block.is_some() && n.block == cur_block && n.children.is_empty();
            if selected {
                p.rect_filled(rect, 3.0, Color32::from_rgb(220, 232, 252));
            } else if resp.hovered() {
                p.rect_filled(rect, 3.0, Color32::from_rgb(236, 239, 244));
            }
            let mut x = rect.left() + 4.0 + r.depth as f32 * 14.0;
            let cy = rect.center().y;
            let has_kids = !n.children.is_empty();
            let arrow_rect = Rect::from_center_size(Pos2::new(x + 6.0, cy), Vec2::splat(14.0));
            if has_kids {
                let c = Color32::from_rgb(120, 125, 135);
                let pts = if r.open {
                    vec![Pos2::new(x + 2.0, cy - 2.5), Pos2::new(x + 10.0, cy - 2.5), Pos2::new(x + 6.0, cy + 3.0)]
                } else {
                    vec![Pos2::new(x + 3.5, cy - 4.0), Pos2::new(x + 3.5, cy + 4.0), Pos2::new(x + 9.0, cy)]
                };
                p.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
            }
            x += 16.0;
            let (badge, col) = kind_badge(n.kind);
            if !badge.is_empty() {
                let br = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::new(15.0, 15.0));
                p.rect_filled(br, 3.0, col);
                p.text(br.center(), egui::Align2::CENTER_CENTER, badge, FontId::proportional(10.5), Color32::WHITE);
                x += 20.0;
            } else {
                p.circle_filled(Pos2::new(x + 4.0, cy), 2.0, col);
                x += 12.0;
            }
            let detail_w = if n.detail.is_empty() { 0.0 } else { ui.fonts_mut(|f| f.layout_no_wrap(n.detail.clone(), small.clone(), Color32::GRAY).size().x) + 10.0 };
            let max_w = (rect.right() - x - detail_w - 6.0).max(20.0);
            let label_color = if n.err { Palette::ERR } else { Color32::from_rgb(40, 44, 52) };
            let galley = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(n.label.clone(), font.clone(), label_color);
                job.wrap.max_width = max_w;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                f.layout_job(job)
            });
            p.galley(Pos2::new(x, cy - galley.size().y / 2.0), galley, label_color);
            if !n.detail.is_empty() {
                p.text(Pos2::new(rect.right() - 6.0, cy), egui::Align2::RIGHT_CENTER, &n.detail, small.clone(), Color32::from_rgb(140, 145, 155));
            }
            if resp.clicked() {
                let on_arrow = resp.interact_pointer_pos().map(|pp| arrow_rect.expand(3.0).contains(pp)).unwrap_or(false);
                if has_kids && (on_arrow || !r.open) {
                    toggles.push((r.key.clone(), !r.open));
                }
                if !on_arrow {
                    jump = Some(n.line);
                }
            }
            if resp.double_clicked() && has_kids {
                toggles.push((r.key.clone(), !r.open));
            }
            resp.context_menu(|ui| {
                if ui.button("Go to").clicked() {
                    jump = Some(n.line);
                    ui.close();
                }
                if n.block.is_none() && !n.children.is_empty() {
                    let mut leaves = Vec::new();
                    leaf_blocks(n, &mut leaves);
                    let lbl = n.label.trim_start_matches('*').to_string();
                    if ui.button(format!("Save all {} blocks into one *Include file…", util::fmt_int(leaves.len() as u64))).clicked() {
                        acts.push(Action::SplitBlock { ranges: merge_blocks(&an, &leaves), suggested: format!("{}_{}.inp", stem, sanitize(&lbl)), label: format!("all {} *{} blocks", leaves.len(), lbl) });
                        ui.close();
                    }
                    if ui.button("Save each block as its own *Include file…").clicked() {
                        acts.push(Action::SplitEach { blocks: leaves.clone(), title: format!("Save {} *{} blocks as *Include files", leaves.len(), lbl) });
                        ui.close();
                    }
                }
                if let Some(bi) = n.block {
                    let b = &an.blocks[bi];
                    if b.norm == "INCLUDE" {
                        if let Some(inc) = an.includes.iter().find(|i| i.line == b.line) {
                            if ui.add_enabled(inc.exists, egui::Button::new("Open include file")).clicked() {
                                acts.push(Action::OpenFile(inc.path.clone(), None));
                                ui.close();
                            }
                            if ui.add_enabled(inc.exists, egui::Button::new("Inline include here")).clicked() {
                                acts.push(Action::InlineInclude { start: b.start, end: b.end, path: inc.path.clone() });
                                ui.close();
                            }
                        }
                    } else if !b.norm.starts_with("END") {
                        if n.children.is_empty() {
                            if ui.button("Save as *Include file…").clicked() {
                                acts.push(block_split_action(&an, bi, &stem));
                                ui.close();
                            }
                        } else if let Some((s, e)) = node_range(&an, n) {
                            let lbl = n.label.split_whitespace().collect::<Vec<_>>().join(" ");
                            if ui.button("Save as *Include file…").clicked() {
                                acts.push(Action::SplitBlock { ranges: vec![(s, e)], suggested: format!("{}_{}.inp", stem, sanitize(&lbl)), label: lbl.clone() });
                                ui.close();
                            }
                        }
                        if n.kind == NodeKind::Part && !mesh_ranges(&an, Some(bi)).is_empty() && ui.button("Save this part's mesh as *Include file(s)…").clicked() {
                            acts.push(Action::SplitPartMesh { part_block: bi });
                            ui.close();
                        }
                    }
                    for &id in &b.bulks {
                        if ui.button("Open in data viewer").clicked() {
                            acts.push(Action::OpenViewer { bulk: id, row: None });
                            ui.close();
                        }
                        let small = STORE.get(id).map(|bk| crate::viewer::can_expand(&bk)).unwrap_or(false);
                        if ui.button(if small { "Edit data as text" } else { "Edit data as text… (too large)" }).clicked() {
                            acts.push(Action::ExpandBulk { bulk: id });
                            ui.close();
                        }
                    }
                    if let Some(name) = b.param("NAME").or(b.param("ELSET")).or(b.param("NSET")) {
                        if ui.button("Copy name").clicked() {
                            ui.ctx().copy_text(name.trim_matches('"').to_string());
                            ui.close();
                        }
                    }
                }
            });
        }
    });
    tab.outline_cache = cache;
    for (k, v) in toggles {
        tab.outline_open.insert(k, v);
        tab.outline_rev += 1;
    }
    if let Some(l) = jump {
        tab.jump_to_line(l, None);
    }
}
