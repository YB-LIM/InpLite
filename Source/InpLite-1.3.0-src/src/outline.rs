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
        NodeKind::Part => ("P", crate::theme::rgb(40, 110, 220)),
        NodeKind::Assembly => ("A", crate::theme::rgb(130, 70, 190)),
        NodeKind::Instance => ("I", crate::theme::rgb(0, 140, 140)),
        NodeKind::Material => ("M", crate::theme::rgb(215, 120, 20)),
        NodeKind::Interaction => ("C", crate::theme::rgb(150, 100, 60)),
        NodeKind::Step => ("S", crate::theme::rgb(40, 150, 70)),
        NodeKind::Include => ("→", crate::theme::rgb(90, 90, 200)),
        NodeKind::Bulk => ("≡", crate::theme::rgb(70, 110, 170)),
        NodeKind::Block => ("", crate::theme::rgb(170, 175, 185)),
        NodeKind::Commented => ("#", crate::theme::rgb(150, 160, 150)),
    }
}

/// Byte range of an outline row: a block, a container up to its *END block, a group of blocks,
/// or a commented-out region.
pub fn row_range(an: &Analysis, n: &OutlineNode) -> Option<(usize, usize)> {
    if let Some(r) = n.range {
        return Some(r);
    }
    match n.block {
        Some(bi) if n.children.is_empty() => Some((an.blocks[bi].start, an.blocks[bi].end)),
        Some(_) => node_range(an, n),
        None => {
            let mut leaves = Vec::new();
            leaf_blocks(n, &mut leaves);
            let s = leaves.iter().map(|&b| an.blocks[b].start).min()?;
            let e = leaves.iter().map(|&b| an.blocks[b].end).max()?;
            Some((s, e))
        }
    }
}

fn end_of(open: &str) -> Option<&'static str> {
    Some(match open {
        "PART" => "ENDPART",
        "ASSEMBLY" => "ENDASSEMBLY",
        "INSTANCE" => "ENDINSTANCE",
        "STEP" => "ENDSTEP",
        _ => return None,
    })
}

fn open_of(end: &str) -> Option<&'static str> {
    Some(match end {
        "ENDPART" => "PART",
        "ENDASSEMBLY" => "ASSEMBLY",
        "ENDINSTANCE" => "INSTANCE",
        "ENDSTEP" => "STEP",
        _ => return None,
    })
}

