//! Diff tab UI.

use crate::actions::Action;
use crate::diff::{DiffResult, RowKind};
use crate::util;
use egui::{Color32, RichText};
use std::path::PathBuf;
use std::sync::Arc;

pub struct DiffTab {
    pub res: Arc<DiffResult>,
    pub a_path: PathBuf,
    pub b_path: PathBuf,
    pub a_real: Vec<u64>,
    pub b_real: Vec<u64>,
    pub show_same: bool,
    pub filter: String,
}

fn tag(k: RowKind) -> (&'static str, Color32, Color32) {
    match k {
        RowKind::Modified => ("CHANGED", crate::theme::rgb(160, 100, 0), crate::theme::rgb(255, 242, 210)),
        RowKind::Added => ("ADDED", crate::theme::rgb(20, 120, 50), crate::theme::rgb(222, 246, 228)),
        RowKind::Removed => ("REMOVED", crate::theme::rgb(190, 30, 30), crate::theme::rgb(253, 228, 228)),
        RowKind::Same => ("SAME", crate::theme::t(Color32::GRAY), crate::theme::rgb(245, 245, 245)),
    }
}

pub fn diff_ui(ui: &mut egui::Ui, d: &mut DiffTab, acts: &mut Vec<Action>) {
    let r = d.res.clone();
    ui.horizontal(|ui| {
        ui.heading("Compare");
        ui.label(RichText::new(format!("A: {}", r.a_name)).monospace());
        ui.label("↔");
        ui.label(RichText::new(format!("B: {}", r.b_name)).monospace());
    });
    ui.horizontal(|ui| {
        for (k, n) in [(RowKind::Modified, r.modified), (RowKind::Added, r.added), (RowKind::Removed, r.removed)] {
            let (t, fg, bg) = tag(k);
            ui.label(RichText::new(format!(" {n} {} ", t.to_ascii_lowercase())).color(fg).background_color(bg));
        }
        ui.label(RichText::new(format!("{} identical blocks · whitespace, letter case and comments ignored · {} ms", util::fmt_int(r.same as u64), r.elapsed_ms)).weak());
        ui.separator();
        ui.checkbox(&mut d.show_same, "show identical");
        ui.add(egui::TextEdit::singleline(&mut d.filter).hint_text("filter").desired_width(160.0));
    });
    ui.separator();
    let f = d.filter.to_ascii_lowercase();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (ri, row) in r.rows.iter().enumerate() {
            if row.kind == RowKind::Same {
                if d.show_same {
                    for k in 0..row.run {
                        let a = &r.a[row.a.unwrap() + k];
                        ui.label(RichText::new(format!("        {}   {}", a.label, a.ctx)).monospace().color(crate::theme::rgb(150, 150, 150)));
                    }
                } else {
                    ui.label(RichText::new(format!("   … {} identical block{}", util::fmt_int(row.run as u64), if row.run == 1 { "" } else { "s" })).small().color(crate::theme::rgb(160, 160, 160)));
                }
                continue;
            }
            let blk = row.a.map(|i| &r.a[i]).or(row.b.map(|i| &r.b[i])).unwrap();
            if !f.is_empty() && !blk.label.to_ascii_lowercase().contains(&f) && !blk.ctx.to_ascii_lowercase().contains(&f) {
                continue;
            }
            let (t, fg, bg) = tag(row.kind);
            let id = ui.make_persistent_id(("diffrow", ri));
            let default_open = row.kind == RowKind::Modified && row.lines.len() + row.changes.len() <= 24;
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open)
                .show_header(ui, |ui| {
                    ui.label(RichText::new(format!(" {t:<7} ")).monospace().color(fg).background_color(bg));
                    ui.label(RichText::new(&blk.label).monospace().strong());
                    if !blk.ctx.is_empty() {
                        ui.label(RichText::new(&blk.ctx).weak());
                    }
                    if let Some(i) = row.a {
                        let real = d.a_real.get(r.a[i].line).copied().unwrap_or(0);
                        if ui.link(format!("A:{}", util::fmt_int(real))).clicked() {
                            acts.push(Action::OpenFile(d.a_path.clone(), Some(real)));
                        }
                    }
                    if let Some(i) = row.b {
                        let real = d.b_real.get(r.b[i].line).copied().unwrap_or(0);
                        if ui.link(format!("B:{}", util::fmt_int(real))).clicked() {
                            acts.push(Action::OpenFile(d.b_path.clone(), Some(real)));
                        }
                    }
                })
                .body(|ui| {
                    for c in &row.changes {
                        ui.label(RichText::new(format!("  parameter  {c}")).monospace().color(crate::theme::rgb(128, 38, 160)));
                    }
                    for (sign, l) in &row.lines {
                        let (fg, bg) = if *sign == '-' {
                            (crate::theme::rgb(150, 20, 20), crate::theme::rgb(253, 232, 232))
                        } else {
                            (crate::theme::rgb(20, 110, 40), crate::theme::rgb(226, 248, 232))
                        };
                        ui.label(RichText::new(format!(" {sign} {l}")).monospace().color(fg).background_color(bg));
                    }
                    if let Some(n) = &row.note {
                        ui.label(RichText::new(n).italics().weak());
                    }
                    if row.kind == RowKind::Added || row.kind == RowKind::Removed {
                        ui.label(RichText::new(format!("  {} data lines", util::fmt_int(blk.data_lines))).weak());
                    }
                });
        }
    });
}
