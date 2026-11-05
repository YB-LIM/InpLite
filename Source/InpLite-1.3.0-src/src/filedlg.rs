//! A small built-in file dialog (used on Linux, where native dialogs need GTK, a desktop
//! portal or zenity — often missing on HPC login nodes and remote desktops).

use crate::util;
use egui::{Color32, Key, RichText};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Open,
    Save,
    Folder,
}

struct Entry {
    name: String,
    dir: bool,
    size: u64,
}

pub struct FileDlg {
    pub mode: Mode,
    pub title: String,
    dir: PathBuf,
    path_text: String,
    name: String,
    entries: Vec<Entry>,
    only_inp: bool,
    show_hidden: bool,
    err: String,
    selected: Option<usize>,
    confirm_overwrite: Option<PathBuf>,
}

pub enum Outcome {
    Chosen(PathBuf),
    Cancelled,
}

/// Input and output files InpLite opens.
fn is_abaqus_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".inp", ".dat", ".msg", ".sta", ".log"].iter().any(|e| n.ends_with(e))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

impl FileDlg {
    pub fn new(mode: Mode, title: &str, dir: Option<&Path>, name: &str) -> FileDlg {
        let dir = dir
            .filter(|d| d.is_dir())
            .map(|d| d.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(home);
        let mut f = FileDlg {
            mode,
            title: title.to_string(),
            path_text: String::new(),
            dir: PathBuf::new(),
            name: name.to_string(),
            entries: Vec::new(),
            only_inp: mode == Mode::Open,
            show_hidden: false,
            err: String::new(),
            selected: None,
            confirm_overwrite: None,
        };
        f.go(dir);
        f
    }

    fn go(&mut self, dir: PathBuf) {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                let mut v: Vec<Entry> = rd
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        // follow symlinks (scratch/project links are common on clusters)
                        let md = std::fs::metadata(e.path()).ok();
                        let dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                        Some(Entry { name, dir, size: md.map(|m| m.len()).unwrap_or(0) })
                    })
                    .collect();
                v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                self.entries = v;
                self.path_text = dir.to_string_lossy().into_owned();
                self.dir = dir;
                self.err.clear();
                self.selected = None;
            }
            Err(e) => self.err = format!("{}: {e}", dir.display()),
        }
    }

    fn visible(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.name.starts_with('.'))
            .filter(|(_, e)| e.dir || self.mode == Mode::Save || !self.only_inp || is_abaqus_file(&e.name))
            .filter(|(_, e)| e.dir || self.mode != Mode::Folder)
            .map(|(i, _)| i)
            .collect()
    }

    /// Draw the dialog; returns the outcome once the user chose or cancelled.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        let mut out = None;
        let mut nav: Option<PathBuf> = None;
        let mut accept = false;
        let mut open = true;
        egui::Window::new(self.title.clone())
            .id(egui::Id::new("inplite_file_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 460.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("↑").on_hover_text("Parent folder").clicked() {
                        if let Some(p) = self.dir.parent() {
                            nav = Some(p.to_path_buf());
                        }
                    }
                    if ui.button("Home").clicked() {
                        nav = Some(home());
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(ui.available_width()).hint_text("folder"));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let p = PathBuf::from(self.path_text.trim());
                        if p.is_dir() {
                            nav = Some(p);
                        } else if p.is_file() && self.mode == Mode::Open {
                            out = Some(Outcome::Chosen(p));
                        } else {
                            self.err = format!("Not a folder: {}", p.display());
                        }
                    }
                });
                ui.separator();
                let vis = self.visible();
                let row_h = 20.0;
                let list_h = (ui.available_height() - 90.0).max(120.0);
                egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show_rows(ui, row_h, vis.len(), |ui, range| {
                    for &i in &vis[range] {
                        let e = &self.entries[i];
                        let sel = self.selected == Some(i);
                        let label = if e.dir { format!("▸ {}/", e.name) } else { e.name.clone() };
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
                        if sel {
                            ui.painter().rect_filled(rect, 2.0, crate::theme::rgb(214, 228, 252));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 2.0, crate::theme::rgb(238, 241, 246));
                        }
                        let color = if e.dir { crate::theme::rgb(40, 70, 130) } else { crate::theme::rgb(40, 44, 52) };
                        ui.painter().text(rect.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(13.5), color);
                        if !e.dir {
                            ui.painter().text(rect.right_center() - egui::vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, util::fmt_bytes(e.size), egui::FontId::proportional(12.0), crate::theme::t(Color32::GRAY));
                        }
                        if resp.clicked() {
                            self.selected = Some(i);
                            if !e.dir && self.mode != Mode::Folder {
                                self.name = e.name.clone();
                            }
                        }
                        if resp.double_clicked() {
                            if e.dir {
                                nav = Some(self.dir.join(&e.name));
                            } else if self.mode != Mode::Folder {
                                self.name = e.name.clone();
                                accept = true;
                            }
                        }
                    }
                });
                ui.separator();
                if self.mode != Mode::Folder {
                    ui.horizontal(|ui| {
                        ui.label("File name");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(ui.available_width() - 4.0));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            accept = true;
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if self.mode == Mode::Open {
                        ui.checkbox(&mut self.only_inp, "Abaqus files only").on_hover_text("*.inp, *.dat, *.msg, *.sta, *.log");
                    }
                    ui.checkbox(&mut self.show_hidden, "hidden files");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            out = Some(Outcome::Cancelled);
                        }
                        let ok = match self.mode {
                            Mode::Open => "Open",
                            Mode::Save => "Save",
                            Mode::Folder => "Select this folder",
                        };
                        if ui.button(ok).clicked() {
                            accept = true;
                        }
                    });
                });
                if let Some(p) = &self.confirm_overwrite {
                    ui.label(RichText::new(format!("{} exists. Press Save again to replace it.", p.display())).color(crate::theme::t(crate::syntax::Palette::WARN)));
                }
                if !self.err.is_empty() {
                    ui.label(RichText::new(&self.err).color(crate::theme::t(crate::syntax::Palette::ERR)));
                }
            });
        if !open {
            out = Some(Outcome::Cancelled);
        }
        if let Some(p) = nav {
            self.go(p);
        }
        if accept && out.is_none() {
            match self.mode {
                Mode::Folder => {
                    let sel = self.selected.map(|i| &self.entries[i]).filter(|e| e.dir).map(|e| self.dir.join(&e.name));
                    out = Some(Outcome::Chosen(sel.unwrap_or_else(|| self.dir.clone())));
                }
                Mode::Open | Mode::Save => {
                    let name = self.name.trim();
                    if name.is_empty() {
                        self.err = "Enter a file name.".into();
                    } else {
                        let p = if Path::new(name).is_absolute() { PathBuf::from(name) } else { self.dir.join(name) };
                        if p.is_dir() {
                            self.go(p);
                        } else if self.mode == Mode::Open && !p.is_file() {
                            self.err = format!("File not found: {}", p.display());
                        } else if self.mode == Mode::Save && p.exists() && self.confirm_overwrite.as_ref() != Some(&p) {
                            self.confirm_overwrite = Some(p);
                        } else {
                            out = Some(Outcome::Chosen(p));
                        }
                    }
                }
            }
        }
        out
    }
}
