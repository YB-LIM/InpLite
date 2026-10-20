//! Application shell: tabs, menus, panels, dialogs, background jobs.

use crate::actions::Action;
use crate::analysis;
use crate::diff;
use crate::diffview::{diff_ui, DiffTab};
use crate::doc::{expected_size, export_nomesh, write_include_file, write_temp, Document, TempWrite};
use crate::editor::{editor_ui, find_references, go_to_definition};
use crate::load::{load, Loaded, Progress};
use crate::mesh::{Hit, IdKind};
use crate::outline::outline_ui;
use crate::panels::{problems_ui, refs_ui};
#[cfg(windows)]
use crate::settings::{pick_folder, pick_open, pick_save};
use crate::settings::{IncludeFolder, Settings};
use crate::store::{placeholder_id, STORE};
use crate::syntax::{Palette, Sev};
use crate::tab::{BulkViewer, DocTab, LineMap};
use crate::util;
use crate::viewer::viewer_ui;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

pub enum Tab {
    Doc(Box<DocTab>),
    Diff(Box<DiffTab>),
}

pub enum After {
    Open { jump: Option<u64> },
    Inline { edit_id: egui::Id, start: usize, end: usize, ver: u64 },
}

pub struct LoadJob {
    pub path: PathBuf,
    pub prog: Arc<Progress>,
    pub rx: Receiver<io::Result<Loaded>>,
    pub after: After,
}

pub struct DiffJob {
    pub rx: Receiver<Result<DiffTab, String>>,
    pub label: String,
}

/// A file write that runs in a background thread (save, export, include split).
pub enum OpKind {
    Save { edit_id: egui::Id, target: PathBuf, ver: u64, sig: (usize, u64), keep: bool, t0: std::time::Instant },
    Export { target: PathBuf },
    /// edits: (start, end, include file name) in the text version `ver`
    Split { edit_id: egui::Id, ver: u64, edits: Vec<SplitEdit>, before: String },
}

pub enum OpResult {
    Temp(io::Result<TempWrite>),
    Bytes(io::Result<u64>),
    Files(Vec<io::Result<u64>>),
}

pub struct FileOp {
    pub label: String,
    pub prog: Arc<Progress>,
    pub rx: Receiver<OpResult>,
    pub kind: OpKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Bottom {
    Problems,
    Refs,
    Data,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GotoMode {
    Line,
    Node,
    Element,
}

pub struct GotoDlg {
    pub mode: GotoMode,
    pub text: String,
    pub results: Vec<(String, Hit)>,
    pub msg: String,
    pub focus: bool,
    pub pending: Option<Receiver<Vec<(String, Hit)>>>,
    /// (mode, label) of `results`; Enter again with the same label goes to the next match
    pub searched: Option<(GotoMode, u64)>,
    pub idx: usize,
}

pub struct SplitDlg {
    pub ranges: Vec<(usize, usize)>,
    /// folder the include file is written to (default: the folder of the input file)
    pub folder: String,
    pub name: String,
    pub label: String,
    pub ver: u64,
    pub err: String,
}

/// One include file of a split operation: these byte ranges (file order) go into the file;
/// the *INCLUDE line (with input=`inc`) replaces the first range, the others are removed.
#[derive(Clone)]
pub struct SplitEdit {
    pub ranges: Vec<(usize, usize)>,
    pub inc: String,
}

pub struct BatchItem {
    pub ranges: Vec<(usize, usize)>,
    pub name: String,
    pub label: String,
}

/// How a part mesh is split into *INCLUDE files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MeshSplit {
    /// one file: *Node, *Element and the *Nset / *Elset blocks right after them
    Combined,
    /// *Node blocks and *Element blocks in two files (sets stay in the main file)
    NodesElements,
    /// four files: *Node, *Element, *Nset, *Elset
    AllFour,
}

pub enum BatchKind {
    /// meshes of these parts (`None` = model level of a flat input)
    Meshes { parts: Vec<Option<usize>>, mode: MeshSplit },
    /// one file per block; data blocks can be filtered by their number of data lines
    Blocks { blocks: Vec<usize>, min_lines: Option<u64> },
}

/// Save several *INCLUDE files at once (all part meshes, all data blocks, …) into one folder.
pub struct BatchDlg {
    pub title: String,
    pub folder: String,
    pub kind: BatchKind,
    pub an: Arc<crate::analysis::Analysis>,
    pub stem: String,
    pub items: Vec<BatchItem>,
    /// candidates before filtering (data blocks)
    pub total: usize,
    pub ver: u64,
    pub overwrite: bool,
    pub err: String,
}

impl BatchDlg {
    /// (Re)build the file list from the kind and its options.
    pub fn rebuild(&mut self) {
        let an = &self.an;
        let stem = &self.stem;
        let mut items = Vec::new();
        match &self.kind {
            BatchKind::Meshes { parts, mode } => {
                self.total = parts.len();
                for part in parts {
                    let (tag, what) = match part {
                        Some(pb) => {
                            let p = an.blocks[*pb].param("NAME").unwrap_or("part").trim_matches('"').to_string();
                            (format!("{}_{}", stem, crate::outline::sanitize(&p)), format!("part {p}"))
                        }
                        None => (stem.clone(), "model".to_string()),
                    };
                    match mode {
                        MeshSplit::Combined => {
                            let r = crate::outline::mesh_ranges(an, *part);
                            if !r.is_empty() {
                                items.push(BatchItem { ranges: r, name: format!("{tag}_mesh.inp"), label: format!("{what}: from the first to the last *Node / *Element block (+ sets right after)") });
                            }
                        }
                        MeshSplit::NodesElements | MeshSplit::AllFour => {
                            let scope = crate::outline::scope_blocks(an, *part);
                            if let Some(why) = crate::outline::order_sensitive(an, &scope) {
                                // splitting would change the order of the mesh definitions: keep one file
                                let r = crate::outline::mesh_ranges(an, *part);
                                if !r.is_empty() {
                                    items.push(BatchItem { ranges: r, name: format!("{tag}_mesh.inp"), label: format!("{what}: one file — {why}") });
                                }
                                continue;
                            }
                            let mut kinds = vec![("NODE", "nodes"), ("ELEMENT", "elements")];
                            let sets_ref = scope.iter().any(|&i| an.blocks[i].norm == "NSET" && an.blocks[i].has_param("ELSET"));
                            if *mode == MeshSplit::AllFour && !sets_ref {
                                kinds.push(("NSET", "nsets"));
                                kinds.push(("ELSET", "elsets"));
                            }
                            for (kw, suffix) in kinds {
                                let sel: Vec<usize> = scope.iter().copied().filter(|&i| an.blocks[i].norm == kw).collect();
                                if !sel.is_empty() {
                                    items.push(BatchItem {
                                        ranges: crate::outline::merge_blocks(an, &sel),
                                        name: format!("{tag}_{suffix}.inp"),
                                        label: format!("{what}: {} *{} block{}", sel.len(), util::title_case(kw), if sel.len() == 1 { "" } else { "s" }),
                                    });
                                }
                            }
                            if *mode == MeshSplit::AllFour && sets_ref {
                                if let Some(last) = items.last_mut() {
                                    last.label.push_str(" (sets stay in the main file: an *Nset refers to an element set)");
                                }
                            }
                        }
                    }
                }
            }
            BatchKind::Blocks { blocks, min_lines } => {
                self.total = blocks.len();
                let mut used: HashSet<String> = HashSet::new();
                for &bi in blocks {
                    if let Some(m) = min_lines {
                        if an.blocks[bi].data_lines < *m {
                            continue;
                        }
                    }
                    if let Action::SplitBlock { ranges, suggested, label } = crate::outline::block_split_action(an, bi, stem) {
                        let base = suggested.trim_end_matches(".inp").to_string();
                        let mut name = suggested.clone();
                        let mut k = 2;
                        while !used.insert(name.to_ascii_lowercase()) {
                            name = format!("{base}_{k}.inp");
                            k += 1;
                        }
                        items.push(BatchItem { ranges, name, label: format!("{} · {} lines", label, util::fmt_int(an.blocks[bi].data_lines)) });
                    }
                }
            }
        }
        self.items = items;
    }
}

/// What a path chosen in a file dialog is for.
#[derive(Clone, Copy, Debug)]
pub enum PathReq {
    Open,
    Compare,
    SaveAs { tab: usize, force: bool },
    ExportNomesh,
    SplitFile,
    BatchFolder,
    IncludeFolderSetting,
}

impl PathReq {
    fn mode(self) -> crate::filedlg::Mode {
        use crate::filedlg::Mode;
        match self {
            PathReq::Open | PathReq::Compare => Mode::Open,
            PathReq::SaveAs { .. } | PathReq::ExportNomesh | PathReq::SplitFile => Mode::Save,
            PathReq::BatchFolder | PathReq::IncludeFolderSetting => Mode::Folder,
        }
    }
}

pub enum Confirm {
    SaveDropping { tab: usize, path: PathBuf, n: usize, lines: u64 },
    Overwrite { tab: usize, path: PathBuf },
    CloseUnsaved { tab: usize },
    ExitUnsaved,
}

#[derive(Default)]
pub struct Dialogs {
    pub goto: Option<GotoDlg>,
    pub split: Option<SplitDlg>,
    pub batch: Option<BatchDlg>,
    /// "Edit as text" refused for this bulk block (explanation dialog)
    pub too_big: Option<u64>,
    pub confirm: Option<Confirm>,
    pub summary: bool,
    pub settings: bool,
    pub about: bool,
    pub disclaimer: bool,
    pub developer: bool,
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub settings: Settings,
    pub loads: Vec<LoadJob>,
    pub diff_jobs: Vec<DiffJob>,
    pub toasts: Vec<(String, f64)>,
    pub bottom: Bottom,
    pub dlg: Dialogs,
    pub exit_ok: bool,
    pub title: String,
    pub pending_acts: Vec<Action>,
    pub file_op: Option<FileOp>,
    pending_split: Option<Vec<SplitEdit>>,
    /// open built-in file dialog and what the chosen path is for
    file_dlg: Option<(crate::filedlg::FileDlg, PathReq)>,
    /// GitHub / LinkedIn / ResearchGate icons (loaded on first use)
    link_icons: Option<Vec<egui::TextureHandle>>,
    #[cfg(feature = "shots")]
    pub shots: Option<crate::shots::Shots>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> App {
        let settings = Settings::load();
        crate::theme::set_dark(settings.dark);
        setup_style(&cc.egui_ctx);
        let mut app = App {
            tabs: Vec::new(),
            active: 0,
            settings,
            loads: Vec::new(),
            diff_jobs: Vec::new(),
            toasts: Vec::new(),
            bottom: Bottom::Problems,
            dlg: Dialogs::default(),
            exit_ok: false,
            title: String::new(),
            pending_acts: Vec::new(),
            file_op: None,
            pending_split: None,
            link_icons: None,
            file_dlg: None,
            #[cfg(feature = "shots")]
            shots: crate::shots::Shots::from_env(),
        };
        for f in files {
            app.open(f, None);
        }
        app
    }

