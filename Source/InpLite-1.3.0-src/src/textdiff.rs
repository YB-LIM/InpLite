//! Side-by-side text compare (like vimdiff or the Notepad++ Compare plugin): two files line by
//! line, changed / added / removed lines coloured, the changed characters inside a line
//! highlighted, identical stretches can be folded, F7 / Shift+F7 jump between differences.

use crate::actions::Action;
use crate::lineview::{self, LineSource, LineStyle, LineViewState};
use crate::store::{placeholder_id, STORE};
use crate::util::{self, Fnv};
use egui::{Color32, Key, Modifiers, RichText};
use similar::{capture_diff_slices_deadline, Algorithm, DiffOp};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

/// Data blocks are expanded into the compared text up to this total size; larger ones are
/// compared as a whole (one line "… data: n lines").
const EXPAND_LIMIT: u64 = 1 << 30;

/// The lines of one side.
pub struct Lines {
    pub name: String,
    pub path: Option<PathBuf>,
    bytes: Vec<u8>,
    starts: Vec<u64>,
    /// (line index, lines in the file it stands for minus one): data blocks that were not expanded
    jumps: Vec<(u64, u64)>,
    pub max_cols: usize,
}

impl Lines {
    fn new(name: String, path: Option<PathBuf>) -> Lines {
        Lines { name, path, bytes: Vec::new(), starts: Vec::new(), jumps: Vec::new(), max_cols: 0 }
    }
    pub fn len(&self) -> u64 {
        self.starts.len() as u64
    }
    pub fn get(&self, i: u64) -> &[u8] {
        let s = self.starts[i as usize] as usize;
        let e = self.starts.get(i as usize + 1).map(|x| *x as usize).unwrap_or(self.bytes.len());
        &self.bytes[s..e]
    }
    pub fn text(&self, i: u64) -> String {
        lineview::expand_tabs(&String::from_utf8_lossy(self.get(i)))
    }
    /// 1-based line number in the file
    pub fn number(&self, i: u64) -> u64 {
        let k = self.jumps.partition_point(|j| j.0 < i);
        i + 1 + self.jumps[..k].iter().map(|j| j.1).sum::<u64>()
    }
    fn push(&mut self, l: &[u8]) {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        self.starts.push(self.bytes.len() as u64);
        self.bytes.extend_from_slice(l);
        self.max_cols = self.max_cols.max(l.len());
    }
    fn push_all(&mut self, b: &[u8]) {
        if b.is_empty() {
            return;
        }
        let b = b.strip_suffix(b"\n").unwrap_or(b);
        for l in b.split(|c| *c == b'\n') {
            self.push(l);
        }
    }
    /// A file as it is on disk.
    pub fn from_file(path: &Path) -> std::io::Result<Lines> {
        let bytes = std::fs::read(path)?;
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut l = Lines::new(name, Some(path.to_path_buf()));
        l.bytes.reserve(bytes.len());
        l.push_all(&bytes);
        Ok(l)
    }
    /// The (possibly unsaved) text of an open input file, data blocks on disk included.
    pub fn from_doc(name: String, path: Option<PathBuf>, text: &str) -> Lines {
        let mut l = Lines::new(name, path);
        let mut budget = EXPAND_LIMIT;
        for (_, s, e) in util::lines(text) {
            let line = &text[s..e];
            if let Some((id, _)) = placeholder_id(line) {
                if let Some(b) = STORE.get(id) {
                    if b.size() <= budget {
                        budget -= b.size();
                        l.push_all(b.bytes());
                        continue;
                    }
                    l.jumps.push((l.len(), b.lines.saturating_sub(1)));
                    let mut h = Fnv::default();
                    h.bytes(b.bytes());
                    l.push(format!("**[{} data: {} lines, not expanded · #{:016x}]", b.kw_disp, util::fmt_int(b.lines), h.0).as_bytes());
                    continue;
                }
            }
            l.push(line.as_bytes());
        }
        l
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Opts {
    pub ignore_blanks: bool,
    pub ignore_case: bool,
}

fn hashes(l: &Lines, o: Opts) -> Vec<u64> {
    (0..l.len())
        .map(|i| {
            let mut h = Fnv::default();
            for &c in l.get(i) {
                if o.ignore_blanks && (c == b' ' || c == b'\t') {
                    continue;
                }
                h.byte(if o.ignore_case { c.to_ascii_uppercase() } else { c });
            }
            h.0
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Same,
    Changed,
    Removed,
    Added,
    /// folded identical lines
    Fold,
}

/// A run of rows.
#[derive(Clone, Copy, Debug)]
pub struct Seg {
    pub kind: Kind,
    pub a0: u64,
    pub b0: u64,
    /// lines of A / B in the run (Same: equal; Changed: rows = max)
    pub na: u64,
    pub nb: u64,
}

impl Seg {
    fn rows(&self) -> u64 {
        match self.kind {
            Kind::Fold => 1,
            _ => self.na.max(self.nb),
        }
    }
}

pub struct DiffData {
    pub a: Lines,
    pub b: Lines,
    pub segs: Vec<Seg>,
    pub changed: u64,
    pub removed: u64,
    pub added: u64,
    pub hunks: usize,
    pub elapsed_ms: u64,
    pub approximate: bool,
}

pub fn compute(a: Lines, b: Lines, o: Opts) -> DiffData {
    let t0 = std::time::Instant::now();
    let (ha, hb) = (hashes(&a, o), hashes(&b, o));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &ha, &hb, Some(deadline));
    let approximate = std::time::Instant::now() >= deadline;
    let mut segs: Vec<Seg> = Vec::new();
    let (mut changed, mut removed, mut added, mut hunks) = (0, 0, 0, 0);
    let mut prev_diff = false;
    for op in ops {
        let s = match op {
            DiffOp::Equal { old_index, new_index, len } => Seg { kind: Kind::Same, a0: old_index as u64, b0: new_index as u64, na: len as u64, nb: len as u64 },
            DiffOp::Delete { old_index, old_len, new_index } => Seg { kind: Kind::Removed, a0: old_index as u64, b0: new_index as u64, na: old_len as u64, nb: 0 },
            DiffOp::Insert { old_index, new_index, new_len } => Seg { kind: Kind::Added, a0: old_index as u64, b0: new_index as u64, na: 0, nb: new_len as u64 },
            DiffOp::Replace { old_index, old_len, new_index, new_len } => Seg { kind: Kind::Changed, a0: old_index as u64, b0: new_index as u64, na: old_len as u64, nb: new_len as u64 },
        };
        match s.kind {
            Kind::Same => prev_diff = false,
            _ => {
                if !prev_diff {
                    hunks += 1;
                }
                prev_diff = true;
                match s.kind {
                    Kind::Changed => {
                        changed += s.na.min(s.nb);
                        removed += s.na.saturating_sub(s.nb);
                        added += s.nb.saturating_sub(s.na);
                    }
                    Kind::Removed => removed += s.na,
                    _ => added += s.nb,
                }
            }
        }
        // merge a delete + insert pair into a change
        if let (Some(last), Kind::Added) = (segs.last_mut(), s.kind) {
            if last.kind == Kind::Removed && last.a0 + last.na == s.a0 {
                last.kind = Kind::Changed;
                last.nb = s.nb;
                let m = last.na.min(last.nb);
                changed += m;
                removed -= m;
                added -= m;
                continue;
            }
        }
        segs.push(s);
    }
    DiffData { a, b, segs, changed, removed, added, hunks, elapsed_ms: t0.elapsed().as_millis() as u64, approximate }
}

/// The rows shown: the segments, with long identical runs folded (keeping `ctx` lines around
/// the differences) when `only_diff` is on.
pub struct RowMap {
    pub segs: Vec<Seg>,
    /// first row of each segment
    pub first: Vec<u64>,
    pub rows: u64,
    /// first row of every difference
    pub hunks: Vec<u64>,
    pub marks: Vec<(u64, Color32)>,
}

impl RowMap {
    pub fn build(d: &DiffData, only_diff: bool, ctx: u64) -> RowMap {
        let mut segs = Vec::with_capacity(d.segs.len() * 2);
        let n = d.segs.len();
        for (i, s) in d.segs.iter().enumerate() {
            if s.kind == Kind::Same && only_diff {
                let head = if i > 0 { ctx } else { 0 };
                let tail = if i + 1 < n { ctx } else { 0 };
                if s.na > head + tail + 1 {
                    if head > 0 {
                        segs.push(Seg { na: head, nb: head, ..*s });
                    }
                    let folded = s.na - head - tail;
                    segs.push(Seg { kind: Kind::Fold, a0: s.a0 + head, b0: s.b0 + head, na: folded, nb: folded });
                    if tail > 0 {
                        segs.push(Seg { a0: s.a0 + s.na - tail, b0: s.b0 + s.nb - tail, na: tail, nb: tail, ..*s });
                    }
                    continue;
                }
            }
            segs.push(*s);
        }
        let mut first = Vec::with_capacity(segs.len());
        let mut r = 0;
        let mut hunks = Vec::new();
        let mut marks = Vec::new();
        let mut prev_diff = false;
        for s in &segs {
            first.push(r);
            let diff = !matches!(s.kind, Kind::Same | Kind::Fold);
            if diff && !prev_diff {
                hunks.push(r);
            }
            if diff && marks.len() < 20_000 {
                marks.push((r, kind_color(s.kind)));
            }
            prev_diff = diff;
            r += s.rows();
        }
        RowMap { segs, first, rows: r, hunks, marks }
    }
    /// (segment, offset in it)
    fn at(&self, row: u64) -> (&Seg, u64) {
        let k = self.first.partition_point(|&f| f <= row).saturating_sub(1);
        (&self.segs[k], row - self.first[k])
    }
    /// Lines of A and B in a row.
    pub fn sides(&self, row: u64) -> (Kind, Option<u64>, Option<u64>) {
        let (s, o) = self.at(row);
        match s.kind {
            Kind::Fold => (Kind::Fold, Some(s.a0), Some(s.b0)),
            Kind::Same => (Kind::Same, Some(s.a0 + o), Some(s.b0 + o)),
            _ => {
                let a = (o < s.na).then_some(s.a0 + o);
                let b = (o < s.nb).then_some(s.b0 + o);
                let k = match (a, b) {
                    (Some(_), Some(_)) => Kind::Changed,
                    (Some(_), None) => Kind::Removed,
                    _ => Kind::Added,
                };
                (k, a, b)
            }
        }
    }
    fn fold_len(&self, row: u64) -> u64 {
        self.at(row).0.na
    }
}

fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Changed => crate::theme::rgb(230, 160, 20),
        Kind::Removed => crate::theme::rgb(220, 60, 60),
        Kind::Added => crate::theme::rgb(40, 160, 70),
        _ => crate::theme::rgb(180, 180, 180),
    }
}

/// Changed character ranges of two lines (prefix/suffix based, plus a character diff for short lines).
fn char_spans(a: &str, b: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    if ac.len() + bc.len() > 4000 {
        let p = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
        let s = ac[p..].iter().rev().zip(bc[p..].iter().rev()).take_while(|(x, y)| x == y).count();
        return (vec![(p, ac.len() - s)], vec![(p, bc.len() - s)]);
    }
    let ops = similar::capture_diff_slices(Algorithm::Myers, &ac, &bc);
    let (mut ra, mut rb) = (Vec::new(), Vec::new());
    for op in ops {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => ra.push((old_index, old_index + old_len)),
            DiffOp::Insert { new_index, new_len, .. } => rb.push((new_index, new_index + new_len)),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                ra.push((old_index, old_index + old_len));
                rb.push((new_index, new_index + new_len));
            }
        }
    }
    (ra, rb)
}