/// Make a byte range keep *Part / *Assembly / *Instance / *Step and their *End lines together:
/// an opening keyword pulls in its *End line (and everything up to it), an *End line whose
/// opening keyword is outside is left in the main file when it is at the edge of the range,
/// otherwise the range is extended back to its opening keyword. Returns the new range and what
/// was changed (for the user).
pub fn balance_span(an: &Analysis, mut s: usize, mut e: usize) -> (usize, usize, Vec<String>) {
    let mut notes: Vec<String> = Vec::new();
    let bl = &an.blocks;
    for _ in 0..64 {
        // blocks whose keyword line is inside the range
        let lo = bl.partition_point(|b| b.start < s);
        let hi = bl.partition_point(|b| b.start < e);
        let idx: Vec<usize> = (lo..hi).collect();
        if idx.is_empty() {
            break;
        }
        let mut stack: Vec<usize> = Vec::new();
        let mut change = false;
        for (k, &i) in idx.iter().enumerate() {
            let n = bl[i].norm.as_str();
            if end_of(n).is_some() {
                stack.push(i);
            } else if let Some(op) = open_of(n) {
                if stack.last().is_some_and(|&t| bl[t].norm == op) {
                    stack.pop();
                } else if stack.iter().any(|&t| bl[t].norm == op) {
                    // badly nested: close up to it
                    while let Some(t) = stack.pop() {
                        if bl[t].norm == op {
                            break;
                        }
                    }
                } else if k == 0 {
                    // the range starts with the *End line of something before it: leave it out
                    s = bl[i].end;
                    notes.push(format!("{} stays in the main file", bl[i].disp));
                    change = true;
                    break;
                } else if idx[k..].iter().all(|&j| open_of(&bl[j].norm).is_some()) {
                    // only *End lines from here on: leave them out
                    e = bl[i].start;
                    notes.push(format!("{} stays in the main file", bl[i].disp));
                    change = true;
                    break;
                } else {
                    // extend back to the opening keyword
                    let mut depth = 0;
                    let mut found = None;
                    for j in (0..idx[0]).rev() {
                        if bl[j].norm == n {
                            depth += 1;
                        } else if bl[j].norm == op {
                            if depth == 0 {
                                found = Some(j);
                                break;
                            }
                            depth -= 1;
                        }
                    }
                    if let Some(j) = found {
                        s = bl[j].start;
                        notes.push(format!("extended back to {}", bl[j].label()));
                        change = true;
                        break;
                    }
                }
            }
        }
        if change {
            continue;
        }
        // opening keywords without their *End line: extend to it
        if let Some(&o) = stack.first() {
            let op = bl[o].norm.as_str();
            let en = end_of(op).unwrap();
            let mut depth = 0;
            let mut found = None;
            for j in o + 1..bl.len() {
                if bl[j].norm == op {
                    depth += 1;
                } else if bl[j].norm == en {
                    if depth == 0 {
                        found = Some(j);
                        break;
                    }
                    depth -= 1;
                }
            }
            if let Some(j) = found {
                if bl[j].end > e {
                    e = bl[j].end;
                    notes.push(format!("extended to {} of {}", bl[j].disp, bl[o].label()));
                    continue;
                }
            }
        }
        break;
    }
    if e < s {
        e = s;
    }
    notes.dedup();
    (s, e, notes)
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
    let Some(fi) = idx.iter().position(|&i| is_mesh_kw(&an.blocks[i].norm)) else { return Vec::new() };
    let mut li = idx.iter().rposition(|&i| is_mesh_kw(&an.blocks[i].norm)).unwrap();
    // the sets right after the last mesh block belong to the mesh too
    while li + 1 < idx.len() && matches!(an.blocks[idx[li + 1]].norm.as_str(), "NSET" | "ELSET") {
        li += 1;
    }
    // one contiguous span keeps the order of everything (generation keywords, *SYSTEM, …):
    // an *INCLUDE is inserted textually, so the model stays exactly the same
    let span = &idx[fi..=li];
    if span.windows(2).all(|w| w[1] == w[0] + 1) {
        return vec![(an.blocks[span[0]].start, an.blocks[span[span.len() - 1]].end)];
    }
    merge_blocks(an, span)
}

/// Keywords that define nodes or elements.
pub fn is_mesh_kw(n: &str) -> bool {
    matches!(n, "NODE" | "NGEN" | "NFILL" | "NCOPY" | "NMAP" | "ELEMENT" | "ELGEN" | "ELCOPY")
}

/// Keywords whose meaning depends on their position relative to *NODE / *ELEMENT blocks, so the
/// mesh of a scope that contains them must not be split into separate files.
pub fn order_sensitive(an: &Analysis, scope: &[usize]) -> Option<&'static str> {
    let mut seen_elem = false;
    for &i in scope {
        let b = &an.blocks[i];
        match b.norm.as_str() {
            "SYSTEM" => return Some("*SYSTEM changes the coordinates of the *NODE lines after it"),
            "NGEN" | "NFILL" | "NCOPY" | "NMAP" => return Some("node generation (*NGEN, *NFILL, *NCOPY, *NMAP) uses nodes defined before it"),
            "ELGEN" | "ELCOPY" => return Some("element generation (*ELGEN, *ELCOPY) uses elements defined before it"),
            "ELEMENT" => seen_elem = true,
            "NODE" if seen_elem => return Some("*NODE blocks come after *ELEMENT blocks"),
            _ => {}
        }
    }
    None
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

/// Selected outline rows (anchor, head) as indices into the cached rows, and their keys (to
/// find them again after the tree was rebuilt).
#[derive(Default, Clone)]
pub struct OutlineSel {
    pub anchor: usize,
    pub head: usize,
    keys: (String, String),
    pub dragging: bool,
}