    /// Ask for a path: native dialog on Windows, the built-in dialog elsewhere (no GTK / zenity
    /// needed, e.g. on HPC login nodes). The result is handled by `on_path`.
    pub fn ask_path(&mut self, req: PathReq, title: &str, dir: Option<PathBuf>, name: &str) {
        #[cfg(windows)]
        {
            use crate::filedlg::Mode;
            let p = match req.mode() {
                Mode::Open => pick_open(title),
                Mode::Save => pick_save(title, dir.as_deref(), name),
                Mode::Folder => pick_folder(title, dir.as_deref()),
            };
            if let Some(p) = p {
                self.on_path(req, p);
            }
        }
        #[cfg(not(windows))]
        {
            self.file_dlg = Some((crate::filedlg::FileDlg::new(req.mode(), title, dir.as_deref(), name), req));
        }
    }

    fn on_path(&mut self, req: PathReq, p: PathBuf) {
        match req {
            PathReq::Open => self.open(p, None),
            PathReq::Compare => self.compare_with(p),
            PathReq::SaveAs { tab, force } => self.save_to(tab, p, force),
            PathReq::ExportNomesh => self.export_nomesh_to(p),
            PathReq::SplitFile => {
                if let Some(s) = self.dlg.split.as_mut() {
                    if let Some(parent) = p.parent() {
                        s.folder = parent.to_string_lossy().into_owned();
                    }
                    if let Some(f) = p.file_name() {
                        s.name = f.to_string_lossy().into_owned();
                    }
                    // the file dialog already asked about replacing an existing file
                    s.err = if p.exists() { format!("Overwrite? {} will be replaced.", s.name) } else { String::new() };
                }
            }
            PathReq::BatchFolder => {
                if let Some(b) = self.dlg.batch.as_mut() {
                    b.folder = p.to_string_lossy().into_owned();
                }
            }
            PathReq::IncludeFolderSetting => {
                self.settings.include_fixed = p.to_string_lossy().into_owned();
                self.settings.save();
            }
        }
    }

    /// File → New: an untitled document with a minimal skeleton; the first save asks for a name.
    pub fn new_file(&mut self) {
        let doc = Document::new_untitled(NEW_FILE_TEMPLATE);
        let mut t = DocTab::new(doc);
        // cursor on the empty heading line
        let at = NEW_FILE_TEMPLATE.find('\n').map(|i| i + 1).unwrap_or(0);
        t.jump_to_line(1, Some((at, at)));
        self.tabs.push(Tab::Doc(Box::new(t)));
        self.active = self.tabs.len() - 1;
    }

    pub fn toast(&mut self, s: impl Into<String>) {
        let s = s.into();
        // the same message again (e.g. repeated keystrokes): restart its timer instead of stacking
        self.toasts.retain(|t| t.0 != s);
        self.toasts.push((s, -1.0));
    }

    pub fn open(&mut self, path: PathBuf, jump: Option<u64>) {
        let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
        // already open?
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if let Tab::Doc(d) = t {
                if d.doc.path.as_ref().map(|p| std::fs::canonicalize(p).unwrap_or(p.clone())) == Some(canon.clone()) {
                    self.active = i;
                    if let Some(j) = jump {
                        d.jump_to_real(j);
                    }
                    return;
                }
            }
        }
        if self.loads.iter().any(|l| l.path == path) {
            return;
        }
        let prog = Progress::new();
        let p2 = prog.clone();
        let th = self.settings.bulk_threshold;
        let (tx, rx) = channel();
        let pc = path.clone();
        std::thread::spawn(move || {
            let r = load(&pc, th, &p2);
            let _ = tx.send(r);
        });
        self.loads.push(LoadJob { path, prog, rx, after: After::Open { jump } });
    }