/// One pane (side A or B) of the compare view.
struct Pane<'a> {
    d: &'a DiffData,
    m: &'a RowMap,
    left: bool,
}

impl LineSource for Pane<'_> {
    fn count(&self) -> u64 {
        self.m.rows
    }
    fn line(&self, i: u64) -> (String, LineStyle) {
        let (k, a, b) = self.m.sides(i);
        let (mine, lines) = if self.left { (a, &self.d.a) } else { (b, &self.d.b) };
        let mut st = LineStyle::default();
        match k {
            Kind::Fold => {
                st.bg = Some(crate::theme::rgb(240, 242, 246));
                st.fg = Some(crate::theme::rgb(130, 136, 150));
                return (format!("  ⋯ {} identical lines ⋯", util::fmt_int(self.m.fold_len(i))), st);
            }
            Kind::Same => {}
            Kind::Changed => {
                st.bg = Some(crate::theme::rgb(255, 246, 214));
                let (ta, tb) = (self.d.a.text(a.unwrap()), self.d.b.text(b.unwrap()));
                let (sa, sb) = char_spans(&ta, &tb);
                let c = crate::theme::rgb(255, 214, 120);
                st.spans = if self.left { sa } else { sb }.into_iter().map(|(x, y)| (x, y, c)).collect();
            }
            Kind::Removed => {
                st.bg = Some(if self.left { crate::theme::rgb(253, 226, 226) } else { crate::theme::rgb(238, 238, 240) });
            }
            Kind::Added => {
                st.bg = Some(if self.left { crate::theme::rgb(238, 238, 240) } else { crate::theme::rgb(222, 246, 228) });
            }
        }
        match mine {
            Some(l) => (lines.text(l), st),
            None => (String::new(), st),
        }
    }
    fn number(&self, i: u64) -> Option<u64> {
        let (k, a, b) = self.m.sides(i);
        if k == Kind::Fold {
            return None;
        }
        let (mine, lines) = if self.left { (a, &self.d.a) } else { (b, &self.d.b) };
        mine.map(|l| lines.number(l))
    }
    fn max_cols(&self) -> usize {
        self.d.a.max_cols.max(self.d.b.max_cols)
    }
    fn max_number(&self) -> u64 {
        let l = |x: &Lines| if x.len() == 0 { 0 } else { x.number(x.len() - 1) };
        l(&self.d.a).max(l(&self.d.b))
    }
    fn marks(&self) -> &[(u64, Color32)] {
        &self.m.marks
    }
}