impl OutlineSel {
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// Byte range covered by rows `a..=b` of the outline (union of their ranges, in file order).
fn rows_span(an: &Analysis, rows: &[FlatRow], a: usize, b: usize) -> Option<(usize, usize)> {
    let mut lo = usize::MAX;
    let mut hi = 0;
    for r in rows.get(a..=b.min(rows.len().saturating_sub(1)))? {
        if let Some(n) = node_at(&an.outline, &r.path) {
            if let Some((s, e)) = row_range(an, n) {
                lo = lo.min(s);
                hi = hi.max(e);
            }
        }
    }
    if lo < hi { Some((lo, hi)) } else { None }
}

pub fn outline_ui(ui: &mut egui::Ui, tab: &mut DocTab, acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("OUTLINE").small().strong().color(crate::theme::rgb(110, 115, 125)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if tab.analysis.as_ref().map(|a| a.ver) != Some(tab.doc.text.ver) {
                ui.spinner();
            }
            if let Some(sel) = &tab.outline_sel {
                let (a, b) = sel.range();
                if b > a {
                    ui.label(egui::RichText::new(format!("{} selected", b - a + 1)).small().color(crate::theme::rgb(40, 90, 180)));
                }
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
        // keep the selection on the same entries
        if let Some(sel) = tab.outline_sel.as_mut() {
            let find = |k: &str| rows.iter().position(|r| r.key == k);
            match (find(&sel.keys.0), find(&sel.keys.1)) {
                (Some(a), Some(b)) => {
                    sel.anchor = a;
                    sel.head = b;
                }
                _ => tab.outline_sel = None,
            }
        }
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
    let (shift, ptr, down) = ui.input(|i| (i.modifiers.shift, i.pointer.interact_pos(), i.pointer.primary_down()));
    let mut new_sel: Option<(usize, usize)> = None;
    let mut drag_end = false;
    let sel_range = tab.outline_sel.as_ref().map(|s| s.range());
    let multi = sel_range.is_some_and(|(a, b)| b > a);
    let mut sa = egui::ScrollArea::vertical().id_salt(tab.edit_id.with("outline")).auto_shrink([false, false]);
    if let Some(o) = tab.outline_scroll.take() {
        sa = sa.vertical_scroll_offset(o);
    }
    let out = sa.show_rows(ui, row_h, rows.len(), |ui, range| {
        for ri in range {
            let r = &rows[ri];
            let Some(n) = node_at(&an.outline, &r.path) else { continue };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click_and_drag());
            let p = ui.painter();
            let in_sel = sel_range.is_some_and(|(a, b)| ri >= a && ri <= b);
            let selected = n.block.is_some() && n.block == cur_block && n.children.is_empty();
            if in_sel && multi {
                p.rect_filled(rect, 0.0, crate::theme::rgb(198, 218, 250));
            } else if selected || in_sel {
                p.rect_filled(rect, 3.0, crate::theme::rgb(220, 232, 252));
            } else if resp.hovered() {
                p.rect_filled(rect, 3.0, crate::theme::rgb(236, 239, 244));
            }
            let mut x = rect.left() + 4.0 + r.depth as f32 * 14.0;
            let cy = rect.center().y;
            let has_kids = !n.children.is_empty();
            let arrow_rect = Rect::from_center_size(Pos2::new(x + 6.0, cy), Vec2::splat(14.0));
            if has_kids {
                let c = crate::theme::rgb(120, 125, 135);
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
                p.text(br.center(), egui::Align2::CENTER_CENTER, badge, FontId::proportional(10.5), crate::theme::t(Color32::WHITE));
                x += 20.0;
            } else {
                p.circle_filled(Pos2::new(x + 4.0, cy), 2.0, col);
                x += 12.0;
            }
            let detail_w = if n.detail.is_empty() { 0.0 } else { ui.fonts_mut(|f| f.layout_no_wrap(n.detail.clone(), small.clone(), crate::theme::t(Color32::GRAY)).size().x) + 10.0 };
            let max_w = (rect.right() - x - detail_w - 6.0).max(20.0);
            let label_color = if n.err {
                crate::theme::t(Palette::ERR)
            } else if n.kind == NodeKind::Commented {
                crate::theme::t(Palette::COMMENT)
            } else {
                crate::theme::rgb(40, 44, 52)
            };
            let galley = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(n.label.clone(), font.clone(), label_color);
                if n.kind == NodeKind::Commented {
                    if let Some(sec) = job.sections.first_mut() {
                        sec.format.italics = true;
                    }
                }
                job.wrap.max_width = max_w;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                f.layout_job(job)
            });
            p.galley(Pos2::new(x, cy - galley.size().y / 2.0), galley, label_color);
            if !n.detail.is_empty() {
                p.text(Pos2::new(rect.right() - 6.0, cy), egui::Align2::RIGHT_CENTER, &n.detail, small.clone(), crate::theme::rgb(140, 145, 155));
            }
            let resp = if n.kind == NodeKind::Commented { resp.on_hover_text(format!("Commented out: {}\nRight-click → Uncomment", n.label)) } else { resp };
            // ---- selection: click, Shift+click, drag over rows
            if resp.drag_started_by(egui::PointerButton::Primary) {
                let a = if shift { tab.outline_sel.as_ref().map(|s| s.anchor).unwrap_or(ri) } else { ri };
                new_sel = Some((a, ri));
                if let Some(s) = tab.outline_sel.as_mut() {
                    s.dragging = true;
                } else {
                    tab.outline_sel = Some(OutlineSel { anchor: a, head: ri, keys: Default::default(), dragging: true });
                }
            }
            if tab.outline_sel.as_ref().is_some_and(|s| s.dragging) && down {
                if let Some(pp) = ptr {
                    if pp.y >= rect.top() && pp.y < rect.bottom() {
                        let a = new_sel.map(|x| x.0).or(tab.outline_sel.as_ref().map(|s| s.anchor)).unwrap_or(ri);
                        new_sel = Some((a, ri));
                    }
                }
            }
            if resp.clicked() {
                let on_arrow = resp.interact_pointer_pos().map(|pp| arrow_rect.expand(3.0).contains(pp)).unwrap_or(false);
                if has_kids && (on_arrow || (!r.open && !shift)) {
                    toggles.push((r.key.clone(), !r.open));
                }
                if shift {
                    let a = tab.outline_sel.as_ref().map(|s| s.anchor).unwrap_or(ri);
                    new_sel = Some((a, ri));
                } else if !on_arrow {
                    new_sel = Some((ri, ri));
                    jump = Some(n.line);
                }
            }
            if resp.double_clicked() && has_kids {
                toggles.push((r.key.clone(), !r.open));
            }
            if resp.secondary_clicked() && !sel_range.is_some_and(|(a, b)| ri >= a && ri <= b) {
                new_sel = Some((ri, ri));
            }
            let in_multi = multi && in_sel;
            resp.context_menu(|ui| {
                if in_multi {
                    let (a, b) = sel_range.unwrap();
                    multi_menu(ui, &an, rows, a, b, &stem, acts);
                } else {
                    row_menu(ui, &an, n, &stem, acts, &mut jump);
                }
            });
        }
    });
    if tab.outline_sel.as_ref().is_some_and(|s| s.dragging) {
        if !down {
            drag_end = true;
        } else if let Some(pp) = ptr {
            // drag above / below the list: scroll
            let r = out.inner_rect;
            let dy = if pp.y < r.top() { pp.y - r.top() } else if pp.y > r.bottom() { pp.y - r.bottom() } else { 0.0 };
            if dy != 0.0 {
                tab.outline_scroll = Some((out.state.offset.y + dy * 0.3).max(0.0));
                let edge = if dy < 0.0 { out.state.offset.y / row_h } else { (out.state.offset.y + r.height()) / row_h };
                let i = (edge.max(0.0) as usize).min(rows.len().saturating_sub(1));
                let a = new_sel.map(|x| x.0).or(tab.outline_sel.as_ref().map(|s| s.anchor)).unwrap_or(i);
                new_sel = Some((a, i));
                ui.ctx().request_repaint();
            }
        }
    }
    if let Some((a, b)) = new_sel {
        let dragging = tab.outline_sel.as_ref().is_some_and(|s| s.dragging) && !drag_end;
        let keys = (rows.get(a).map(|r| r.key.clone()).unwrap_or_default(), rows.get(b).map(|r| r.key.clone()).unwrap_or_default());
        tab.outline_sel = Some(OutlineSel { anchor: a, head: b, keys, dragging });
    } else if drag_end {
        if let Some(s) = tab.outline_sel.as_mut() {
            s.dragging = false;
        }
    }
    tab.outline_cache = cache;
    for (k, v) in toggles {
        tab.outline_open.insert(k, v);
        tab.outline_rev += 1;
    }
    if let Some(l) = jump {
        tab.jump_to_line(l, None);
    }
}