    fn doc_tab(&mut self) -> Option<&mut DocTab> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Doc(d)) => Some(d),
            _ => None,
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) -> bool {
        let mut busy = false;
        let mut done = Vec::new();
        for (i, j) in self.loads.iter().enumerate() {
            match j.rx.try_recv() {
                Ok(r) => done.push((i, r)),
                Err(std::sync::mpsc::TryRecvError::Empty) => busy = true,
                Err(_) => done.push((i, Err(io::Error::other("loader stopped")))),
            }
        }
        for (i, r) in done.into_iter().rev() {
            let j = self.loads.remove(i);
            match (r, j.after) {
                (Ok(l), After::Open { jump }) => {
                    let ms = l.elapsed_ms;
                    let mut doc = Document::from_loaded(j.path.clone(), l);
                    doc.load_ms = ms;
                    let mut t = DocTab::new(doc);
                    if let Some(r) = jump {
                        t.jump_to_real(r);
                    }
                    self.settings.push_recent(&j.path);
                    self.settings.save();
                    self.tabs.push(Tab::Doc(Box::new(t)));
                    self.active = self.tabs.len() - 1;
                }
                (Ok(l), After::Inline { edit_id, start, end, ver }) => {
                    let mut ok = false;
                    for t in &mut self.tabs {
                        if let Tab::Doc(d) = t {
                            if d.edit_id == edit_id && d.doc.text.ver == ver && end <= d.doc.text.s.len() {
                                let mut text = l.text.clone();
                                if d.doc.text.s[start..end].ends_with('\n') && !text.ends_with('\n') {
                                    text.push('\n');
                                }
                                d.edit("Inline include", start..end, &text);
                                d.doc.known_bulks.extend(l.bulks.iter().copied());
                                d.start_mesh();
                                ok = true;
                            }
                        }
                    }
                    if ok {
                        self.toast(format!("Inlined {}", j.path.display()));
                    } else {
                        self.toast("The text changed while loading the include file; try again.");
                    }
                }
                (Err(e), _) => {
                    if e.kind() != io::ErrorKind::Interrupted {
                        self.toast(format!("Could not open {}: {e}", j.path.display()));
                    }
                }
            }
        }
        let mut dd = Vec::new();
        for (i, j) in self.diff_jobs.iter().enumerate() {
            match j.rx.try_recv() {
                Ok(r) => dd.push((i, r)),
                Err(std::sync::mpsc::TryRecvError::Empty) => busy = true,
                Err(_) => dd.push((i, Err("diff stopped".into()))),
            }
        }
        for (i, r) in dd.into_iter().rev() {
            self.diff_jobs.remove(i);
            match r {
                Ok(t) => {
                    self.tabs.push(Tab::Diff(Box::new(t)));
                    self.active = self.tabs.len() - 1;
                }
                Err(e) => self.toast(format!("Compare failed: {e}")),
            }
        }
        let now = ctx.input(|i| i.time);
        for t in &mut self.tabs {
            if let Tab::Doc(d) = t {
                if d.poll(now) {
                    busy = true;
                }
            }
        }
        busy
    }

    // ------------------------------------------------------------ file operations

    pub fn save(&mut self, ti: usize, save_as: bool, force: bool) {
        if self.file_op.is_some() {
            return;
        }
        let Some(Tab::Doc(d)) = self.tabs.get_mut(ti) else { return };
        if save_as || d.doc.path.is_none() {
            let (dir, name) = (d.doc.dir(), d.doc.title());
            self.ask_path(PathReq::SaveAs { tab: ti, force }, "Save As", Some(dir), &name);
            return;
        }
        let path = d.doc.path.clone().unwrap();
        self.save_to(ti, path, force);
    }

    /// Save tab `ti` to `path` (checks, then a background write).
    fn save_to(&mut self, ti: usize, path: PathBuf, force: bool) {
        if self.file_op.is_some() {
            return;
        }
        let keep = self.settings.keep_backup;
        let Some(Tab::Doc(d)) = self.tabs.get_mut(ti) else { return };
        if !force {
            let missing = d.doc.missing_bulks();
            if !missing.is_empty() {
                let lines = missing.iter().map(|b| b.lines).sum();
                self.dlg.confirm = Some(Confirm::SaveDropping { tab: ti, path, n: missing.len(), lines });
                return;
            }
            if Some(&path) == d.doc.path.as_ref() && d.doc.changed_on_disk() {
                self.dlg.confirm = Some(Confirm::Overwrite { tab: ti, path });
                return;
            }
        }
        // Write in a background thread; the UI stays responsive and shows a progress dialog.
        let text = d.doc.text.s.clone();
        let opts = d.doc.opts();
        let (edit_id, ver, sig) = (d.edit_id, d.doc.text.ver, d.doc.signature());
        let prog = Progress::new();
        prog.total.store(expected_size(&text), std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = channel();
        let (p2, target) = (prog.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(OpResult::Temp(write_temp(&text, &opts, &target, Some(&p2))));
        });
        let label = format!("Saving {}", path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        self.file_op = Some(FileOp { label, prog, rx, kind: OpKind::Save { edit_id, target: path, ver, sig, keep, t0: std::time::Instant::now() } });
    }

    fn export_nomesh(&mut self) {
        let Some(d) = self.doc_tab() else { return };
        let stem = d.doc.title().trim_end_matches(".inp").to_string();
        let dir = d.doc.dir();
        self.ask_path(PathReq::ExportNomesh, "Export without mesh", Some(dir), &format!("{stem}_nomesh.inp"));
    }

    fn export_nomesh_to(&mut self, p: PathBuf) {
        let Some(d) = self.doc_tab() else { return };
        let (text, opts, title) = (d.doc.text.s.clone(), d.doc.opts(), d.doc.title());
        let prog = Progress::new();
        prog.total.store(text.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = channel();
        let (p2, target) = (prog.clone(), p.clone());
        std::thread::spawn(move || {
            let _ = tx.send(OpResult::Bytes(export_nomesh(&text, opts, &title, &target, Some(&p2))));
        });
        self.file_op = Some(FileOp { label: "Exporting without mesh".into(), prog, rx, kind: OpKind::Export { target: p } });
    }

    /// Write blocks of the active document into include files (background), then replace them
    /// by *INCLUDE lines when done.
    pub fn start_split(&mut self, edits: Vec<SplitEdit>) {
        if self.file_op.is_some() {
            return;
        }
        let Some(d) = self.doc_tab() else { return };
        let dir = d.doc.dir();
        let opts = d.doc.opts();
        let (edit_id, ver) = (d.edit_id, d.doc.text.ver);
        let before = d.doc.text.s.clone();
        let slices: Vec<(PathBuf, String)> = edits
            .iter()
            .map(|e| (crate::analysis::resolve_include(&dir, &e.inc), e.ranges.iter().map(|&(a, b)| &d.doc.text.s[a..b]).collect::<String>()))
            .collect();
        let prog = Progress::new();
        prog.total.store(slices.iter().map(|x| expected_size(&x.1)).sum(), std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = channel();
        let p2 = prog.clone();
        std::thread::spawn(move || {
            let mut res = Vec::new();
            let mut base = 0u64;
            for (path, text) in &slices {
                // progress of each file is reported relative to the files written before it
                let sub = Progress::new();
                let r = std::thread::scope(|sc| {
                    let h = sc.spawn(|| write_include_file(path, text, &opts, Some(&sub)));
                    while !h.is_finished() {
                        p2.done.store(base + sub.done.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    h.join().unwrap_or_else(|_| Err(io::Error::other("writer thread failed")))
                });
                base += expected_size(text);
                res.push(r);
            }
            let _ = tx.send(OpResult::Files(res));
        });
        let label = if edits.len() == 1 { format!("Writing {}", edits[0].inc) } else { format!("Writing {} include files", edits.len()) };
        self.file_op = Some(FileOp { label, prog, rx, kind: OpKind::Split { edit_id, ver, edits, before } });
    }

    fn poll_file_op(&mut self) -> bool {
        let Some(op) = &self.file_op else { return false };
        let r = match op.rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            Err(_) => OpResult::Bytes(Err(io::Error::other("background writer stopped"))),
        };
        let op = self.file_op.take().unwrap();
        match (op.kind, r) {
            (OpKind::Save { edit_id, target, ver, sig, keep, t0 }, OpResult::Temp(tw)) => {
                let tw = match tw {
                    Ok(tw) => tw,
                    Err(e) => {
                        self.toast(format!("Save failed: {e} (the original file was not changed)"));
                        return false;
                    }
                };
                let Some(d) = self.tabs.iter_mut().find_map(|t| match t {
                    Tab::Doc(d) if d.edit_id == edit_id => Some(d),
                    _ => None,
                }) else {
                    let _ = std::fs::remove_file(&tw.tmp);
                    return false;
                };
                match d.doc.finish_save(&target, tw, ver, sig, keep) {
                    Ok(rep) => {
                        let bak = rep.backup.as_ref().and_then(|b| b.file_name()).map(|b| format!(" · previous version kept as {}", b.to_string_lossy())).unwrap_or_default();
                        let msg = format!("Saved {} ({}) in {:.1} s{}", d.doc.title(), util::fmt_bytes(rep.bytes), t0.elapsed().as_secs_f32(), bak);
                        self.settings.push_recent(&target);
                        self.settings.save();
                        self.toast(msg);
                    }
                    Err(e) => self.toast(format!("Save failed: {e} (the original file was not changed)")),
                }
            }
            (OpKind::Export { target }, OpResult::Bytes(r)) => match r {
                Ok(n) => self.toast(format!("Exported {} ({})", target.display(), util::fmt_bytes(n))),
                Err(e) => self.toast(format!("Export failed: {e}")),
            },
            (OpKind::Split { edit_id, ver, edits, before }, OpResult::Files(res)) => {
                let mut msgs = Vec::new();
                let mut made = Vec::new();
                if let Some(d) = self.tabs.iter_mut().find_map(|t| match t {
                    Tab::Doc(d) if d.edit_id == edit_id => Some(d),
                    _ => None,
                }) {
                    if d.doc.text.ver != ver {
                        msgs.push("The text changed while writing; the *Include lines were not inserted.".to_string());
                    } else {
                        // all range replacements, applied from the end so earlier offsets stay valid
                        let mut ops: Vec<(usize, usize, String)> = Vec::new();
                        for (i, e) in edits.iter().enumerate() {
                            match &res[i] {
                                Ok(n) => {
                                    for (k, &(st, en)) in e.ranges.iter().enumerate() {
                                        let slice = &d.doc.text.s[st..en];
                                        for (_, ls, le) in util::lines(slice) {
                                            if let Some((id, _)) = placeholder_id(&slice[ls..le]) {
                                                d.doc.known_bulks.remove(&id);
                                            }
                                        }
                                        let with = if k == 0 {
                                            let nl = if slice.ends_with('\n') { "\n" } else { "" };
                                            format!("*Include, input={}{nl}", e.inc)
                                        } else {
                                            String::new()
                                        };
                                        ops.push((st, en, with));
                                    }
                                    made.push(format!("{} ({})", e.inc, util::fmt_bytes(*n)));
                                }
                                Err(err) => msgs.push(format!("Could not write {}: {err}", e.inc)),
                            }
                        }
                        ops.sort_by_key(|o| std::cmp::Reverse(o.0));
                        for (st, en, with) in ops {
                            d.doc.text.replace_range(st..en, &with);
                        }
                        d.doc.text.break_group();
                        if !made.is_empty() {
                            d.op_undo = Some(("Save as *Include".into(), before));
                            d.completion = None;
                            d.start_mesh();
                        }
                    }
                }
                if !made.is_empty() {
                    let shown = if made.len() > 6 { format!("{} files ({}, … {})", made.len(), made[0], made[made.len() - 1]) } else { made.join(", ") };
                    msgs.push(format!("Wrote {}. Save the main file to keep the *Include line{}.", shown, if made.len() > 1 { "s" } else { "" }));
                }
                for m in msgs {
                    self.toast(m);
                }
            }
            _ => self.toast("Unexpected result from a background file operation."),
        }
        false
    }

    fn file_op_ui(&mut self, ctx: &egui::Context) {
        let Some(op) = &self.file_op else { return };
        let done = op.prog.done.load(std::sync::atomic::Ordering::Relaxed);
        let total = op.prog.total.load(std::sync::atomic::Ordering::Relaxed).max(1);
        egui::Modal::new(egui::Id::new("file_op")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(&op.label).strong());
            });
            ui.add_space(6.0);
            ui.add(egui::ProgressBar::new((done as f32 / total as f32).min(1.0)).show_percentage());
            ui.label(RichText::new(format!("{} of {}", util::fmt_bytes(done.min(total)), util::fmt_bytes(total))).small().weak());
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn compare(&mut self) {
        if !matches!(self.tabs.get(self.active), Some(Tab::Doc(_))) {
            return;
        }
        let dir = self.doc_tab().map(|d| d.doc.dir());
        self.ask_path(PathReq::Compare, "Compare with…", dir, "");
    }

    pub fn compare_with(&mut self, b_path: PathBuf) {
        let Some(d) = self.doc_tab() else { return };
        let a_text = d.doc.text.s.clone();
        let a_name = d.doc.title();
        let a_path = d.doc.path.clone().unwrap_or_default();
        let dir = d.doc.dir();
        let th = self.settings.bulk_threshold;
        let (tx, rx) = channel();
        let label = format!("{} ↔ {}", a_name, b_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        std::thread::spawn(move || {
            let r = (|| -> Result<DiffTab, String> {
                let p = Progress::new();
                let lb = load(&b_path, th, &p).map_err(|e| e.to_string())?;
                let aa = analysis::analyze(&a_text, 0, &dir);
                let ab = analysis::analyze(&lb.text, 0, b_path.parent().unwrap_or(Path::new(".")));
                let ba = diff::blocks(&a_text, &aa);
                let bb = diff::blocks(&lb.text, &ab);
                let b_name = b_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let res = diff::compute(a_name, ba, b_name, bb);
                Ok(DiffTab {
                    res: Arc::new(res),
                    a_real: LineMap::build(&a_text, 0).real,
                    b_real: LineMap::build(&lb.text, 0).real,
                    a_path,
                    b_path,
                    show_same: false,
                    filter: String::new(),
                })
            })();
            let _ = tx.send(r);
        });
        self.diff_jobs.push(DiffJob { rx, label });
    }

    fn close_tab(&mut self, i: usize, force: bool) {
        if let Some(Tab::Doc(d)) = self.tabs.get(i) {
            if d.doc.modified() && !force {
                self.dlg.confirm = Some(Confirm::CloseUnsaved { tab: i });
                return;
            }
        }
        if i < self.tabs.len() {
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
    }

    /// Tools → Save all part meshes (Part/Assembly input) or Save mesh (flat input);
    /// `only` = one part (outline).
    fn split_meshes(&mut self, only: Option<usize>) {
        let Some(d) = self.doc_tab() else { return };
        let Some(an) = d.current_analysis().map(|_| d.analysis.clone().unwrap()) else {
            self.toast("Analysis is still running; try again in a moment.");
            return;
        };
        let parts: Vec<Option<usize>> = match only {
            Some(pb) => vec![Some(pb)],
            None if an.summary.assembly_based => an.blocks.iter().enumerate().filter(|(_, b)| b.norm == "PART").map(|(i, _)| Some(i)).collect(),
            None => vec![None],
        };
        let title = match only {
            Some(pb) => format!("Save the mesh of part {} as *Include files", an.blocks[pb].param("NAME").unwrap_or("").trim_matches('"')),
            None if an.summary.assembly_based => "Save all part meshes as *Include files".into(),
            None => "Save the mesh as *Include files".into(),
        };
        self.open_batch(title, an, BatchKind::Meshes { parts, mode: MeshSplit::Combined });
        if self.dlg.batch.as_ref().is_some_and(|b| b.items.is_empty()) {
            self.dlg.batch = None;
            self.toast("No *Node / *Element blocks found.");
        }
    }

    /// Tools → Save data blocks: keyword blocks with at least N data lines, one file each.
    fn split_all_bulks(&mut self) {
        let Some(d) = self.doc_tab() else { return };
        let Some(an) = d.current_analysis().map(|_| d.analysis.clone().unwrap()) else {
            self.toast("Analysis is still running; try again in a moment.");
            return;
        };
        let th = d.doc.threshold;
        let blocks: Vec<usize> = an
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.data_lines > 0 && !matches!(b.norm.as_str(), "INCLUDE" | "HEADING") && !b.norm.starts_with("END"))
            .map(|(i, _)| i)
            .collect();
        if blocks.is_empty() {
            self.toast("This file has no keyword blocks with data lines.");
            return;
        }
        self.open_batch("Save data blocks as *Include files".into(), an, BatchKind::Blocks { blocks, min_lines: Some(th) });
    }

    fn split_each(&mut self, blocks: &[usize], title: String) {
        let Some(d) = self.doc_tab() else { return };
        let Some(an) = d.current_analysis().map(|_| d.analysis.clone().unwrap()) else {
            self.toast("Analysis is still running; try again in a moment.");
            return;
        };
        self.open_batch(title, an, BatchKind::Blocks { blocks: blocks.to_vec(), min_lines: None });
    }

    fn open_batch(&mut self, title: String, an: Arc<crate::analysis::Analysis>, kind: BatchKind) {
        if self.doc_tab().is_some_and(|d| d.doc.path.is_none()) {
            self.toast("Save the file first: *Include files are written next to it and referenced relative to it.");
            return;
        }
        let Some((ver, dir, stem)) = self.doc_tab().map(|d| (d.doc.text.ver, d.doc.dir(), d.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string())) else { return };
        let folder = self.settings.include_folder(&dir).to_string_lossy().into_owned();
        let mut b = BatchDlg { title, folder, kind, an, stem, items: Vec::new(), total: 0, ver, overwrite: false, err: String::new() };
        b.rebuild();
        self.dlg.batch = Some(b);
    }

    // ------------------------------------------------------------ actions

    fn handle(&mut self, acts: Vec<Action>) {
        for a in acts {
            match a {
                Action::OpenFile(p, j) => self.open(p, j),
                Action::Toast(s) => self.toast(s),
                Action::ShowRefs => {
                    self.bottom = Bottom::Refs;
                    self.settings.show_bottom = true;
                }
                Action::OpenViewer { bulk, row } => {
                    if let Some(d) = self.doc_tab() {
                        d.viewer = Some(BulkViewer::new(bulk, row));
                        self.bottom = Bottom::Data;
                        self.settings.show_bottom = true;
                    }
                }
                Action::SplitBlock { .. } | Action::SplitEach { .. } | Action::SplitPartMesh { .. } if self.doc_tab().is_some_and(|d| d.doc.path.is_none()) => {
                    self.toast("Save the file first: *Include files are written next to it and referenced relative to it.");
                }
                Action::SplitBlock { ranges, suggested, label } => {
                    let inc_dir = self.doc_tab().map(|d| d.doc.dir()).map(|dd| self.settings.include_folder(&dd));
                    if let (Some(d), Some(folder)) = (self.doc_tab(), inc_dir) {
                        let ver = d.doc.text.ver;
                        let folder = folder.to_string_lossy().into_owned();
                        self.dlg.split = Some(SplitDlg { ranges, folder, name: suggested, label, ver, err: String::new() });
                    }
                }
                Action::SplitEach { blocks, title } => self.split_each(&blocks, title),
                Action::SplitPartMesh { part_block } => self.split_meshes(Some(part_block)),
                Action::InlineInclude { start, end, path } => {
                    if let Some(d) = self.doc_tab() {
                        let (edit_id, ver) = (d.edit_id, d.doc.text.ver);
                        let prog = Progress::new();
                        let p2 = prog.clone();
                        let th = self.settings.bulk_threshold;
                        let (tx, rx) = channel();
                        let pc = path.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(load(&pc, th, &p2));
                        });
                        self.loads.push(LoadJob { path, prog, rx, after: After::Inline { edit_id, start, end, ver } });
                    }
                }
                Action::ExpandBulk { bulk } => {
                    let Some(d) = self.doc_tab() else { continue };
                    let Some(b) = STORE.get(bulk) else { continue };
                    if !crate::viewer::can_expand(&b) {
                        self.dlg.too_big = Some(bulk);
                        continue;
                    }
                    d.ensure_map();
                    let Some(&line) = d.map.bulk_line.get(&bulk) else { continue };
                    let s = d.map.starts[line];
                    let e = d.map.starts.get(line + 1).map(|x| x - 1).unwrap_or(d.doc.text.s.len());
                    let bytes = b.bytes();
                    let mut txt = if d.doc.latin1 { bytes.iter().map(|&c| c as char).collect::<String>() } else { String::from_utf8_lossy(bytes).into_owned() };
                    txt = txt.replace("\r\n", "\n");
                    d.edit("Edit data as text", s..e, &txt);
                    d.doc.known_bulks.remove(&bulk);
                    d.start_mesh();
                    if d.viewer.as_ref().map(|v| v.bulk) == Some(bulk) {
                        d.viewer = None;
                    }
                    self.toast(format!("{} lines loaded into the editor", util::fmt_int(b.lines)));
                }
            }
        }
    }

    // ------------------------------------------------------------ UI pieces

    fn menu(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New").shortcut_text("Ctrl+N")).clicked() {
                    ui.close();
                    self.new_file();
                }
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    ui.close();
                    self.ask_path(PathReq::Open, "Open Abaqus input file", None, "");
                }
                ui.menu_button("Open recent", |ui| {
                    let rec = self.settings.recent.clone();
                    if rec.is_empty() {
                        ui.label("(none)");
                    }
                    for r in rec {
                        if ui.button(&r).clicked() {
                            ui.close();
                            self.open(PathBuf::from(r), None);
                        }
                    }
                });
                ui.separator();
                let has_doc = matches!(self.tabs.get(self.active), Some(Tab::Doc(_)));
                if ui.add_enabled(has_doc, egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    ui.close();
                    self.save(self.active, false, false);
                }
                if ui.add_enabled(has_doc, egui::Button::new("Save As…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    ui.close();
                    self.save(self.active, true, false);
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Export without mesh…")).clicked() {
                    ui.close();
                    self.export_nomesh();
                }
                if ui.add_enabled(has_doc, egui::Button::new("Compare with file…")).clicked() {
                    ui.close();
                    self.compare();
                }
                ui.separator();
                if ui.add_enabled(!self.tabs.is_empty(), egui::Button::new("Close tab").shortcut_text("Ctrl+W")).clicked() {
                    ui.close();
                    self.close_tab(self.active, false);
                }
                if ui.button("Exit").clicked() {
                    ui.close();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let has_doc = matches!(self.tabs.get(self.active), Some(Tab::Doc(_)));
                if ui.add_enabled(has_doc, egui::Button::new("Find").shortcut_text("Ctrl+F")).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        d.find.open = true;
                        d.find.focus = true;
                    }
                }
                if ui.add_enabled(has_doc, egui::Button::new("Go to line / node / element…").shortcut_text("Ctrl+G")).clicked() {
                    ui.close();
                    self.open_goto();
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Go to definition").shortcut_text("F12 / Ctrl+click")).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        go_to_definition(d, acts);
                    }
                }
                if ui.add_enabled(has_doc, egui::Button::new("Find references").shortcut_text("Shift+F12")).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        find_references(d, acts);
                    }
                }
                if ui.add_enabled(has_doc, egui::Button::new("Complete keyword / parameter").shortcut_text("Ctrl+Space")).clicked() {
                    ui.close();
                }
                ui.separator();
                let undo = self.doc_tab().and_then(|d| d.op_undo.as_ref().map(|u| u.0.clone()));
                if ui.add_enabled(undo.is_some(), egui::Button::new(format!("Undo: {}", undo.clone().unwrap_or_else(|| "last operation".into())))).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        if let Some((_, old)) = d.op_undo.take() {
                            d.doc.text.set(old);
                        }
                    }
                }
            });
            ui.menu_button("Tools", |ui| {
                let has_doc = matches!(self.tabs.get(self.active), Some(Tab::Doc(_)));
                let has_sel = self.doc_tab().is_some_and(|d| d.sel.0 != d.sel.1);
                if ui.add_enabled(has_sel, egui::Button::new("Save selection as *Include file…")).on_disabled_hover_text("Select lines in the editor first (drag, Shift+click or Shift+arrows)").clicked() {
                    ui.close();
                    if let Some(a) = self.doc_tab().and_then(crate::editor::selection_split_action) {
                        acts.push(a);
                    }
                }
                if ui.add_enabled(has_doc, egui::Button::new("Save block at cursor as *Include file…")).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        if let Some(an) = d.current_analysis() {
                            if let Some(bi) = an.block_at_line(d.cursor.0) {
                                let stem = d.doc.title().trim_end_matches(".inp").to_string();
                                acts.push(crate::outline::block_split_action(an, bi, &stem));
                            }
                        }
                    }
                }
                let assembly = self.doc_tab().and_then(|d| d.analysis.as_ref().map(|a| a.summary.assembly_based)).unwrap_or(true);
                let mesh_label = if assembly { "Save all part meshes as *Include files…" } else { "Save mesh (*Node / *Element) as *Include file…" };
                if ui.add_enabled(has_doc, egui::Button::new(mesh_label)).clicked() {
                    ui.close();
                    self.split_meshes(None);
                }
                if ui.add_enabled(has_doc, egui::Button::new("Save data blocks as *Include files…")).on_hover_text("Every keyword block with at least N data lines goes into its own file (N is chosen in the dialog)").clicked() {
                    ui.close();
                    self.split_all_bulks();
                }
                ui.separator();
                if ui.add_enabled(has_doc, egui::Button::new("Model summary")).clicked() {
                    ui.close();
                    self.dlg.summary = true;
                }
                if ui.add_enabled(has_doc, egui::Button::new("Re-run checks").shortcut_text("F5")).clicked() {
                    ui.close();
                    if let Some(d) = self.doc_tab() {
                        d.start_analysis();
                        d.start_mesh();
                    }
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    ui.close();
                    self.dlg.settings = true;
                }
            });
            ui.menu_button("View", |ui| {
                let mut dark = self.settings.dark;
                if ui.checkbox(&mut dark, "Dark mode").clicked() {
                    self.settings.dark = dark;
                    crate::theme::set_dark(dark);
                    setup_style(ui.ctx());
                    self.link_icons = None;
                    self.settings.save();
                }
                ui.separator();
                ui.checkbox(&mut self.settings.show_outline, "Outline");
                ui.checkbox(&mut self.settings.show_bottom, "Problems / data panel");
                ui.separator();
                if ui.button("Larger font  (Ctrl +)").clicked() {
                    self.settings.font_size = (self.settings.font_size + 1.0).min(28.0);
                }
                if ui.button("Smaller font  (Ctrl -)").clicked() {
                    self.settings.font_size = (self.settings.font_size - 1.0).max(8.0);
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    ui.close();
                    self.dlg.about = true;
                }
            });
            ui.menu_button("Information", |ui| {
                if ui.button("Disclaimer").clicked() {
                    ui.close();
                    self.dlg.disclaimer = true;
                }
                if ui.button("Developer").clicked() {
                    ui.close();
                    self.dlg.developer = true;
                }
            });
        });
    }

    fn open_goto(&mut self) {
        self.dlg.goto = Some(GotoDlg { mode: GotoMode::Line, text: String::new(), results: Vec::new(), msg: String::new(), focus: true, pending: None, searched: None, idx: 0 });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        egui::ScrollArea::horizontal().id_salt("tabbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let (title, modified, tip) = match t {
                        Tab::Doc(d) => (d.doc.title(), d.doc.modified(), d.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()),
                        Tab::Diff(d) => (format!("{} ↔ {}", d.res.a_name, d.res.b_name), false, "keyword-aware compare".into()),
                    };
                    let sel = i == self.active;
                    let frame = egui::Frame::new()
                        .fill(if sel { crate::theme::t(Color32::WHITE) } else { crate::theme::rgb(236, 238, 242) })
                        .stroke(egui::Stroke::new(1.0_f32, if sel { crate::theme::rgb(200, 205, 215) } else { crate::theme::rgb(225, 228, 233) }))
                        .corner_radius(egui::CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 })
                        .inner_margin(egui::Margin::symmetric(10, 4));
                    frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let txt = RichText::new(format!("{}{}", title, if modified { "  ●" } else { "" }));
                            let txt = if sel { txt.strong() } else { txt };
                            if ui.add(egui::Label::new(txt).sense(egui::Sense::click())).on_hover_text(tip).clicked() {
                                self.active = i;
                            }
                            if ui.add(egui::Button::new(RichText::new("×").color(crate::theme::t(Color32::GRAY))).frame(false)).clicked() {
                                close = Some(i);
                            }
                        });
                    });
                }
                for l in &self.loads {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new(l.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()).weak());
                }
                for j in &self.diff_jobs {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new(format!("comparing {}", j.label)).weak());
                }
            });
        });
        if let Some(i) = close {
            self.close_tab(i, false);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.tabs.get(self.active) {
                Some(Tab::Doc(d)) => {
                    let real = d.map.real.get(d.cursor.0).copied().unwrap_or(1);
                    ui.label(RichText::new(d.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let diags = d.all_diags();
                        let ne = diags.iter().filter(|x| x.sev == Sev::Error).count();
                        let nw = diags.len() - ne;
                        ui.label(RichText::new(format!("● {nw}")).color(crate::theme::t(Palette::WARN)));
                        ui.label(RichText::new(format!("● {ne}")).color(crate::theme::t(Palette::ERR)));
                        ui.separator();
                        if d.doc.path.is_some() {
                            ui.label(RichText::new(format!("opened in {:.2} s", d.doc.load_ms as f64 / 1000.0)).small().weak());
                        } else {
                            ui.label(RichText::new("new file, not saved yet").small().weak());
                        }
                        ui.separator();
                        ui.label(RichText::new(util::fmt_bytes(d.doc.file_len)).small());
                        ui.separator();
                        if let Some(a) = &d.analysis {
                            let (t, tip) = if a.summary.assembly_based {
                                ("Part / Assembly", "Input defined as an assembly of part instances: node/element labels are local to each part; mesh export works per part")
                            } else {
                                ("Flat input", "No *PART / *ASSEMBLY: the whole model is one mesh; mesh export writes one file")
                            };
                            ui.label(RichText::new(t).small().color(crate::theme::rgb(60, 90, 150))).on_hover_text(tip);
                            ui.separator();
                        }
                        ui.label(RichText::new(if d.doc.latin1 { "Latin-1" } else { "UTF-8" }).small());
                        ui.label(RichText::new(format!("{}{}", d.doc.eol.label(), if d.doc.mixed_eol { " (mixed)" } else { "" })).small());
                        ui.separator();
                        ui.label(RichText::new(format!("Ln {}, Col {}", util::fmt_int(real), d.cursor.1 + 1)).small());
                    });
                }
                Some(Tab::Diff(_)) => {
                    ui.label(RichText::new("Compare view").small().weak());
                }
                None => {
                    ui.label(RichText::new("InpLite").small().weak());
                }
            }
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            ui.label(RichText::new("InpLite").size(34.0).strong().color(crate::theme::rgb(30, 60, 120)));
            ui.label(RichText::new("lightweight editor for Abaqus input files").size(15.0).weak());
            ui.add_space(24.0);
            if !self.loads.is_empty() {
                for l in &self.loads {
                    ui.label(format!("Opening {} …", l.path.display()));
                    ui.add(egui::ProgressBar::new(l.prog.frac()).desired_width(420.0).show_percentage());
                    if ui.button("Cancel").clicked() {
                        l.prog.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                return;
            }
            if ui.add(egui::Button::new(RichText::new("  Open file…  ").size(16.0)).min_size(egui::vec2(180.0, 36.0))).clicked() {
                self.ask_path(PathReq::Open, "Open Abaqus input file", None, "");
            }
            ui.add_space(6.0);
            ui.label(RichText::new("or drop an .inp file here  ·  Ctrl+O").weak());
            ui.add_space(24.0);
            if !self.settings.recent.is_empty() {
                ui.label(RichText::new("Recent").strong());
                for r in self.settings.recent.clone().into_iter().take(8) {
                    if ui.link(&r).clicked() {
                        self.open(PathBuf::from(r), None);
                    }
                }
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // ---- go to
        let mut close_goto = false;
        let mut nav: Option<Hit> = None;
        let mut nav_line: Option<u64> = None;
        if let Some(g) = self.dlg.goto.as_mut() {
            if let Some(rx) = &g.pending {
                if let Ok(res) = rx.try_recv() {
                    g.pending = None;
                    g.results = res;
                    g.idx = 0;
                    if g.results.is_empty() {
                        g.msg = "not found (labels inside *Include files are not searched)".into();
                        g.searched = None;
                    } else if g.results.len() == 1 {
                        nav = Some(g.results[0].1.clone());
                        close_goto = true;
                    } else {
                        // the same label exists in several parts/instances: go to the first,
                        // Enter again goes to the next one
                        nav = Some(g.results[0].1.clone());
                        g.focus = true;
                    }
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
            }
            let mut open = true;
            egui::Window::new("Go to")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .pivot(egui::Align2::CENTER_TOP)
                .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 80.0))
                .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut g.mode, GotoMode::Line, "Line");
                    ui.radio_value(&mut g.mode, GotoMode::Node, "Node label");
                    ui.radio_value(&mut g.mode, GotoMode::Element, "Element label");
                });
                let r = ui.add(egui::TextEdit::singleline(&mut g.text).desired_width(f32::INFINITY).hint_text("number"));
                if g.focus {
                    r.request_focus();
                    g.focus = false;
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let go = enter || ui.button("Go").clicked();
                let parsed = g.text.replace([',', ' '], "").parse::<u64>().ok();
                let again = go && g.pending.is_none() && g.results.len() > 1 && parsed.is_some() && g.searched == parsed.map(|n| (g.mode, n));
                if again {
                    g.idx = (g.idx + 1) % g.results.len();
                    nav = Some(g.results[g.idx].1.clone());
                    g.focus = true;
                } else if go {
                    g.msg.clear();
                    g.results.clear();
                    g.searched = None;
                    match parsed.ok_or(()) {
                        Err(_) => g.msg = "enter a number".into(),
                        Ok(n) => match g.mode {
                            GotoMode::Line => {
                                nav_line = Some(n);
                                close_goto = true;
                            }
                            mode => {
                                if let Some(Tab::Doc(d)) = self.tabs.get(self.active) {
                                    match &d.mesh {
                                        None => g.msg = "the mesh index is still being built — try again in a moment".into(),
                                        Some(m) => {
                                            // search in the background: unsorted blocks need a full scan
                                            let kind = if mode == GotoMode::Node { IdKind::Node } else { IdKind::Element };
                                            let (m, text) = (m.clone(), d.doc.text.s.clone());
                                            let (tx, rx) = channel();
                                            std::thread::spawn(move || {
                                                let _ = tx.send(m.find(kind, n, &text));
                                            });
                                            g.pending = Some(rx);
                                            g.searched = Some((mode, n));
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
                if g.pending.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("searching…");
                    });
                }
                if !g.msg.is_empty() {
                    ui.label(RichText::new(&g.msg).color(crate::theme::t(Palette::ERR)));
                }
                if g.results.len() > 1 {
                    ui.label(format!("Match {} of {} — labels are local to each part/instance; press Enter for the next one:", g.idx + 1, g.results.len()));
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for (i, (scope, h)) in g.results.iter().enumerate() {
                            if ui.selectable_label(i == g.idx, scope).clicked() {
                                nav = Some(h.clone());
                                g.idx = i;
                                g.focus = true;
                            }
                        }
                    });
                }
            });
            if !open {
                close_goto = true;
            }
        }
        if close_goto {
            self.dlg.goto = None;
        }
        if let Some(n) = nav_line {
            if let Some(d) = self.doc_tab() {
                d.jump_to_real(n);
                if d.viewer.is_some() {
                    self.bottom = Bottom::Data;
                }
            }
        }
        if let Some(h) = nav {
            // while the dialog stays open (several matches) the keyboard stays in the dialog
            let focus_editor = self.dlg.goto.is_none();
            if let Some(d) = self.doc_tab() {
                match h {
                    Hit::Text { line } => {
                        let s = d.map.starts.get(line).copied().unwrap_or(0);
                        let e = d.map.starts.get(line + 1).map(|x| x - 1).unwrap_or(d.doc.text.s.len());
                        d.jump = Some(crate::tab::Jump { line, sel: Some((s, e)), focus: focus_editor });
                    }
                    Hit::Bulk { id, row } => {
                        d.ensure_map();
                        if let Some(&l) = d.map.bulk_line.get(&id) {
                            d.jump = Some(crate::tab::Jump { line: l, sel: None, focus: focus_editor });
                        }
                        d.viewer = Some(BulkViewer::new(id, Some(row)));
                        self.bottom = Bottom::Data;
                        self.settings.show_bottom = true;
                    }
                }
            }
        }

        // ---- split to include
        let mut close_split = false;
        let mut do_split = false;
        let mut split_now: Option<SplitEdit> = None;
        let doc_dir = self.doc_tab().map(|d| d.doc.dir());
        let mut browse = false;
        if let Some(s) = self.dlg.split.as_mut() {
            egui::Window::new("Save as *Include file").collapsible(false).resizable(false).pivot(egui::Align2::CENTER_TOP).default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 80.0)).show(ctx, |ui| {
                ui.label(format!("Save {} into a separate file and replace it with an *Include line.", s.label));
                ui.add_space(4.0);
                egui::Grid::new("split_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Folder");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut s.folder).desired_width(360.0));
                        if ui.button("Browse…").on_hover_text("Choose the folder and file name").clicked() {
                            browse = true;
                        }
                    });
                    ui.end_row();
                    ui.label("File name");
                    ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(360.0));
                    ui.end_row();
                });
                let inc = doc_dir.as_ref().map(|d| include_ref(d, &PathBuf::from(s.folder.trim()).join(s.name.trim()))).unwrap_or_default();
                ui.add_space(2.0);
                ui.label(RichText::new(format!("*Include, input={inc}")).monospace().color(crate::theme::t(Palette::KEYWORD)));
                ui.label(RichText::new("Files outside the input file's folder are referenced with their full path. The main file changes only when you save.").small().weak());
                if !s.err.is_empty() {
                    ui.label(RichText::new(&s.err).color(crate::theme::t(Palette::ERR)));
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        do_split = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close_split = true;
                    }
                });
            });
        }
        if browse {
            let s = self.dlg.split.as_mut().unwrap();
            let folder = PathBuf::from(s.folder.trim());
            let dir = if folder.is_dir() { Some(folder) } else { doc_dir.clone() };
            let name = s.name.trim().to_string();
            self.ask_path(PathReq::SplitFile, "Save *Include file as", dir, &name);
        }
        if do_split {
            let s = self.dlg.split.as_mut().unwrap();
            if let Some(Tab::Doc(d)) = self.tabs.get_mut(self.active) {
                let name = s.name.trim().to_string();
                let folder = PathBuf::from(s.folder.trim());
                let path = folder.join(&name);
                if d.doc.text.ver != s.ver {
                    s.err = "The text changed since this was opened; close and try again.".into();
                } else if name.is_empty() || name.contains(['/', '\\']) {
                    s.err = "Enter a plain file name (choose the folder with Browse…).".into();
                } else if !folder.is_dir() && std::fs::create_dir_all(&folder).is_err() {
                    s.err = format!("Cannot create the folder {}", folder.display());
                } else if same_file(&path, d.doc.path.as_deref()) || d.doc.src.as_ref().is_some_and(|src| same_file(&path, Some(&src.path))) {
                    s.err = "That is the input file itself. Choose another name.".into();
                } else if path.exists() && !s.err.starts_with("Overwrite?") {
                    s.err = format!("Overwrite? {name} already exists in that folder; press Save again to replace it.");
                } else {
                    split_now = Some(SplitEdit { ranges: s.ranges.clone(), inc: include_ref(&d.doc.dir(), &path) });
                    close_split = true;
                }
            }
        }
        if let Some(e) = split_now {
            self.start_split(vec![e]);
        }
        if close_split {
            self.dlg.split = None;
        }

        // ---- "Edit as text" refused: explain and offer the alternatives
        if let Some(id) = self.dlg.too_big {
            let mut open = true;
            let mut act: Option<Action> = None;
            if let Some(b) = STORE.get(id) {
                let split = self.doc_tab().and_then(|d| {
                    let stem = d.doc.title().trim_end_matches(".inp").trim_end_matches(".INP").to_string();
                    d.current_analysis().and_then(|an| an.blocks.iter().position(|bl| bl.bulks.contains(&id)).map(|bi| crate::outline::block_split_action(an, bi, &stem)))
                });
                egui::Window::new("Edit as text")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .min_width(480.0)
                    .max_width(520.0)
                    .pivot(egui::Align2::CENTER_TOP)
                    .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 100.0))
                    .show(ctx, |ui| {
                        ui.label(RichText::new(&b.kw_line).monospace().strong().color(crate::theme::t(Palette::KEYWORD)));
                        ui.add_space(4.0);
                        ui.add(egui::Label::new(crate::viewer::expand_refusal(&b)).wrap());
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.add_enabled(split.is_some(), egui::Button::new("Save as *Include file…")).clicked() {
                                act = split.clone();
                            }
                            if ui.button("Open in data viewer").clicked() {
                                act = Some(Action::OpenViewer { bulk: id, row: None });
                            }
                            if ui.button("Close").clicked() {
                                act = Some(Action::Toast(String::new()));
                            }
                        });
                    });
            }
            if let Some(a) = act {
                open = false;
                if !matches!(&a, Action::Toast(s) if s.is_empty()) {
                    self.pending_acts.push(a);
                }
            }
            if !open {
                self.dlg.too_big = None;
            }
        }

        // ---- batch split (all part meshes, all data blocks, a group of blocks)
        let mut close_batch = false;
        let mut run_batch = false;
        let mut browse_batch = false;
        if let Some(bd) = self.dlg.batch.as_mut() {
            let folder = PathBuf::from(bd.folder.trim());
            let existing = bd.items.iter().filter(|it| folder.join(&it.name).exists()).count();
            let mut open = true;
            egui::Window::new(bd.title.clone())
                .id(egui::Id::new("batch_split"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .max_width(600.0)
                .pivot(egui::Align2::CENTER_TOP)
                .default_pos(ctx.content_rect().center_top() + egui::vec2(0.0, 80.0))
                .show(ctx, |ui| {
                    ui.set_width(580.0);
                    let mut changed = false;
                    match &mut bd.kind {
                        BatchKind::Meshes { mode, .. } => {
                            ui.label(RichText::new("Split").strong());
                            changed |= ui.radio_value(mode, MeshSplit::Combined, "One file: everything from the first to the last *Node / *Element block (order kept)").changed();
                            changed |= ui.radio_value(mode, MeshSplit::NodesElements, "Two files: *Node and *Element separately (sets stay in the main file)").changed();
                            changed |= ui.radio_value(mode, MeshSplit::AllFour, "Four files: *Node, *Element, *Nset and *Elset separately").changed();
                            if *mode == MeshSplit::AllFour {
                                ui.label(
                                    RichText::new("All set definitions of a kind are moved together to the place of the first one. If a set refers to another set defined later (e.g. *Nset, elset=…), check the order.")
                                        .small()
                                        .color(crate::theme::t(Palette::WARN)),
                                );
                            }
                            ui.add_space(4.0);
                        }
                        BatchKind::Blocks { min_lines: Some(m), .. } => {
                            ui.horizontal(|ui| {
                                ui.label("Only blocks with at least");
                                changed |= ui
                                    .add(egui::DragValue::new(m).range(1..=1_000_000_000u64).speed(10.0).custom_formatter(|v, _| util::fmt_int(v as u64)).custom_parser(|t| t.replace([',', ' '], "").parse::<f64>().ok()))
                                    .changed();
                                ui.label("data lines");
                            });
                            ui.add_space(4.0);
                        }
                        _ => {}
                    }
                    if changed {
                        bd.rebuild();
                    }
                    let of_total = if matches!(bd.kind, BatchKind::Blocks { min_lines: Some(_), .. }) { format!(" ({} of {} keyword blocks with data)", bd.items.len(), bd.total) } else { String::new() };
                    ui.label(format!("{} file{} will be written{}; each block is replaced by an *Include line.", bd.items.len(), if bd.items.len() == 1 { "" } else { "s" }, of_total));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Folder");
                        ui.add(egui::TextEdit::singleline(&mut bd.folder).desired_width(440.0));
                        if ui.button("Browse…").clicked() {
                            browse_batch = true;
                        }
                    });
                    ui.add_space(4.0);
                    egui::Frame::new().stroke(egui::Stroke::new(1.0_f32, crate::theme::rgb(225, 228, 233))).inner_margin(6.0).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show_rows(ui, 18.0, bd.items.len(), |ui, range| {
                            for it in &bd.items[range] {
                                ui.horizontal(|ui| {
                                    let exists = folder.join(&it.name).exists();
                                    ui.add(egui::Label::new(RichText::new(&it.name).monospace().color(if exists { crate::theme::t(Palette::WARN) } else { crate::theme::t(Palette::KEYWORD) })).truncate()).on_hover_text(&it.label);
                                    ui.add(egui::Label::new(RichText::new(&it.label).small().weak()).truncate());
                                });
                            }
                        });
                    });
                    if existing > 0 {
                        ui.checkbox(&mut bd.overwrite, format!("Overwrite the {existing} file(s) that already exist (otherwise they are skipped)"));
                    }
                    ui.label(RichText::new("The main file changes only when you save it.").small().weak());
                    if !bd.err.is_empty() {
                        ui.label(RichText::new(&bd.err).color(crate::theme::t(Palette::ERR)));
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!bd.items.is_empty(), egui::Button::new("Save")).clicked() {
                            run_batch = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_batch = true;
                        }
                    });
                });
            if !open {
                close_batch = true;
            }
        }
        if browse_batch {
            let cur = self.dlg.batch.as_ref().map(|b| PathBuf::from(b.folder.trim())).filter(|c| c.is_dir());
            self.ask_path(PathReq::BatchFolder, "Folder for the *Include files", cur, "");
        }
        if run_batch {
            let bd = self.dlg.batch.as_mut().unwrap();
            let folder = PathBuf::from(bd.folder.trim());
            if let Some(Tab::Doc(d)) = self.tabs.get_mut(self.active) {
                if d.doc.text.ver != bd.ver {
                    bd.err = "The text changed since this was opened; close and try again.".into();
                } else if !folder.is_dir() && std::fs::create_dir_all(&folder).is_err() {
                    bd.err = format!("Cannot create the folder {}", folder.display());
                } else {
                    let dd = d.doc.dir();
                    let mut edits = Vec::new();
                    let mut skipped = 0;
                    for it in &bd.items {
                        let path = folder.join(&it.name);
                        if same_file(&path, d.doc.path.as_deref()) {
                            skipped += 1;
                            continue;
                        }
                        if path.exists() && !bd.overwrite {
                            skipped += 1;
                            continue;
                        }
                        edits.push(SplitEdit { ranges: it.ranges.clone(), inc: include_ref(&dd, &path) });
                    }
                    if skipped > 0 {
                        self.toasts.push((format!("{skipped} existing file(s) skipped"), -1.0));
                    }
                    close_batch = true;
                    if !edits.is_empty() {
                        self.pending_split = Some(edits);
                    }
                }
            }
        }
        if close_batch {
            self.dlg.batch = None;
        }
        if let Some(e) = self.pending_split.take() {
            self.start_split(e);
        }

        // ---- confirmations
        let mut resolved: Option<(bool, usize, u8)> = None; // (yes, tab, kind)
        if let Some(c) = &self.dlg.confirm {
            let (title, msg, yes, kind, tab) = match c {
                Confirm::SaveDropping { tab, n, lines, .. } => (
                    "Data blocks were removed",
                    format!("{n} large data block(s) ({} lines) are no longer referenced in the text and will NOT be written.\nSave anyway?", util::fmt_int(*lines)),
                    "Save without them",
                    0u8,
                    *tab,
                ),
                Confirm::Overwrite { tab, .. } => ("File changed on disk", "The file was modified by another program since it was opened.\nOverwrite it? (the current disk version is kept as .bak)".to_string(), "Overwrite", 1, *tab),
                Confirm::CloseUnsaved { tab } => ("Unsaved changes", "Close this tab and discard the changes?".to_string(), "Discard", 2, *tab),
                Confirm::ExitUnsaved => ("Unsaved changes", "Some files have unsaved changes. Exit anyway?".to_string(), "Exit without saving", 3, 0),
            };
            egui::Window::new(title).collapsible(false).resizable(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center() - egui::vec2(0.0, 40.0)).show(ctx, |ui| {
                ui.label(msg);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(yes).color(crate::theme::t(Palette::ERR))).clicked() {
                        resolved = Some((true, tab, kind));
                    }
                    if ui.button("Cancel").clicked() {
                        resolved = Some((false, tab, kind));
                    }
                });
            });
        }
        if let Some((yes, tab, kind)) = resolved {
            let c = self.dlg.confirm.take();
            if yes {
                match (kind, c) {
                    (0, Some(Confirm::SaveDropping { path, .. })) | (1, Some(Confirm::Overwrite { path, .. })) => {
                        if let Some(Tab::Doc(d)) = self.tabs.get_mut(tab) {
                            if d.doc.path.as_ref() != Some(&path) {
                                d.doc.path = Some(path);
                            }
                        }
                        self.save(tab, false, true);
                    }
                    (2, _) => self.close_tab(tab, true),
                    (3, _) => {
                        self.exit_ok = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    _ => {}
                }
            }
        }

        // ---- summary
        if self.dlg.summary {
            let mut open = true;
            let max_h = (ctx.content_rect().height() - 120.0).max(240.0);
            egui::Window::new("Model summary")
                .open(&mut open)
                .default_width(600.0)
                .min_width(420.0)
                .max_width(900.0)
                .default_height(max_h.min(560.0))
                .max_height(max_h)
                .resizable(true)
                .show(ctx, |ui| {
                if let Some(Tab::Doc(d)) = self.tabs.get(self.active) {
                    summary_ui(ui, d);
                }
            });
            self.dlg.summary = open;
        }
        // ---- settings
        let mut browse_fixed = false;
        if self.dlg.settings {
            let mut open = true;
            egui::Window::new("Settings").open(&mut open).resizable(false).max_width(620.0).show(ctx, |ui| {
                egui::Grid::new("settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label("Keep data blocks on disk when longer than");
                    ui.add(egui::DragValue::new(&mut self.settings.bulk_threshold).range(2..=1_000_000).suffix(" lines"));
                    ui.end_row();
                    ui.label("Keep previous version as .bak when saving");
                    ui.checkbox(&mut self.settings.keep_backup, "");
                    ui.end_row();
                    ui.label("Editor font size");
                    ui.add(egui::Slider::new(&mut self.settings.font_size, 8.0..=28.0));
                    ui.end_row();
                    ui.label("Default folder for *Include files");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut self.settings.include_folder, IncludeFolder::SameAsInput, "Folder of the main input file");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.settings.include_folder, IncludeFolder::SubFolder, "Sub-folder");
                            ui.add_enabled(self.settings.include_folder == IncludeFolder::SubFolder, egui::TextEdit::singleline(&mut self.settings.include_subdir).desired_width(140.0));
                        });
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.settings.include_folder, IncludeFolder::Fixed, "This folder");
                            let en = self.settings.include_folder == IncludeFolder::Fixed;
                            ui.add_enabled(en, egui::TextEdit::singleline(&mut self.settings.include_fixed).desired_width(220.0));
                            if ui.add_enabled(en, egui::Button::new("Browse…")).clicked() {
                                browse_fixed = true;
                            }
                        });
                    });
                    ui.end_row();
                });
                ui.label(RichText::new("The data-block threshold applies to files opened afterwards. The *Include folder is only the default: every save dialog has a Browse… button. Sub-folders are created when needed and referenced with relative paths.").small().weak());
            });
            if !open {
                self.settings.save();
            }
            self.dlg.settings = open;
        }
        if browse_fixed {
            let cur = Some(PathBuf::from(self.settings.include_fixed.trim())).filter(|c| c.is_dir());
            self.ask_path(PathReq::IncludeFolderSetting, "Folder for *Include files", cur, "");
        }
        // ---- built-in file dialog (Linux)
        if let Some((fd, _)) = self.file_dlg.as_mut() {
            if let Some(outcome) = fd.show(ctx) {
                let (_, req) = self.file_dlg.take().unwrap();
                if let crate::filedlg::Outcome::Chosen(p) = outcome {
                    self.on_path(req, p);
                }
            }
        }
        // ---- information: disclaimer
        if self.dlg.disclaimer {
            let mut open = true;
            egui::Window::new("Disclaimer").open(&mut open).collapsible(false).resizable(false).min_width(520.0).max_width(560.0).show(ctx, |ui| {
                ui.label(RichText::new(format!("InpLite {}", env!("CARGO_PKG_VERSION"))).strong());
                ui.add_space(4.0);
                for (head, body) in DISCLAIMER {
                    ui.label(RichText::new(*head).strong());
                    ui.add(egui::Label::new(*body).wrap());
                    ui.add_space(6.0);
                }
            });
            self.dlg.disclaimer = open;
        }
        // ---- information: developer
        if self.dlg.developer {
            if self.link_icons.is_none() {
                let load = |name: &str, bytes: &[u8]| ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([64, 64], bytes), egui::TextureOptions::LINEAR);
                self.link_icons = Some(vec![
                    load("link_github", include_bytes!("../assets/link_github.rgba")),
                    load("link_linkedin", include_bytes!("../assets/link_linkedin.rgba")),
                    load("link_researchgate", include_bytes!("../assets/link_researchgate.rgba")),
                ]);
            }
            let icons = self.link_icons.clone().unwrap_or_default();
            let mut open = true;
            egui::Window::new("Developer").open(&mut open).collapsible(false).resizable(false).min_width(380.0).show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("InpLite {}", env!("CARGO_PKG_VERSION"))).weak());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Developed by Youngbin LIM").size(18.0).strong().color(crate::theme::rgb(40, 44, 52)));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Built with Claude (Anthropic)").color(crate::theme::rgb(110, 114, 122)));
                    ui.add_space(14.0);
                    let links = [
                        ("GitHub", "https://github.com/YB-LIM"),
                        ("LinkedIn", "https://www.linkedin.com/in/lyb0684/"),
                        ("ResearchGate", "https://www.researchgate.net/profile/Lim-Youngbin?ev=hdr_xprf"),
                    ];
                    // centre the row of links
                    let w = 3.0 * 118.0;
                    ui.allocate_ui_with_layout(egui::vec2(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        for (i, (name, url)) in links.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(118.0, 28.0), egui::Sense::click());
                            let hovered = resp.hovered();
                            let tint = if hovered { crate::theme::gray(25) } else { crate::theme::gray(95) };
                            if hovered {
                                ui.painter().rect_filled(rect, 4.0, crate::theme::gray(238));
                            }
                            if let Some(tex) = icons.get(i) {
                                let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(18.0, 18.0));
                                ui.painter().image(tex.id(), ir, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
                            }
                            ui.painter().text(egui::pos2(rect.left() + 33.0, rect.center().y), egui::Align2::LEFT_CENTER, *name, egui::FontId::proportional(13.5), tint);
                            let resp = resp.on_hover_text(*url).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(*url));
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new("Independent open-source project · not affiliated with Dassault Systèmes / SIMULIA").small().weak());
                    ui.add_space(4.0);
                });
            });
            self.dlg.developer = open;
        }
        // ---- about
        if self.dlg.about {
            let mut open = true;
            egui::Window::new("Keyboard shortcuts").open(&mut open).resizable(false).show(ctx, |ui| {
                ui.label(RichText::new(format!("InpLite {}", env!("CARGO_PKG_VERSION"))).strong());
                ui.label("Lightweight editor for Abaqus input (.inp) files.");
                ui.label(RichText::new(format!("Keyword checks: {} keywords from the {}", crate::schema::SCHEMA.kws.len(), crate::schema::SCHEMA.source)).small());
                ui.add_space(6.0);
                egui::Grid::new("keys").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (k, v) in [
                        ("Ctrl+O / Ctrl+S / Ctrl+Shift+S", "open / save / save as"),
                        ("Ctrl+F", "find in the editor"),
                        ("Ctrl+G", "go to line, node or element label"),
                        ("Ctrl+click, F12", "go to definition (sets, surfaces, materials, *Include)"),
                        ("Shift+F12", "find references"),
                        ("Ctrl+Space", "complete keyword / parameter / name"),
                        ("Ctrl+Z / Ctrl+Y", "undo / redo"),
                        ("Ctrl+W", "close tab"),
                        ("F5", "re-run checks"),
                        ("Ctrl + / Ctrl -", "font size"),
                    ] {
                        ui.label(RichText::new(k).monospace());
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
            self.dlg.about = open;
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for t in &mut self.toasts {
            if t.1 < 0.0 {
                t.1 = now + 5.0;
            }
        }
        self.toasts.retain(|t| t.1 > now);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0]).order(egui::Order::Foreground).show(ctx, |ui| {
            for (m, _) in &self.toasts {
                egui::Frame::popup(ui.style()).fill(crate::theme::rgb(44, 48, 58)).show(ui, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(RichText::new(m).color(crate::theme::t(Color32::WHITE)));
                });
                ui.add_space(4.0);
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

const NEW_FILE_TEMPLATE: &str = "*Heading\n\n** ----------------------------------------------------------------\n** Model definition\n** ----------------------------------------------------------------\n";

/// Information → Disclaimer
const DISCLAIMER: &[(&str, &str)] = &[
    (
        "Not an official SIMULIA product",
        "InpLite is an independent, personal open-source project. It is not a product of SIMULIA or Dassault Systèmes, and it is not developed, endorsed, supported, validated or distributed by Dassault Systèmes or any of its subsidiaries. The software has no relation to the company the developer works for.",
    ),
    (
        "Trademarks",
        "Abaqus, SIMULIA and Dassault Systèmes are trademarks or registered trademarks of Dassault Systèmes or its subsidiaries. They are mentioned only to describe the file format this software works with.",
    ),
    (
        "Use at your own risk",
        "The software is provided \"as is\", without warranty of any kind (MIT License). You are solely responsible for its use and for the results: check edited input files before running analyses and keep backups of your models. The developer is not liable for any damage, data loss or incorrect analysis results arising from the use of this software.",
    ),
    (
        "Checks are not Abaqus",
        "Syntax and model checks are based on the Abaqus Keywords Guide and may be incomplete or wrong. Abaqus itself (for example a datacheck run) is the reference.",
    ),
    (
        "Support",
        "Please do not contact SIMULIA support about this tool. Questions and bug reports go to the project page on GitHub.",
    ),
];

fn summary_ui(ui: &mut egui::Ui, d: &DocTab) {
    let Some(a) = d.analysis.as_ref() else {
        ui.label("Analyzing…");
        return;
    };
    let s = &a.summary;
    // long lists wrap inside the window and the whole summary scrolls vertically
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        let key_w = 150.0;
        let row = |ui: &mut egui::Ui, k: &str, v: String| {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(key_w, 18.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.set_min_width(key_w);
                    ui.label(RichText::new(k).strong());
                });
                ui.allocate_ui_with_layout(egui::vec2(w - key_w - 12.0, 18.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_max_width(w - key_w - 12.0);
                    ui.add(egui::Label::new(v).wrap());
                });
            });
            ui.add_space(2.0);
        };
        row(ui, "File", d.doc.title());
        row(ui, "Size", format!("{} · {} lines · {} keyword blocks", util::fmt_bytes(d.doc.file_len), util::fmt_int(d.map.total), util::fmt_int(s.keywords as u64)));
        row(ui, "Analysis", s.product.unwrap_or("(no steps)").to_string());
        row(ui, "Structure", if s.assembly_based { format!("Part / Assembly: {} part(s), {} instance(s)", s.parts.len(), s.instances.len()) } else { "flat input (no parts / assembly)".into() });
        if let Some(m) = &d.mesh {
            row(ui, "Nodes", util::fmt_int(m.total_nodes));
            row(ui, "Elements", util::fmt_int(m.total_elements));
        } else {
            row(ui, "Mesh", "indexing…".into());
        }
        row(ui, "Node / element sets", format!("{} / {}", util::fmt_int(s.nsets as u64), util::fmt_int(s.elsets as u64)));
        row(ui, "Surfaces", util::fmt_int(s.surfaces as u64));
        row(ui, "Materials", if s.materials.is_empty() { "-".into() } else { format!("{}: {}", s.materials.len(), s.materials.join(", ")) });
        row(ui, "Sections", util::fmt_int(s.sections as u64));
        row(ui, "Interactions", format!("{} contact pair(s){}, {} tie(s), {} coupling(s), {} interaction propert{}", s.contact_pairs, if s.general_contact { ", general contact" } else { "" }, s.ties, s.couplings, s.interactions, if s.interactions == 1 { "y" } else { "ies" }));
        row(ui, "Amplitudes", s.amplitudes.to_string());
        row(ui, "Parameters", s.parameters.to_string());
        if !s.includes.is_empty() {
            row(ui, "Includes", s.includes.iter().map(|(p, ok)| if *ok { p.clone() } else { format!("{p} (missing)") }).collect::<Vec<_>>().join(", "));
        }
        if let Some(m) = &d.mesh {
            ui.add_space(8.0);
            ui.label(RichText::new("Elements by type").strong());
            egui::Grid::new("etypes").num_columns(3).striped(true).spacing([24.0, 2.0]).show(ui, |ui| {
                for (scope, t, n) in &m.elements {
                    ui.label(scope);
                    ui.label(RichText::new(t).monospace());
                    ui.label(util::fmt_int(*n));
                    ui.end_row();
                }
            });
        }
        if !s.steps.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Steps").strong());
            egui::Grid::new("steps").num_columns(3).striped(true).spacing([24.0, 2.0]).show(ui, |ui| {
                for st in &s.steps {
                    ui.label(&st.name);
                    ui.label(&st.procedure);
                    ui.label(if st.nlgeom { "NLGEOM" } else { "" });
                    ui.end_row();
                }
            });
        }
    });
}

