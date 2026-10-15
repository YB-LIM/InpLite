//! Bottom panel content: problems list and reference list.

use crate::actions::Action;
use crate::syntax::{Palette, Sev};
use crate::tab::DocTab;
use crate::util;
use egui::{Color32, FontId, Pos2, Sense, Vec2};

pub fn problems_ui(ui: &mut egui::Ui, tab: &mut DocTab, acts: &mut Vec<Action>) {
    let diags = tab.all_diags();
    let ne = diags.iter().filter(|d| d.sev == Sev::Error).count();
    let nw = diags.len() - ne;
    ui.horizontal(|ui| {
        ui.toggle_value(&mut tab.problems_filter.0, egui::RichText::new(format!("● {ne} errors")).color(Palette::ERR));
        ui.toggle_value(&mut tab.problems_filter.1, egui::RichText::new(format!("● {nw} warnings")).color(Palette::WARN));
        ui.separator();
        match (&tab.mesh, &tab.mesh_job) {
            (_, Some((_, p))) => {
                ui.spinner();
                ui.label(egui::RichText::new(format!("mesh check {:.0}%", p.frac() * 100.0)).weak());
            }
            (Some(m), None) => {
                ui.label(
                    egui::RichText::new(format!(
                        "mesh check: {} nodes, {} elements in {:.1} s",
                        util::fmt_int(m.total_nodes),
                        util::fmt_int(m.total_elements),
                        m.elapsed_ms as f64 / 1000.0
                    ))
                    .weak(),
                );
            }
            _ => {}
        }
    });
    ui.add_space(2.0);
    let shown: Vec<&crate::syntax::Diag> = diags.iter().filter(|d| if d.sev == Sev::Error { tab.problems_filter.0 } else { tab.problems_filter.1 }).collect();
    if shown.is_empty() {
        ui.label(egui::RichText::new("No problems found.").weak());
        return;
    }
    tab.ensure_map();
    let row_h = 20.0;
    let mut go: Option<crate::syntax::Diag> = None;
    egui::ScrollArea::vertical().id_salt(tab.edit_id.with("problems")).auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
        for d in &shown[range] {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(236, 239, 244));
            }
            let c = if d.sev == Sev::Error { Palette::ERR } else { Palette::WARN };
            ui.painter().circle_filled(Pos2::new(rect.left() + 10.0, rect.center().y), 4.0, c);
            let real = tab.map.real.get(d.line).copied().unwrap_or(0) + d.bulk.map(|b| b.1).unwrap_or(0);
            let loc = format!("Ln {}", util::fmt_int(real));
            ui.painter().text(Pos2::new(rect.left() + 22.0, rect.center().y), egui::Align2::LEFT_CENTER, &loc, FontId::monospace(11.5), Color32::from_rgb(120, 125, 135));
            ui.painter().text(Pos2::new(rect.left() + 120.0, rect.center().y), egui::Align2::LEFT_CENTER, &d.msg, FontId::proportional(13.0), Color32::from_rgb(40, 44, 52));
            if resp.clicked() {
                go = Some((*d).clone());
            }
        }
    });
    if let Some(d) = go {
        if let Some((bulk, row)) = d.bulk {
            acts.push(Action::OpenViewer { bulk, row: Some(row) });
            tab.jump_to_line(d.line, None);
        } else {
            let ls = tab.map.starts.get(d.line).copied().unwrap_or(0);
            let le = tab.map.starts.get(d.line + 1).copied().unwrap_or(tab.doc.text.s.len());
            let sel = if d.start >= ls && d.end <= le && d.end > d.start { Some((d.start, d.end)) } else { None };
            tab.jump_to_line(d.line, sel);
        }
    }
}

pub fn refs_ui(ui: &mut egui::Ui, tab: &mut DocTab, acts: &mut Vec<Action>) {
    if tab.refs.is_empty() {
        ui.label(egui::RichText::new("Place the cursor on a set / surface / material name and press Shift+F12.").weak());
        return;
    }
    ui.label(egui::RichText::new(&tab.refs_title).strong());
    tab.ensure_map();
    let mut go = None;
    egui::ScrollArea::vertical().id_salt(tab.edit_id.with("refs")).auto_shrink([false, false]).show(ui, |ui| {
        for (i, r) in tab.refs.iter().enumerate() {
            let loc = match &r.file {
                Some((p, l)) => format!("{}:{}", p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), l),
                None => format!("Ln {}", util::fmt_int(tab.map.real.get(r.line).copied().unwrap_or(0))),
            };
            let txt = egui::RichText::new(format!("{:<14} {}", loc, r.label)).monospace();
            let txt = if r.is_def { txt.color(Palette::KEYWORD) } else { txt };
            if ui.selectable_label(false, txt).clicked() {
                go = Some(i);
            }
        }
    });
    if let Some(i) = go {
        let r = &tab.refs[i];
        match &r.file {
            Some((p, l)) => acts.push(Action::OpenFile(p.clone(), Some(*l))),
            None => {
                let (l, s, e) = (r.line, r.start, r.end);
                tab.jump_to_line(l, if e > s { Some((s, e)) } else { None });
            }
        }
    }
}
