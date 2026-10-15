//! Per-document UI state and its background jobs.

use crate::analysis::{self, Analysis};
use crate::doc::Document;
use crate::load::Progress;
use crate::mesh::{self, MeshIndex};
use crate::store::{placeholder_id, STORE};
use crate::syntax::Diag;
use crate::util;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

pub struct LineMap {
    pub ver: u64,
    pub starts: Vec<usize>,
    /// 1-based real file line number of each skeleton line
    pub real: Vec<u64>,
    pub bulk_line: HashMap<u64, usize>,
    pub total: u64,
    /// longest line (bytes), for the horizontal scroll range
    pub max_cols: usize,
}

impl LineMap {
    pub fn build(text: &str, ver: u64) -> LineMap {
        let starts = util::line_starts(text);
        let mut real = Vec::with_capacity(starts.len());
        let mut bulk_line = HashMap::new();
        let mut r: u64 = 1;
        let mut max_cols = 0;
        for (i, &s) in starts.iter().enumerate() {
            real.push(r);
            let e = starts.get(i + 1).map(|x| x - 1).unwrap_or(text.len());
            let line = &text[s..e.max(s)];
            max_cols = max_cols.max(line.len());
            if line.starts_with(crate::store::PH_PREFIX) {
                if let Some((id, _)) = placeholder_id(line) {
                    bulk_line.insert(id, i);
                    r += STORE.get(id).map(|b| b.lines).unwrap_or(1);
                    continue;
                }
            }
            r += 1;
        }
        let total = r.saturating_sub(1);
        LineMap { ver, starts, real, bulk_line, total, max_cols }
    }
    pub fn line_of_byte(&self, b: usize) -> usize {
        util::line_of(&self.starts, b)
    }
    /// skeleton line for a real (1-based) line number, plus bulk row if it falls inside a bulk block
    pub fn from_real(&self, real: u64) -> (usize, Option<u64>) {
        let i = match self.real.binary_search(&real) {
            Ok(i) => return (i, None),
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let off = real - self.real[i];
        (i, if off > 0 { Some(off) } else { None })
    }
}

#[derive(Clone, Copy)]
pub struct Jump {
    pub line: usize,
    pub sel: Option<(usize, usize)>,
    pub focus: bool,
}

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub case: bool,
    pub count: usize,
    pub counted_for: (u64, String, bool),
    pub focus: bool,
}

pub struct Completion {
    pub items: Vec<CItem>,
    pub sel: usize,
    pub replace: (usize, usize),
    pub ver: u64,
    pub manual: bool,
}

#[derive(Clone)]
pub struct CItem {
    pub label: String,
    pub insert: String,
    pub detail: String,
    /// cursor offset after insert (from start of insert)
    pub cursor: Option<usize>,
}

pub struct RefItem {
    pub label: String,
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub file: Option<(std::path::PathBuf, u64)>,
    pub is_def: bool,
}

pub struct BulkViewer {
    pub bulk: u64,
    pub top: u64,
    pub sel: Option<u64>,
    pub goto: String,
    pub widths: Vec<usize>,
    pub msg: String,
    /// background label search: (label, result)
    pub search: Option<(u64, std::sync::mpsc::Receiver<Option<u64>>)>,
}

impl BulkViewer {
    pub fn new(bulk: u64, row: Option<u64>) -> BulkViewer {
        BulkViewer { bulk, top: row.map(|r| r.saturating_sub(5)).unwrap_or(0), sel: row, goto: String::new(), widths: Vec::new(), msg: String::new(), search: None }
    }
}

pub struct DocTab {
    pub doc: Document,
    pub edit_id: egui::Id,
    /// highlighted visible window of the editor
    pub view: crate::codeview::ViewCache,
    /// editor geometry of the last frame
    pub geom: crate::codeview::Geom,
    /// selection (anchor, head) as byte offsets; head is the caret
    pub sel: (usize, usize),
    /// column kept while moving up/down
    pub pref_col: Option<usize>,
    /// scroll the editor to this byte offset (centered?)
    pub scroll_req: Option<(usize, bool)>,
    /// message for the user from the editor (shown as a toast)
    pub notice: Option<String>,
    /// middle-button autoscroll in progress
    pub autoscroll: Option<crate::codeview::AutoScroll>,
    diag_cache: std::cell::RefCell<(usize, usize, Arc<Vec<Diag>>)>,
    pub map: LineMap,
    pub analysis: Option<Arc<Analysis>>,
    an_job: Option<(u64, Receiver<Analysis>)>,
    pub last_change: f64,
    last_ver_seen: u64,
    pub mesh: Option<Arc<MeshIndex>>,
    pub mesh_job: Option<(Receiver<MeshIndex>, Arc<Progress>)>,
    pub cursor: (usize, usize, usize),
    pub jump: Option<Jump>,
    pub viewer: Option<BulkViewer>,
    pub completion: Option<Completion>,
    pub refs: Vec<RefItem>,
    pub refs_title: String,
    pub outline_open: HashMap<String, bool>,
    pub outline_rev: u64,
    pub outline_cache: crate::outline::OutlineCache,
    pub outline_filter: String,
    pub find: FindState,
    pub op_undo: Option<(String, String)>,
    pub problems_filter: (bool, bool),
    /// show the hover tooltip for this byte offset (used for documentation screenshots)
    pub force_hover: Option<usize>,
}