/// Where the text of a side comes from (to compare again after edits).
#[derive(Clone, Debug)]
pub enum Side {
    /// an open input file (its current text)
    Doc { edit_id: egui::Id, name: String, path: Option<PathBuf> },
    /// a file on disk
    File(PathBuf),
}

pub struct CompareTab {
    pub a: Side,
    pub b: Side,
    pub opts: Opts,
    pub only_diff: bool,
    pub data: Option<Arc<DiffData>>,
    pub map: Option<RowMap>,
    pub view: LineViewState,
    pub pending: Option<Receiver<Result<DiffData, String>>>,
    pub err: String,
    /// ask the app to compute again (options changed, refresh)
    pub recompute: bool,
    pub id: egui::Id,
}

impl CompareTab {
    pub fn new(a: Side, b: Side) -> CompareTab {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        CompareTab { a, b, opts: Opts::default(), only_diff: false, data: None, map: None, view: LineViewState::default(), pending: None, err: String::new(), recompute: true, id: egui::Id::new(("inplite-compare", n)) }
    }
    pub fn title(&self) -> String {
        let n = |s: &Side| match s {
            Side::Doc { name, .. } => name.clone(),
            Side::File(p) => p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        format!("{} ↔ {}", n(&self.a), n(&self.b))
    }
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else { return false };
        match rx.try_recv() {
            Ok(Ok(d)) => {
                let keep = self.view.top;
                self.map = Some(RowMap::build(&d, self.only_diff, 3));
                self.data = Some(Arc::new(d));
                self.pending = None;
                self.err.clear();
                self.view.top = keep;
                if self.view.sel.is_none() {
                    if let Some(&h) = self.map.as_ref().and_then(|m| m.hunks.first()) {
                        self.view.go(h);
                    }
                }
                false
            }
            Ok(Err(e)) => {
                self.err = e;
                self.pending = None;
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => {
                self.pending = None;
                false
            }
        }
    }
    fn go_hunk(&mut self, dir: i32) {
        let Some(m) = &self.map else { return };
        let cur = self.view.sel.map(|s| s.1).unwrap_or(self.view.top);
        let t = if dir > 0 { m.hunks.iter().find(|&&h| h > cur).or(m.hunks.first()) } else { m.hunks.iter().rev().find(|&&h| h < cur).or(m.hunks.last()) };
        if let Some(&h) = t {
            self.view.go(h);
            self.view.reveal = Some(h);
        }
    }
}

pub fn compare_ui(ui: &mut egui::Ui, t: &mut CompareTab, font_size: f32, acts: &mut Vec<Action>) {
    // F7 / Shift+F7, Alt+Down / Alt+Up: next / previous difference
    let (next, prev) = ui.input_mut(|i| {
        let prev = i.consume_key(Modifiers::SHIFT, Key::F7) || i.consume_key(Modifiers::ALT, Key::ArrowUp);
        (i.consume_key(Modifiers::NONE, Key::F7) || i.consume_key(Modifiers::ALT, Key::ArrowDown), prev)
    });
    if next {
        t.go_hunk(1);
    }
    if prev {
        t.go_hunk(-1);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Compare").strong());
        if let Some(d) = &t.data {
            let txt = if d.hunks == 0 {
                RichText::new("  the files are identical  ").color(crate::theme::rgb(20, 120, 40)).background_color(crate::theme::rgb(222, 246, 228))
            } else {
                RichText::new(format!("  {} difference{}  ", util::fmt_int(d.hunks as u64), if d.hunks == 1 { "" } else { "s" })).strong()
            };
            ui.label(txt);
            for (n, what, fg, bg) in [
                (d.changed, "changed", crate::theme::rgb(150, 100, 0), crate::theme::rgb(255, 242, 210)),
                (d.removed, "only in A", crate::theme::rgb(170, 30, 30), crate::theme::rgb(253, 228, 228)),
                (d.added, "only in B", crate::theme::rgb(20, 120, 50), crate::theme::rgb(222, 246, 228)),
            ] {
                if n > 0 {
                    ui.label(RichText::new(format!(" {} {what} ", util::fmt_int(n))).color(fg).background_color(bg));
                }
            }
            if d.approximate {
                ui.label(RichText::new("(very many differences: approximate)").small().color(crate::theme::t(crate::syntax::Palette::WARN)));
            }
        }
        if t.pending.is_some() {
            ui.spinner();
            ui.label(RichText::new("comparing…").weak());
        }
        ui.separator();
        if ui.button("◀ Prev").on_hover_text("Previous difference (Shift+F7, Alt+Up)").clicked() {
            t.go_hunk(-1);
        }
        if ui.button("Next ▶").on_hover_text("Next difference (F7, Alt+Down)").clicked() {
            t.go_hunk(1);
        }
        if ui.checkbox(&mut t.only_diff, "Only differences").on_hover_text("Fold identical lines (3 lines of context are kept)").changed() {
            if let Some(d) = &t.data {
                t.map = Some(RowMap::build(d, t.only_diff, 3));
                t.view.top = 0;
                t.view.sel = None;
            }
        }
        let mut o = t.opts;
        ui.checkbox(&mut o.ignore_blanks, "Ignore blanks");
        ui.checkbox(&mut o.ignore_case, "Ignore case");
        if o != t.opts {
            t.opts = o;
            t.recompute = true;
        }
        if ui.button("Refresh").on_hover_text("Compare the current texts again").clicked() {
            t.recompute = true;
        }
    });
    if !t.err.is_empty() {
        ui.label(RichText::new(&t.err).color(crate::theme::t(crate::syntax::Palette::ERR)));
    }
    let (Some(d), Some(m)) = (t.data.clone(), t.map.as_ref()) else {
        return;
    };
    // headers
    let full = ui.available_rect_before_wrap();
    let half = (full.width() - 6.0) / 2.0;
    let side_name = |l: &Lines, which: &str| format!("{which}: {}{}", l.name, l.path.as_ref().map(|p| format!("   ({})", p.display())).unwrap_or_default());
    ui.horizontal(|ui| {
        ui.allocate_ui(egui::vec2(half, 18.0), |ui| {
            ui.add(egui::Label::new(RichText::new(side_name(&d.a, "A")).monospace().small()).truncate());
        });
        ui.allocate_ui(egui::vec2(half, 18.0), |ui| {
            ui.add(egui::Label::new(RichText::new(side_name(&d.b, "B")).monospace().small()).truncate());
        });
    });
    let body = ui.available_rect_before_wrap();
    let lr = egui::Rect::from_min_size(body.min, egui::vec2(half, body.height()));
    let rr = egui::Rect::from_min_max(egui::pos2(body.left() + half + 6.0, body.top()), body.max);
    let mut dbl: Option<(bool, u64)> = None;
    for (left, r) in [(true, lr), (false, rr)] {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
        let pane = Pane { d: &d, m, left };
        let out = lineview::show(&mut child, t.id.with(left), &mut t.view, &pane, font_size);
        if let Some(row) = out.double_clicked {
            dbl = Some((left, row));
        }
        out.response.context_menu(|ui| {
            let row = t.view.sel.map(|s| s.1).unwrap_or(t.view.top);
            let (_, a, b) = m.sides(row);
            for (lbl, side, line, lines) in [("A", &t.a, a, &d.a), ("B", &t.b, b, &d.b)] {
                let path = match side {
                    Side::Doc { path, .. } => path.clone(),
                    Side::File(p) => Some(p.clone()),
                };
                if let (Some(p), Some(l)) = (path, line) {
                    if ui.button(format!("Open {lbl} at line {}", util::fmt_int(lines.number(l)))).clicked() {
                        acts.push(Action::OpenFile(p, Some(lines.number(l))));
                        ui.close();
                    }
                }
            }
        });
    }
    ui.allocate_rect(body, egui::Sense::hover());
    if let Some((left, row)) = dbl {
        let (_, a, b) = m.sides(row);
        let (side, line, lines) = if left { (&t.a, a, &d.a) } else { (&t.b, b, &d.b) };
        let path = match side {
            Side::Doc { path, .. } => path.clone(),
            Side::File(p) => Some(p.clone()),
        };
        if let (Some(p), Some(l)) = (path, line) {
            acts.push(Action::OpenFile(p, Some(lines.number(l))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_rows() {
        let mk = |s: &str| {
            let mut l = Lines::new("x".into(), None);
            l.push_all(s.as_bytes());
            l
        };
        let a = mk("*Heading\n*Node\n1, 0., 0.\n2, 1., 0.\n3, 2., 0.\n*Element\n");
        let b = mk("*Heading\n*Node\n1, 0., 0.\n2, 1.5, 0.\n3, 2., 0.\n4, 3., 0.\n*Element\n");
        let d = compute(a, b, Opts::default());
        assert_eq!(d.hunks, 2);
        assert_eq!((d.changed, d.removed, d.added), (1, 0, 1));
        let m = RowMap::build(&d, false, 3);
        assert_eq!(m.rows, 7);
        assert_eq!(m.sides(3), (Kind::Changed, Some(3), Some(3)));
        assert_eq!(m.sides(5), (Kind::Added, None, Some(5)));
        let (sa, sb) = char_spans("2, 1., 0.", "2, 1.5, 0.");
        assert!(sa.is_empty() || sa.iter().all(|r| r.0 == r.1) || sa.len() <= 1);
        assert_eq!(sb, vec![(5, 6)]);
        let m2 = RowMap::build(&d, true, 1);
        assert!(m2.segs.iter().any(|s| s.kind == Kind::Fold));
        assert_eq!(m2.hunks.len(), 2);
        let o = Opts { ignore_blanks: true, ignore_case: true };
        let d2 = compute(mk("*NODE\n1,0\n"), mk("*Node\n1, 0\n"), o);
        assert_eq!(d2.hunks, 0);
    }
}