fn same_file(a: &std::path::Path, b: Option<&std::path::Path>) -> bool {
    match (std::fs::canonicalize(a), b.map(std::fs::canonicalize)) {
        (Ok(x), Some(Ok(y))) => x == y,
        _ => false,
    }
}

/// INPUT= value for an include file: relative to the input file's folder when the file is in
/// that folder (or below it, written with '/'), otherwise the full path; quoted when it contains
/// blanks or commas.
pub fn include_ref(doc_dir: &std::path::Path, target: &std::path::Path) -> String {
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dd = canon(doc_dir);
    let tdir = target.parent().map(canon).unwrap_or_default();
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let r = match tdir.strip_prefix(&dd) {
        Ok(rel) if rel.as_os_str().is_empty() => name,
        // relative paths with '/' work in Abaqus on Windows and on Linux (cluster runs)
        Ok(rel) => {
            let mut parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            parts.push(name);
            parts.join("/")
        }
        Err(_) => target.to_string_lossy().into_owned(),
    };
    if r.contains([' ', ',', '=']) { format!("\"{r}\"") } else { r }
}

pub fn setup_style(ctx: &egui::Context) {
    let mut v = if crate::theme::is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.widgets.inactive.bg_fill = crate::theme::rgb(236, 238, 242);
    v.widgets.hovered.weak_bg_fill = crate::theme::rgb(228, 232, 240);
    v.widgets.hovered.bg_fill = crate::theme::rgb(228, 232, 240);
    v.widgets.active.weak_bg_fill = crate::theme::rgb(214, 222, 236);
    v.widgets.active.bg_fill = crate::theme::rgb(214, 222, 236);
    v.widgets.open.weak_bg_fill = crate::theme::rgb(228, 232, 240);
    v.panel_fill = crate::theme::rgb(246, 247, 249);
    v.window_fill = crate::theme::t(Color32::WHITE);
    v.extreme_bg_color = crate::theme::t(Color32::WHITE);
    v.faint_bg_color = crate::theme::rgb(248, 249, 251);
    v.selection.bg_fill = crate::theme::rgb(196, 218, 252);
    v.selection.stroke = egui::Stroke::new(1.0_f32, crate::theme::rgb(40, 90, 180));
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, crate::theme::rgb(222, 225, 230));
    v.window_corner_radius = egui::CornerRadius::same(8);
    let edge = egui::Stroke::new(1.0_f32, crate::theme::rgb(204, 208, 216));
    v.widgets.inactive.bg_stroke = edge;
    v.widgets.inactive.weak_bg_fill = crate::theme::rgb(242, 243, 246);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, crate::theme::rgb(150, 165, 195));
    ctx.set_visuals(v);
    // Fall back to the monospace font for symbols missing in the proportional font (●, →, ≡, ↑, …)
    let mut fonts = egui::FontDefinitions::default();
    if let Some(fam) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        fam.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        use egui::{FontFamily, FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(13.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional));
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.save();
    }
}