impl DocTab {
    pub fn new(doc: Document) -> DocTab {
        let ver = doc.text.ver;
        let map = LineMap::build(&doc.text.s, ver);
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut t = DocTab {
            doc,
            edit_id: egui::Id::new(("inplite-editor", n)),
            view: Default::default(),
            geom: Default::default(),
            sel: (0, 0),
            pref_col: None,
            scroll_req: None,
            notice: None,
            autoscroll: None,
            diag_cache: Default::default(),
            map,
            analysis: None,
            an_job: None,
            last_change: -10.0,
            last_ver_seen: ver,
            mesh: None,
            mesh_job: None,
            cursor: (0, 0, 0),
            jump: None,
            viewer: None,
            completion: None,
            refs: Vec::new(),
            refs_title: String::new(),
            outline_open: HashMap::new(),
            outline_rev: 0,
            outline_cache: Default::default(),
            outline_filter: String::new(),
            find: FindState::default(),
            op_undo: None,
            problems_filter: (true, true),
            force_hover: None,
        };
        t.start_analysis();
        t.start_mesh();
        t
    }

    pub fn ensure_map(&mut self) {
        if self.map.ver != self.doc.text.ver {
            self.map = LineMap::build(&self.doc.text.s, self.doc.text.ver);
        }
    }

    pub fn start_analysis(&mut self) {
        let text = self.doc.text.s.clone();
        let ver = self.doc.text.ver;
        let dir = self.doc.dir();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut a = analysis::analyze(&text, ver, &dir);
            a.live = crate::syntax::check(&text);
            let _ = tx.send(a);
        });
        self.an_job = Some((ver, rx));
    }

    pub fn start_mesh(&mut self) {
        let text = self.doc.text.s.clone();
        let ver = self.doc.text.ver;
        let prog = Progress::new();
        let p2 = prog.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let m = mesh::build(&text, ver, &p2);
            let _ = tx.send(m);
        });
        self.mesh_job = Some((rx, prog));
    }

    /// Poll background jobs; schedule re-analysis after edits (debounced). Returns true if busy.
    pub fn poll(&mut self, now: f64) -> bool {
        let mut busy = false;
        if self.doc.text.ver != self.last_ver_seen {
            self.last_ver_seen = self.doc.text.ver;
            self.last_change = now;
        }
        if let Some((_, rx)) = &self.an_job {
            match rx.try_recv() {
                Ok(a) => {
                    // freeing a big analysis takes tens of ms: do it off the UI thread
                    if let Some(old) = self.analysis.replace(Arc::new(a)) {
                        std::thread::spawn(move || drop(old));
                    }
                    self.an_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => busy = true,
                Err(_) => self.an_job = None,
            }
        }
        let have = self.analysis.as_ref().map(|a| a.ver);
        if self.an_job.is_none() && have != Some(self.doc.text.ver) {
            if now - self.last_change > 0.35 {
                self.start_analysis();
            }
            busy = true;
        }
        if let Some((rx, _)) = &self.mesh_job {
            match rx.try_recv() {
                Ok(m) => {
                    if let Some(old) = self.mesh.replace(Arc::new(m)) {
                        std::thread::spawn(move || drop(old));
                    }
                    self.mesh_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => busy = true,
                Err(_) => self.mesh_job = None,
            }
        }
        busy
    }

    pub fn current_analysis(&self) -> Option<&Analysis> {
        self.analysis.as_deref().filter(|a| a.ver == self.doc.text.ver)
    }

    /// All diagnostics: instant syntax checks + analysis + mesh, de-duplicated and sorted by
    /// line. Cached until a new analysis or mesh check arrives.
    pub fn all_diags(&self) -> Arc<Vec<Diag>> {
        let ka = self.analysis.as_ref().map(|a| Arc::as_ptr(a) as usize).unwrap_or(0);
        let km = self.mesh.as_ref().map(|m| Arc::as_ptr(m) as usize).unwrap_or(0);
        {
            let c = self.diag_cache.borrow();
            if c.0 == ka && c.1 == km && (ka != 0 || km != 0) {
                return c.2.clone();
            }
        }
        let mut v: Vec<Diag> = Vec::new();
        if let Some(a) = &self.analysis {
            v.extend(a.live.iter().cloned());
            v.extend(a.diags.iter().cloned());
        }
        if let Some(m) = &self.mesh {
            v.extend(m.diags.iter().cloned());
        }
        let mut seen = HashSet::new();
        v.retain(|d| seen.insert((d.line, d.msg.clone(), d.bulk)));
        v.sort_by_key(|d| (d.line, d.bulk.map(|b| b.1).unwrap_or(0), d.sev));
        let v = Arc::new(v);
        *self.diag_cache.borrow_mut() = (ka, km, v.clone());
        v
    }

    pub fn jump_to_line(&mut self, line: usize, sel: Option<(usize, usize)>) {
        self.jump = Some(Jump { line, sel, focus: true });
    }

    pub fn jump_to_real(&mut self, real: u64) {
        self.ensure_map();
        let (line, row) = self.map.from_real(real);
        self.jump_to_line(line, None);
        if let Some(row) = row {
            let s = self.map.starts[line];
            let e = self.map.starts.get(line + 1).map(|x| x - 1).unwrap_or(self.doc.text.s.len());
            if let Some((id, _)) = placeholder_id(&self.doc.text.s[s..e]) {
                self.viewer = Some(BulkViewer::new(id, Some(row)));
            }
        }
    }

    pub fn line_text(&self, line: usize) -> &str {
        let s = self.map.starts.get(line).copied().unwrap_or(0);
        let e = self.map.starts.get(line + 1).map(|x| x - 1).unwrap_or(self.doc.text.s.len());
        &self.doc.text.s[s.min(self.doc.text.s.len())..e.max(s).min(self.doc.text.s.len())]
    }

    /// Replace a byte range of the text, remembering the previous text for "Undo last operation".
    pub fn edit(&mut self, desc: &str, r: std::ops::Range<usize>, with: &str) {
        self.op_undo = Some((desc.to_string(), self.doc.text.s.clone()));
        self.doc.text.replace_range(r, with);
        self.completion = None;
    }
}