/// Right-click menu for several selected rows.
fn multi_menu(ui: &mut egui::Ui, an: &Analysis, rows: &[FlatRow], a: usize, b: usize, stem: &str, acts: &mut Vec<Action>) {
    let Some((s0, e0)) = rows_span(an, rows, a, b) else {
        ui.label("Nothing to save in the selection");
        return;
    };
    let (s, e, notes) = balance_span(an, s0, e0);
    let n = b - a + 1;
    ui.label(egui::RichText::new(format!("{n} selected entries")).small().weak());
    if !notes.is_empty() {
        ui.label(egui::RichText::new(notes.join("; ")).small().color(crate::theme::t(Palette::WARN)));
    }
    let first = rows.get(a).and_then(|r| node_at(&an.outline, &r.path)).map(|n| n.label.clone()).unwrap_or_default();
    let name = sanitize(first.split_whitespace().take(3).collect::<Vec<_>>().join("_").trim_start_matches('*'));
    if ui.button("Save selection as one *Include file…").clicked() {
        let mut label = format!("the {n} selected outline entries");
        if !notes.is_empty() {
            label.push_str(&format!(" ({})", notes.join("; ")));
        }
        acts.push(Action::SplitBlock { ranges: vec![(s, e)], suggested: format!("{stem}_{name}_etc.inp"), label });
        ui.close();
    }
    ui.separator();
    if ui.button("Comment out (**)").clicked() {
        acts.push(Action::Comment { ranges: vec![(s, e)], mode: crate::comment::Mode::Comment });
        ui.close();
    }
    // only the commented-out entries of the selection (not ordinary comments around them)
    let commented: Vec<(usize, usize)> = rows[a..=b.min(rows.len() - 1)]
        .iter()
        .filter_map(|r| node_at(&an.outline, &r.path))
        .filter(|n| n.kind == NodeKind::Commented)
        .filter_map(|n| n.range)
        .collect();
    if !commented.is_empty() && ui.button(format!("Uncomment {} commented-out entr{}", commented.len(), if commented.len() == 1 { "y" } else { "ies" })).clicked() {
        acts.push(Action::Comment { ranges: commented, mode: crate::comment::Mode::Uncomment });
        ui.close();
    }
}