impl App {
    /// One UI frame (separate from eframe so tests can drive the whole UI).
    pub fn frame(&mut self, ctx: &egui::Context) {
        let mut acts: Vec<Action> = std::mem::take(&mut self.pending_acts);
        // dropped files
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            self.open(p, None);
        }
        // shortcuts
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let busy_writing = self.file_op.is_some();
        let mut new_req = false;
        let (open, save, save_as, find, goto, close, f5, plus, minus) = if busy_writing {
            (false, false, false, false, false, false, false, false, false)
        } else {
            ctx.input_mut(|i| {
            // egui ignores extra Shift when matching, so Ctrl+Shift+S must be consumed before Ctrl+S
            let save_as = i.consume_shortcut(&sc(Modifiers::COMMAND | Modifiers::SHIFT, Key::S));
            new_req = i.consume_shortcut(&sc(Modifiers::COMMAND, Key::N));
            (
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::O)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::S)),
                save_as,
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::F)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::G)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::W)),
                i.consume_key(Modifiers::NONE, Key::F5),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Plus)) || i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Equals)),
                i.consume_shortcut(&sc(Modifiers::COMMAND, Key::Minus)),
            )
            })
        };
        if new_req {
            self.new_file();
        }
        if open {
            let dir = self.doc_tab().map(|d| d.doc.dir());
            self.ask_path(PathReq::Open, "Open Abaqus input file", dir, "");
        }
        if save_as {
            self.save(self.active, true, false);
        } else if save {
            self.save(self.active, false, false);
        }
        if find {
            if let Some(d) = self.doc_tab() {
                d.find.open = true;
                d.find.focus = true;
            }
        }
        if goto && matches!(self.tabs.get(self.active), Some(Tab::Doc(_))) {
            self.open_goto();
        }
        if close && !self.tabs.is_empty() {
            self.close_tab(self.active, false);
        }
        if f5 {
            if let Some(d) = self.doc_tab() {
                d.start_analysis();
                d.start_mesh();
            }
        }
        if plus {
            self.settings.font_size = (self.settings.font_size + 1.0).min(28.0);
        }
        if minus {
            self.settings.font_size = (self.settings.font_size - 1.0).max(8.0);
        }
        // exit with unsaved changes
        if ctx.input(|i| i.viewport().close_requested()) && busy_writing {
            // never quit in the middle of writing a file
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if ctx.input(|i| i.viewport().close_requested()) && !self.exit_ok {
            let unsaved = self.tabs.iter().any(|t| matches!(t, Tab::Doc(d) if d.doc.modified()));
            if unsaved {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dlg.confirm = Some(Confirm::ExitUnsaved);
            } else {
                self.settings.save();
            }
        }

        let busy = self.poll_jobs(ctx) | self.poll_file_op();
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        // window title
        let title = match self.tabs.get(self.active) {
            Some(Tab::Doc(d)) => format!("{}{} — InpLite", d.doc.title(), if d.doc.modified() { " ●" } else { "" }),
            Some(Tab::Diff(d)) => format!("{} ↔ {} — InpLite", d.res.a_name, d.res.b_name),
            None => "InpLite".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::TopBottomPanel::top("menu").frame(egui::Frame::new().fill(crate::theme::rgb(250, 250, 251)).inner_margin(egui::Margin::symmetric(8, 3))).show(ctx, |ui| {
            self.menu(ui, &mut acts);
        });
        if !self.tabs.is_empty() || !self.loads.is_empty() || !self.diff_jobs.is_empty() {
            egui::TopBottomPanel::top("tabs").frame(egui::Frame::new().fill(crate::theme::rgb(236, 238, 242)).inner_margin(egui::Margin { left: 8, right: 8, top: 5, bottom: 0 })).show(ctx, |ui| {
                self.tab_bar(ui);
            });
        }
        egui::TopBottomPanel::bottom("status").frame(egui::Frame::new().fill(crate::theme::rgb(240, 242, 245)).inner_margin(egui::Margin::symmetric(10, 3))).show(ctx, |ui| {
            self.status_bar(ui);
        });

        let fs = self.settings.font_size;
        let show_bottom = self.settings.show_bottom;
        let show_outline = self.settings.show_outline;
        let mut bottom = self.bottom;
        let active = self.active;
        match self.tabs.get_mut(active) {
            Some(Tab::Doc(d)) => {
                if show_bottom {
                    egui::TopBottomPanel::bottom("bottom")
                        .resizable(true)
                        .default_height(250.0)
                        .min_height(80.0)
                        .frame(egui::Frame::new().fill(crate::theme::t(Color32::WHITE)).inner_margin(egui::Margin::symmetric(8, 6)).stroke(egui::Stroke::new(1.0_f32, crate::theme::rgb(225, 228, 233))))
                        .show(ctx, |ui| {
                            // fill the panel so it keeps its height when the content is short
                            ui.set_min_height(ui.available_height());
                            ui.horizontal(|ui| {
                                let n = d.all_diags().len();
                                ui.selectable_value(&mut bottom, Bottom::Problems, format!("Problems ({n})"));
                                ui.selectable_value(&mut bottom, Bottom::Refs, "References");
                                let dl = match &d.viewer {
                                    Some(v) => STORE.get(v.bulk).map(|b| format!("Data: {}", b.kw_disp)).unwrap_or("Data".into()),
                                    None => "Data viewer".into(),
                                };
                                ui.selectable_value(&mut bottom, Bottom::Data, dl);
                            });
                            ui.separator();
                            match bottom {
                                Bottom::Problems => problems_ui(ui, d, &mut acts),
                                Bottom::Refs => refs_ui(ui, d, &mut acts),
                                Bottom::Data => viewer_ui(ui, d, fs, &mut acts),
                            }
                        });
                }
                if show_outline {
                    egui::SidePanel::left("outline")
                        .resizable(true)
                        .default_width(300.0)
                        .min_width(160.0)
                        .frame(egui::Frame::new().fill(crate::theme::rgb(246, 247, 249)).inner_margin(egui::Margin::symmetric(8, 8)))
                        .show(ctx, |ui| {
                            ui.set_min_height(ui.available_height());
                            outline_ui(ui, d, &mut acts);
                        });
                }
                egui::CentralPanel::default().frame(egui::Frame::new().fill(crate::theme::t(Color32::WHITE))).show(ctx, |ui| {
                    editor_ui(ui, d, fs, &mut acts);
                });
            }
            Some(Tab::Diff(dt)) => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(crate::theme::t(Color32::WHITE)).inner_margin(12.0)).show(ctx, |ui| {
                    diff_ui(ui, dt, &mut acts);
                });
            }
            None => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(crate::theme::t(Color32::WHITE))).show(ctx, |ui| {
                    self.welcome(ui);
                });
            }
        }
        self.bottom = bottom;
        self.dialogs(ctx);
        self.handle(acts);
        self.file_op_ui(ctx);
        self.toasts_ui(ctx);
        #[cfg(feature = "shots")]
        crate::shots::step(self, ctx);
    }
}