/// Right-click menu of one row.
fn row_menu(ui: &mut egui::Ui, an: &Arc<Analysis>, n: &OutlineNode, stem: &str, acts: &mut Vec<Action>, jump: &mut Option<usize>) {
    if ui.button("Go to").clicked() {
        *jump = Some(n.line);
        ui.close();
    }
    if n.kind == NodeKind::Commented {
        if let Some((s, e)) = n.range {
            if ui.button("Uncomment").on_hover_text("Remove the ** in front of these lines").clicked() {
                acts.push(Action::Comment { ranges: vec![(s, e)], mode: crate::comment::Mode::Uncomment });
                ui.close();
            }
        }
        return;
    }
    if n.block.is_none() && !n.children.is_empty() {
        let mut leaves = Vec::new();
        leaf_blocks(n, &mut leaves);
        let lbl = n.label.trim_start_matches('*').to_string();
        if ui.button(format!("Save all {} blocks into one *Include file…", util::fmt_int(leaves.len() as u64))).clicked() {
            acts.push(Action::SplitBlock { ranges: merge_blocks(an, &leaves), suggested: format!("{}_{}.inp", stem, sanitize(&lbl)), label: format!("all {} *{} blocks", leaves.len(), lbl) });
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
                    acts.push(block_split_action(an, bi, stem));
                    ui.close();
                }
            } else if let Some((s, e)) = node_range(an, n) {
                let lbl = n.label.split_whitespace().collect::<Vec<_>>().join(" ");
                if ui.button("Save as *Include file…").clicked() {
                    acts.push(Action::SplitBlock { ranges: vec![(s, e)], suggested: format!("{}_{}.inp", stem, sanitize(&lbl)), label: lbl.clone() });
                    ui.close();
                }
            }
            if n.kind == NodeKind::Part && !mesh_ranges(an, Some(bi)).is_empty() && ui.button("Save this part's mesh as *Include file(s)…").clicked() {
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
    if let Some((s0, e0)) = row_range(an, n) {
        ui.separator();
        let (s, e, notes) = balance_span(an, s0, e0);
        let r = ui.button("Comment out (**)");
        let r = if notes.is_empty() { r } else { r.on_hover_text(notes.join("; ")) };
        if r.clicked() {
            acts.push(Action::Comment { ranges: vec![(s, e)], mode: crate::comment::Mode::Comment });
            ui.close();
        }
    }
}