#[cfg(test)]
impl App {
    pub fn save_to_for_test(&mut self, ti: usize, p: PathBuf) {
        self.save_to(ti, p, true);
    }
    /// The items the batch dialog would offer (part meshes or data blocks), with options.
    pub fn batch_items_for_test(&mut self, meshes: bool, mode: MeshSplit, min_lines: Option<u64>) -> Vec<BatchItem> {
        if meshes {
            self.split_meshes(None);
        } else {
            self.split_all_bulks();
        }
        let Some(mut b) = self.dlg.batch.take() else { return Vec::new() };
        match &mut b.kind {
            BatchKind::Meshes { mode: m, .. } => *m = mode,
            BatchKind::Blocks { min_lines: m, .. } => *m = min_lines,
        }
        b.rebuild();
        b.items
    }
    pub fn for_test() -> App {
        App {
            tabs: Vec::new(),
            active: 0,
            settings: Settings::default(),
            loads: Vec::new(),
            diff_jobs: Vec::new(),
            toasts: Vec::new(),
            bottom: Bottom::Problems,
            dlg: Dialogs::default(),
            exit_ok: false,
            title: String::new(),
            pending_acts: Vec::new(),
            file_op: None,
            pending_split: None,
            link_icons: None,
            file_dlg: None,
            #[cfg(feature = "shots")]
            shots: None,
        }
    }
    /// Wait for the current background file operation.
    pub fn wait_file_op(&mut self) {
        let t0 = std::time::Instant::now();
        while self.poll_file_op() {
            assert!(t0.elapsed().as_secs() < 600, "file operation did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
