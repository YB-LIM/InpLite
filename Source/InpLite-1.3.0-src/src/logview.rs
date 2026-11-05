//! Viewer for Abaqus output files (.dat, .msg, .sta, .log): coloured errors / warnings / notes,
//! step and increment structure, a summary, and monitoring of a file that is still being written
//! (like `tail -f`, the "eye" of Notepad++).
//!
//! The file is read into memory (never memory-mapped or kept open: Abaqus must be able to append
//! to it, or delete and rewrite it for the next run).

use crate::lineview::{self, LineSource, LineStyle, LineViewState};
use crate::util;
use egui::{Color32, FontId, Pos2, Rect, RichText, Sense, Vec2};
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogKind {
    Dat,
    Msg,
    Sta,
    Log,
}

impl LogKind {
    pub fn of(p: &Path) -> Option<LogKind> {
        let e = p.extension()?.to_string_lossy().to_ascii_lowercase();
        Some(match e.as_str() {
            "dat" => LogKind::Dat,
            "msg" => LogKind::Msg,
            "sta" => LogKind::Sta,
            "log" => LogKind::Log,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            LogKind::Dat => "data file (.dat)",
            LogKind::Msg => "message file (.msg)",
            LogKind::Sta => "status file (.sta)",
            LogKind::Log => "log file (.log)",
        }
    }
}

// line classes
pub const C_PLAIN: u8 = 0;
pub const C_ERR: u8 = 1;
pub const C_ERR_CONT: u8 = 2;
pub const C_WARN: u8 = 3;
pub const C_WARN_CONT: u8 = 4;
pub const C_NOTE: u8 = 5;
pub const C_NOTE_CONT: u8 = 6;
pub const C_STEP: u8 = 7;
pub const C_INC: u8 = 8;
pub const C_CUT: u8 = 9;
pub const C_HEAD: u8 = 10;
pub const C_OK: u8 = 11;
pub const C_FAIL: u8 = 12;
pub const C_TITLE: u8 = 13;
pub const C_ROW: u8 = 14;
pub const C_TIME: u8 = 15;

#[derive(Clone, Debug)]
pub struct LogMsg {
    pub line: u64,
    /// C_ERR, C_WARN or C_NOTE
    pub sev: u8,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct IncInfo {
    pub line: u64,
    pub num: i64,
    /// attempts that failed (cutbacks)
    pub cut: u32,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct StepInfo {
    pub line: u64,
    pub num: i64,
    pub label: String,
    pub incs: Vec<IncInfo>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Status {
    #[default]
    Unknown,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub status: Status,
    pub status_line: Option<u64>,
    pub errors: usize,
    pub warnings: usize,
    pub notes: usize,
    pub cutbacks: usize,
    pub step: Option<i64>,
    pub inc: Option<i64>,
    pub step_time: Option<String>,
    pub total_time: Option<String>,
    pub wallclock: Option<String>,
    pub cpu: Option<String>,
    pub product: Option<String>,
    pub problem: Vec<(String, String)>,
}

#[derive(Default)]
struct Scan {
    /// class of continuation lines of the current message (0: none)
    cont: u8,
    msg: Option<usize>,
}

pub struct LogData {
    pub path: PathBuf,
    pub kind: LogKind,
    pub bytes: Vec<u8>,
    /// start of every line (the last one may be incomplete while the file is written)
    pub starts: Vec<u64>,
    /// class of every complete line
    pub cls: Vec<u8>,
    pub max_cols: usize,
    pub msgs: Vec<LogMsg>,
    pub steps: Vec<StepInfo>,
    pub summary: Summary,
    pub marks: Vec<(u64, Color32)>,
    /// bytes processed (end of the last complete line)
    done: usize,
    scan: Scan,
    /// file length and modification time when last read
    pub stamp: (u64, Option<SystemTime>),
    /// changes with every read
    pub ver: u64,
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    memchr::memmem::find(h, n)
}

fn tokens(l: &[u8]) -> Vec<&[u8]> {
    l.split(|c| *c == b' ' || *c == b'\t' || *c == b',').filter(|t| !t.is_empty()).collect()
}

fn int_of(t: &[u8]) -> Option<i64> {
    std::str::from_utf8(t).ok()?.parse().ok()
}

fn int_after(l: &[u8], key: &[u8]) -> Option<i64> {
    let p = find(l, key)? + key.len();
    tokens(&l[p..]).first().and_then(|t| int_of(t))
}

fn text_after(l: &[u8], key: &[u8]) -> Option<String> {
    let p = find(l, key)? + key.len();
    let t = tokens(&l[p..]);
    t.iter().find(|t| **t != b"=").map(|t| String::from_utf8_lossy(t).trim_start_matches('=').to_string())
}

/// "S T E P       1     S T A T I C   A N A L Y S I S": letters separated by single blanks.
fn is_spaced(l: &[u8]) -> bool {
    let t = tokens(l);
    if t.len() < 5 {
        return false;
    }
    let single = t.iter().filter(|x| x.len() == 1).count();
    let letters = t.iter().filter(|x| x.len() == 1 && x[0].is_ascii_alphabetic()).count();
    letters >= 4 && single * 10 >= t.len() * 8
}

/// "S T E P   1   S T A T I C" -> "STEP 1 STATIC"
fn despace(l: &str) -> String {
    l.trim().split("  ").filter(|g| !g.trim().is_empty()).map(|g| g.replace(' ', "")).collect::<Vec<_>>().join(" ")
}

impl LogData {
    pub fn read(path: &Path) -> io::Result<LogData> {
        let kind = LogKind::of(path).unwrap_or(LogKind::Log);
        let mut f = std::fs::File::open(path)?;
        let md = f.metadata()?;
        let mut bytes = Vec::with_capacity(md.len() as usize + 1024);
        f.read_to_end(&mut bytes)?;
        let mut d = LogData {
            path: path.to_path_buf(),
            kind,
            bytes: Vec::new(),
            starts: Vec::new(),
            cls: Vec::new(),
            max_cols: 0,
            msgs: Vec::new(),
            steps: Vec::new(),
            summary: Summary::default(),
            marks: Vec::new(),
            done: 0,
            scan: Scan::default(),
            stamp: (md.len(), md.modified().ok()),
            ver: 0,
        };
        d.append(bytes);
        d.stamp.0 = d.bytes.len() as u64;
        Ok(d)
    }

    pub fn lines(&self) -> u64 {
        self.starts.len() as u64
    }

    pub fn line_bytes(&self, i: u64) -> &[u8] {
        let s = self.starts[i as usize] as usize;
        let e = self.starts.get(i as usize + 1).map(|x| *x as usize - 1).unwrap_or(self.bytes.len());
        let mut e = e.max(s);
        if e > s && self.bytes[e - 1] == b'\n' {
            e -= 1;
        }
        if e > s && self.bytes[e - 1] == b'\r' {
            e -= 1;
        }
        &self.bytes[s..e]
    }

    /// Add bytes at the end of the file and classify the new complete lines.
    pub fn append(&mut self, more: Vec<u8>) {
        if self.bytes.is_empty() {
            self.bytes = more;
        } else {
            self.bytes.extend_from_slice(&more);
        }
        // the last (incomplete) line starts again
        if let Some(&last) = self.starts.last() {
            if last as usize >= self.done {
                self.starts.pop();
            }
        }
        let mut pos = self.done;
        let n = self.bytes.len();
        while pos < n {
            self.starts.push(pos as u64);
            match memchr::memchr(b'\n', &self.bytes[pos..]) {
                Some(i) => {
                    let li = self.starts.len() as u64 - 1;
                    let end = pos + i;
                    self.max_cols = self.max_cols.max(end - pos);
                    self.classify(li, pos, end);
                    pos = end + 1;
                    self.done = pos;
                }
                None => {
                    self.max_cols = self.max_cols.max(n - pos);
                    break;
                }
            }
        }
        self.ver += 1;
    }

    fn mark(&mut self, li: u64, c: Color32) {
        if self.marks.len() < 20_000 {
            self.marks.push((li, c));
        }
    }

    fn cur_step(&mut self, li: u64) -> &mut StepInfo {
        if self.steps.is_empty() {
            self.steps.push(StepInfo { line: li, num: 1, label: "Step 1".into(), incs: Vec::new() });
        }
        self.steps.last_mut().unwrap()
    }

    fn classify(&mut self, li: u64, s: usize, e: usize) {
        let mut e2 = e;
        if e2 > s && self.bytes[e2 - 1] == b'\r' {
            e2 -= 1;
        }
        let raw = self.bytes[s..e2].to_vec();
        let up = raw.to_ascii_uppercase();
        let l = up.as_slice();
        let t = l.trim_ascii();
        let c = self.class_of(li, &raw, l, t);
        self.cls.push(c);
    }

    fn class_of(&mut self, li: u64, raw: &[u8], l: &[u8], t: &[u8]) -> u8 {
        let kind = self.kind;
        if t.is_empty() {
            self.scan.cont = 0;
            self.scan.msg = None;
            return C_PLAIN;
        }
        // messages
        let head = if find(l, b"***ERROR").is_some() || (kind == LogKind::Log && (t.starts_with(b"ABAQUS ERROR") || find(l, b"ERROR:").is_some())) {
            Some((C_ERR, C_ERR_CONT))
        } else if find(l, b"***WARNING").is_some() || (kind == LogKind::Log && t.starts_with(b"ABAQUS WARNING")) {
            Some((C_WARN, C_WARN_CONT))
        } else if find(l, b"***NOTE").is_some() {
            Some((C_NOTE, C_NOTE_CONT))
        } else {
            None
        };
        if let Some((h, cont)) = head {
            let text = String::from_utf8_lossy(raw).trim().trim_start_matches("***ERROR:").trim_start_matches("***WARNING:").trim_start_matches("***NOTE:").trim().to_string();
            self.msgs.push(LogMsg { line: li, sev: h, text });
            self.scan.cont = cont;
            self.scan.msg = Some(self.msgs.len() - 1);
            match h {
                C_ERR => {
                    self.summary.errors += 1;
                    self.mark(li, crate::theme::t(crate::syntax::Palette::ERR));
                }
                C_WARN => {
                    self.summary.warnings += 1;
                    self.mark(li, crate::theme::t(crate::syntax::Palette::WARN));
                }
                _ => self.summary.notes += 1,
            }
            return h;
        }
        if self.scan.cont != 0 {
            let indented = raw.first().is_some_and(|c| *c == b' ' || *c == b'\t');
            if indented && find(l, b"***").is_none() && !is_spaced(l) {
                if let Some(m) = self.scan.msg.and_then(|i| self.msgs.get_mut(i)) {
                    if m.text.len() < 600 {
                        m.text.push(' ');
                        m.text.push_str(String::from_utf8_lossy(raw).trim());
                    }
                }
                return self.scan.cont;
            }
            self.scan.cont = 0;
            self.scan.msg = None;
        }
        // completion
        if find(l, b"HAS NOT BEEN COMPLETED").is_some() || find(l, b"NOT COMPLETED").is_some() || find(l, b"EXITED WITH ERROR").is_some() || find(l, b"EXITED WITH AN ERROR").is_some() {
            self.summary.status = Status::Failed;
            self.summary.status_line = Some(li);
            self.mark(li, crate::theme::t(crate::syntax::Palette::ERR));
            return C_FAIL;
        }
        if find(l, b"COMPLETED SUCCESSFULLY").is_some() || find(l, b"HAS BEEN COMPLETED").is_some() || t.starts_with(b"ANALYSIS COMPLETE") || (kind == LogKind::Log && find(l, b" COMPLETED").is_some() && t.starts_with(b"ABAQUS JOB")) {
            self.summary.status = Status::Completed;
            self.summary.status_line = Some(li);
            return C_OK;
        }
        // job time
        if find(l, b"WALLCLOCK TIME").is_some() {
            self.summary.wallclock = text_after(l, b"=").or_else(|| text_after(l, b"WALLCLOCK TIME (SEC)"));
            return C_TIME;
        }
        if find(l, b"TOTAL CPU TIME").is_some() {
            self.summary.cpu = text_after(l, b"=");
            return C_TIME;
        }
        // problem size (.dat)
        for (key, name) in [(&b"NUMBER OF ELEMENTS IS"[..], "Elements"), (b"NUMBER OF NODES IS", "Nodes"), (b"TOTAL NUMBER OF VARIABLES IN THE MODEL", "Variables")] {
            if find(l, key).is_some() && !self.summary.problem.iter().any(|p| p.0 == name) {
                if let Some(v) = tokens(l).last() {
                    self.summary.problem.push((name.into(), String::from_utf8_lossy(v).into_owned()));
                }
            }
        }
        // steps
        if is_spaced(l) {
            if find(l, b"S T E P").is_some() {
                let label = despace(&String::from_utf8_lossy(raw));
                let num = tokens(l).iter().find_map(|x| if x.len() >= 1 && x.iter().all(|c| c.is_ascii_digit()) { int_of(x) } else { None }).unwrap_or(self.steps.len() as i64 + 1);
                self.steps.push(StepInfo { line: li, num, label, incs: Vec::new() });
                self.summary.step = Some(num);
                return C_STEP;
            }
            return C_TITLE;
        }
        if kind == LogKind::Sta && t.starts_with(b"STEP") && find(l, b"ORIGIN").is_some() {
            let num = int_after(l, b"STEP").unwrap_or(self.steps.len() as i64 + 1);
            self.steps.push(StepInfo { line: li, num, label: format!("Step {num}"), incs: Vec::new() });
            self.summary.step = Some(num);
            return C_STEP;
        }
        // increments (.msg)
        if find(l, b"INCREMENT").is_some() && find(l, b"STARTS").is_some() {
            let inc = int_after(l, b"INCREMENT").unwrap_or(0);
            let att = int_after(l, b"ATTEMPT NUMBER").unwrap_or(1);
            self.summary.inc = Some(inc);
            let st = self.cur_step(li);
            if att <= 1 || st.incs.last().map(|x| x.num) != Some(inc) {
                st.incs.push(IncInfo { line: li, num: inc, cut: 0, detail: String::new() });
            }
            if att > 1 {
                if let Some(x) = st.incs.last_mut() {
                    x.cut += 1;
                }
                self.summary.cutbacks += 1;
                self.mark(li, crate::theme::rgb(230, 140, 0));
                return C_CUT;
            }
            return C_INC;
        }
        if t.starts_with(b"INCREMENT") && t.ends_with(b"SUMMARY") {
            let inc = int_after(l, b"INCREMENT").unwrap_or(0);
            self.summary.inc = Some(inc);
            if kind == LogKind::Dat {
                self.cur_step(li).incs.push(IncInfo { line: li, num: inc, cut: 0, detail: String::new() });
            }
            return C_INC;
        }
        if find(l, b"TIME INCREMENT COMPLETED").is_some() || find(l, b"STEP TIME COMPLETED").is_some() {
            if let Some(v) = text_after(l, b"TOTAL TIME COMPLETED") {
                self.summary.total_time = Some(v.clone());
                if let Some(x) = self.steps.last_mut().and_then(|s| s.incs.last_mut()) {
                    x.detail = format!("total time {v}");
                }
            }
            if let Some(v) = text_after(l, b"STEP TIME COMPLETED") {
                self.summary.step_time = Some(v);
            }
            return C_INC;
        }
        // status file
        if kind == LogKind::Sta {
            if li < 3 && t.starts_with(b"ABAQUS") {
                self.summary.product = Some(String::from_utf8_lossy(raw).split("  ").next().unwrap_or("").trim().to_string());
                return C_HEAD;
            }
            if (find(l, b"STEP").is_some() && find(l, b"INC").is_some() && find(l, b"ATT").is_some()) || (t.starts_with(b"STEP") && find(l, b"TOTAL").is_some()) || t.starts_with(b"INCREMENT") || find(l, b"ITERS").is_some() {
                return C_HEAD;
            }
            let tk = tokens(t);
            if tk.len() >= 7 {
                if let (Some(stp), Some(inc)) = (int_of(tk[0]), int_of(tk[1])) {
                    // Standard: STEP INC ATT SEVERE EQUIL TOTAL TOTAL-TIME STEP-TIME INC
                    let att = String::from_utf8_lossy(tk[2]).to_string();
                    let cut = att.ends_with('U');
                    if self.steps.last().map(|s| s.num) != Some(stp) {
                        self.steps.push(StepInfo { line: li, num: stp, label: format!("Step {stp}"), incs: Vec::new() });
                    }
                    let total = String::from_utf8_lossy(tk[6]).to_string();
                    let st = self.steps.last_mut().unwrap();
                    if st.incs.last().map(|x| x.num) != Some(inc) {
                        st.incs.push(IncInfo { line: li, num: inc, cut: 0, detail: String::new() });
                    }
                    let x = st.incs.last_mut().unwrap();
                    if cut {
                        x.cut += 1;
                    } else {
                        x.detail = format!("total time {total}");
                    }
                    self.summary.step = Some(stp);
                    self.summary.inc = Some(inc);
                    if cut {
                        self.summary.cutbacks += 1;
                        self.mark(li, crate::theme::rgb(230, 140, 0));
                        return C_CUT;
                    }
                    self.summary.total_time = Some(total);
                    self.summary.step_time = tk.get(7).map(|x| String::from_utf8_lossy(x).into_owned());
                    return C_ROW;
                }
            }
            if tk.len() >= 3 {
                // Explicit: INCREMENT TOTAL-TIME CPU-TIME STABLE-INC …
                if let Some(inc) = int_of(tk[0]) {
                    if tk[1].contains(&b'E') && tk[1].contains(&b'.') {
                        self.summary.inc = Some(inc);
                        self.summary.total_time = Some(String::from_utf8_lossy(tk[1]).into_owned());
                        return C_ROW;
                    }
                }
            }
        }
        C_PLAIN
    }

    /// Re-read the file if it changed: appended bytes are added; a shorter or rewritten file is
    /// read again completely (returns Err(true) for that, so the caller can do it in the
    /// background). Ok(true): new lines.
    pub fn refresh(&mut self) -> Result<bool, bool> {
        let Ok(md) = std::fs::metadata(&self.path) else { return Ok(false) };
        let (len, mt) = (md.len(), md.modified().ok());
        if (len, mt) == self.stamp {
            return Ok(false);
        }
        if len < self.stamp.0 {
            return Err(true);
        }
        if len == self.stamp.0 {
            // touched but not longer: compare the end
            self.stamp = (len, mt);
            return Ok(false);
        }
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Ok(false),
        };
        if f.seek(SeekFrom::Start(self.stamp.0)).is_err() {
            return Err(true);
        }
        let mut more = Vec::new();
        if f.read_to_end(&mut more).is_err() {
            return Ok(false);
        }
        self.stamp = (self.stamp.0 + more.len() as u64, mt);
        self.append(more);
        Ok(true)
    }
}

// ---------------------------------------------------------------- outline tree

#[derive(Clone, Copy, PartialEq, Eq)]
enum NKind {
    Group,
    Err,
    Warn,
    Note,
    Step,
    Inc,
    Cut,
}

struct LNode {
    label: String,
    detail: String,
    line: Option<u64>,
    kind: NKind,
    children: Vec<LNode>,
}

fn sig(t: &str) -> String {
    let mut out = String::new();
    let mut digit = false;
    for c in t.chars() {
        if c.is_ascii_digit() {
            if !digit {
                out.push('#');
            }
            digit = true;
        } else {
            digit = false;
            out.push(c);
        }
        if out.len() > 160 {
            break;
        }
    }
    out
}

fn build_tree(d: &LogData) -> Vec<LNode> {
    let mut root = Vec::new();
    for (sev, name, kind) in [(C_ERR, "Errors", NKind::Err), (C_WARN, "Warnings", NKind::Warn), (C_NOTE, "Notes", NKind::Note)] {
        let ms: Vec<&LogMsg> = d.msgs.iter().filter(|m| m.sev == sev).collect();
        if ms.is_empty() {
            continue;
        }
        // group identical messages (numbers ignored)
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<&LogMsg>> = HashMap::new();
        for m in &ms {
            let k = sig(&m.text);
            if !groups.contains_key(&k) {
                order.push(k.clone());
            }
            groups.entry(k).or_default().push(m);
        }
        let mut kids = Vec::new();
        for k in order {
            let g = &groups[&k];
            let short: String = g[0].text.chars().take(140).collect();
            if g.len() == 1 {
                kids.push(LNode { label: short, detail: format!("Ln {}", util::fmt_int(g[0].line + 1)), line: Some(g[0].line), kind, children: Vec::new() });
            } else {
                let occ = g.iter().take(5000).map(|m| LNode { label: m.text.chars().take(140).collect(), detail: format!("Ln {}", util::fmt_int(m.line + 1)), line: Some(m.line), kind, children: Vec::new() }).collect();
                kids.push(LNode { label: short, detail: format!("×{}", util::fmt_int(g.len() as u64)), line: Some(g[0].line), kind, children: occ });
            }
        }
        root.push(LNode { label: format!("{name}"), detail: util::fmt_int(ms.len() as u64), line: Some(ms[0].line), kind: NKind::Group, children: kids });
    }
    if !d.steps.is_empty() {
        let kids = d
            .steps
            .iter()
            .map(|s| {
                let incs: Vec<LNode> = s
                    .incs
                    .iter()
                    .take(100_000)
                    .map(|x| LNode {
                        label: format!("Increment {}", x.num),
                        detail: if x.cut > 0 { format!("{} cutback{} {}", x.cut, if x.cut == 1 { "" } else { "s" }, x.detail) } else { x.detail.clone() },
                        line: Some(x.line),
                        kind: if x.cut > 0 { NKind::Cut } else { NKind::Inc },
                        children: Vec::new(),
                    })
                    .collect();
                let cuts: u32 = s.incs.iter().map(|x| x.cut).sum();
                let detail = if s.incs.is_empty() { String::new() } else if cuts > 0 { format!("{} inc · {} cutbacks", s.incs.len(), cuts) } else { format!("{} inc", s.incs.len()) };
                LNode { label: s.label.clone(), detail, line: Some(s.line), kind: NKind::Step, children: incs }
            })
            .collect();
        root.push(LNode { label: "Steps".into(), detail: util::fmt_int(d.steps.len() as u64), line: Some(d.steps[0].line), kind: NKind::Group, children: kids });
    }
    root
}

struct Row {
    depth: usize,
    path: Vec<u32>,
    key: String,
    open: bool,
}

fn flatten(nodes: &[LNode], depth: usize, prefix: &str, path: &mut Vec<u32>, open: &HashMap<String, bool>, out: &mut Vec<Row>) {
    for (i, n) in nodes.iter().enumerate() {
        let key = format!("{prefix}/{}", if depth == 0 { n.label.clone() } else { format!("{}#{}", n.label, n.line.unwrap_or(0)) });
        let def = depth == 0 && (n.label == "Errors" || n.label == "Steps" && n.children.len() <= 20);
        let o = *open.get(&key).unwrap_or(&def);
        path.push(i as u32);
        out.push(Row { depth, path: path.clone(), key: key.clone(), open: o });
        if o && !n.children.is_empty() {
            flatten(&n.children, depth + 1, &key, path, open, out);
        }
        path.pop();
    }
}

fn node_at<'a>(root: &'a [LNode], path: &[u32]) -> Option<&'a LNode> {
    let mut list = root;
    let mut node = None;
    for &i in path {
        let n = list.get(i as usize)?;
        node = Some(n);
        list = &n.children;
    }
    node
}

// ---------------------------------------------------------------- tab

#[derive(Default)]
pub struct LogFind {
    pub open: bool,
    pub query: String,
    pub focus: bool,
    pub msg: String,
}

pub struct LogTab {
    pub data: LogData,
    pub view: LineViewState,
    pub monitor: bool,
    last_poll: f64,
    /// when the file last grew (UI time)
    pub last_growth: Option<f64>,
    /// the file changed on disk (shown when not monitoring)
    pub changed: bool,
    tree: Vec<LNode>,
    tree_ver: u64,
    rows: Vec<Row>,
    rows_key: (u64, usize, u64),
    open: HashMap<String, bool>,
    open_rev: u64,
    pub find: LogFind,
    pub reload: Option<Receiver<io::Result<LogData>>>,
    pub id: egui::Id,
}

impl LogTab {
    pub fn new(data: LogData) -> LogTab {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // a status file of a running job: follow it right away
        let follow = data.kind == LogKind::Sta && data.summary.status == Status::Unknown;
        let mut view = LineViewState::default();
        if data.kind == LogKind::Sta {
            view.follow = true;
        }
        LogTab {
            data,
            view,
            monitor: follow,
            last_poll: 0.0,
            last_growth: None,
            changed: false,
            tree: Vec::new(),
            tree_ver: u64::MAX,
            rows: Vec::new(),
            rows_key: (u64::MAX, 0, 0),
            open: HashMap::new(),
            open_rev: 0,
            find: LogFind::default(),
            reload: None,
            id: egui::Id::new(("inplite-log", n)),
        }
    }

    pub fn title(&self) -> String {
        self.data.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn start_reload(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let p = self.data.path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LogData::read(&p));
        });
        self.reload = Some(rx);
    }

    /// Background work and monitoring. Returns true while something is pending.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let now = ctx.input(|i| i.time);
        if let Some(rx) = &self.reload {
            match rx.try_recv() {
                Ok(Ok(d)) => {
                    let keep_follow = self.view.follow || self.monitor;
                    self.data = d;
                    self.reload = None;
                    self.changed = false;
                    self.tree_ver = u64::MAX;
                    if keep_follow {
                        self.view.follow = true;
                    }
                    self.last_growth = Some(now);
                }
                Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self.reload = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            }
        }
        let period = if self.monitor { 0.5 } else { 2.0 };
        if now - self.last_poll >= period {
            self.last_poll = now;
            if self.monitor {
                match self.data.refresh() {
                    Ok(true) => {
                        self.view.follow = true;
                        self.last_growth = Some(now);
                    }
                    Ok(false) => {}
                    Err(_) => self.start_reload(),
                }
            } else if let Ok(md) = std::fs::metadata(&self.data.path) {
                self.changed = (md.len(), md.modified().ok()) != self.data.stamp;
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(period));
        self.reload.is_some()
    }
}

struct Src<'a>(&'a LogData);

impl LineSource for Src<'_> {
    fn count(&self) -> u64 {
        self.0.lines()
    }
    fn line(&self, i: u64) -> (String, LineStyle) {
        let t = lineview::expand_tabs(&String::from_utf8_lossy(self.0.line_bytes(i)));
        let c = self.0.cls.get(i as usize).copied().unwrap_or(C_PLAIN);
        (t, style_of(c))
    }
    fn max_cols(&self) -> usize {
        self.0.max_cols
    }
    fn marks(&self) -> &[(u64, Color32)] {
        &self.0.marks
    }
}

fn style_of(c: u8) -> LineStyle {
    use crate::syntax::Palette;
    let t = crate::theme::t;
    let st = |fg: Color32, bg: Option<Color32>, strong: bool| LineStyle { fg: Some(fg), bg, strong, spans: Vec::new() };
    match c {
        C_ERR => st(t(Palette::ERR), Some(t(Palette::ERR_BG)), true),
        C_ERR_CONT => st(t(Palette::ERR), None, false),
        C_WARN => st(crate::theme::rgb(176, 96, 0), Some(t(Palette::WARN_BG)), true),
        C_WARN_CONT => st(crate::theme::rgb(176, 96, 0), None, false),
        C_NOTE => st(crate::theme::rgb(0, 110, 140), None, true),
        C_NOTE_CONT => st(crate::theme::rgb(0, 110, 140), None, false),
        C_STEP => st(t(Palette::KEYWORD), Some(crate::theme::rgb(226, 236, 252)), true),
        C_INC => st(crate::theme::rgb(30, 120, 60), None, false),
        C_CUT => st(crate::theme::rgb(190, 100, 0), Some(crate::theme::rgb(255, 246, 225)), true),
        C_HEAD => st(t(Palette::PARAM), None, true),
        C_OK => st(crate::theme::rgb(20, 120, 40), Some(crate::theme::rgb(222, 246, 228)), true),
        C_FAIL => st(t(Palette::ERR), Some(t(Palette::ERR_BG)), true),
        C_TITLE => st(t(Palette::KEYWORD), None, true),
        C_ROW => st(t(Palette::DATA), None, false),
        C_TIME => st(t(Palette::PARAM), None, false),
        _ => LineStyle::default(),
    }
}

fn eye(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(30.0, 22.0), Sense::click());
    let p = ui.painter();
    let bg = if on {
        crate::theme::rgb(214, 228, 252)
    } else if resp.hovered() {
        crate::theme::rgb(236, 239, 244)
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 4.0, bg);
    let c = if on { crate::theme::rgb(30, 80, 180) } else { crate::theme::rgb(110, 115, 125) };
    let cx = rect.center();
    // eye: two arcs + pupil
    let pts_top: Vec<Pos2> = (0..=16).map(|k| {
        let a = std::f32::consts::PI * (k as f32 / 16.0);
        Pos2::new(cx.x - 10.0 * a.cos(), cx.y - 6.0 * a.sin())
    }).collect();
    let pts_bot: Vec<Pos2> = (0..=16).map(|k| {
        let a = std::f32::consts::PI * (k as f32 / 16.0);
        Pos2::new(cx.x - 10.0 * a.cos(), cx.y + 6.0 * a.sin())
    }).collect();
    p.add(egui::Shape::line(pts_top, egui::Stroke::new(1.6_f32, c)));
    p.add(egui::Shape::line(pts_bot, egui::Stroke::new(1.6_f32, c)));
    p.circle_filled(cx, 3.2, c);
    resp
}

/// Toolbar + text of a log tab.
pub fn log_ui(ui: &mut egui::Ui, tab: &mut LogTab, font_size: f32) {
    let now = ui.input(|i| i.time);
    ui.horizontal(|ui| {
        let r = eye(ui, tab.monitor).on_hover_text(if tab.monitor {
            "Monitoring: the file is re-read every 0.5 s and the view follows its end (click to stop)"
        } else {
            "Monitor this file (tail -f): re-read it while Abaqus writes it and follow its end"
        });
        if r.clicked() {
            tab.monitor = !tab.monitor;
            if tab.monitor {
                tab.view.follow = true;
                if let Err(_) = tab.data.refresh() {
                    tab.start_reload();
                }
            }
        }
        if tab.monitor {
            let ago = tab.last_growth.map(|t| now - t);
            let txt = match ago {
                Some(a) if a < 2.0 => "monitoring · updated just now".to_string(),
                Some(a) => format!("monitoring · last change {} ago", fmt_ago(a)),
                None => "monitoring · no change yet".into(),
            };
            ui.label(RichText::new(txt).small().color(crate::theme::rgb(30, 80, 180)));
        }
        let reload = ui.add(egui::Button::new(if tab.changed && !tab.monitor { "Reload (changed on disk)" } else { "Reload" }));
        if reload.clicked() {
            tab.start_reload();
        }
        if tab.reload.is_some() {
            ui.spinner();
        }
        if ui.button("End").on_hover_text("Go to the end of the file (Ctrl+End)").clicked() {
            tab.view.follow = true;
        }
        ui.separator();
        let fr = ui.add(egui::TextEdit::singleline(&mut tab.find.query).desired_width(200.0).hint_text("find (Ctrl+F)"));
        if tab.find.focus {
            fr.request_focus();
            tab.find.focus = false;
        }
        let enter = fr.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let back = ui.input(|i| i.modifiers.shift);
        let mut go = if enter { if back { -1 } else { 1 } } else { 0 };
        if ui.button("▲").on_hover_text("Previous (Shift+Enter)").clicked() {
            go = -1;
        }
        if ui.button("▼").on_hover_text("Next (Enter)").clicked() {
            go = 1;
        }
        if enter {
            fr.request_focus();
        }
        if go != 0 && !tab.find.query.is_empty() {
            match find_line(&tab.data, &tab.find.query, tab.view.sel.map(|s| s.1).unwrap_or(tab.view.top), go) {
                Some(l) => {
                    tab.view.go(l);
                    tab.find.msg.clear();
                }
                None => tab.find.msg = "not found".into(),
            }
        }
        if !tab.find.msg.is_empty() {
            ui.label(RichText::new(&tab.find.msg).small().color(crate::theme::t(crate::syntax::Palette::ERR)));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{} · {} lines · {}", tab.data.kind.label(), util::fmt_int(tab.data.lines()), util::fmt_bytes(tab.data.bytes.len() as u64))).small().weak());
        });
    });
    ui.add_space(2.0);
    let src = Src(&tab.data);
    lineview::show(ui, tab.id, &mut tab.view, &src, font_size);
}

fn fmt_ago(s: f64) -> String {
    if s < 90.0 {
        format!("{:.0} s", s)
    } else if s < 5400.0 {
        format!("{:.0} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

/// Next / previous line containing `q` (ASCII case-insensitive), wrapping around.
pub fn find_line(d: &LogData, q: &str, from: u64, dir: i32) -> Option<u64> {
    let n = d.lines();
    if n == 0 {
        return None;
    }
    let q = q.as_bytes().to_ascii_uppercase();
    let hit = |i: u64| -> bool {
        let l = d.line_bytes(i);
        l.len() >= q.len() && l.windows(q.len()).any(|w| w.eq_ignore_ascii_case(&q))
    };
    for k in 1..=n {
        let i = if dir > 0 { (from + k) % n } else { (from + n - (k % n)) % n };
        if hit(i) {
            return Some(i);
        }
    }
    None
}

/// Summary and tree of a log tab (left panel).
pub fn log_outline_ui(ui: &mut egui::Ui, tab: &mut LogTab) {
    let s = &tab.data.summary;
    ui.label(RichText::new("SUMMARY").small().strong().color(crate::theme::rgb(110, 115, 125)));
    let (st_txt, st_col) = match (s.status, tab.monitor) {
        (Status::Completed, _) => ("Completed", crate::theme::rgb(20, 130, 50)),
        (Status::Failed, _) => ("Not completed", crate::theme::t(crate::syntax::Palette::ERR)),
        (Status::Unknown, true) => ("Running / not finished", crate::theme::rgb(30, 80, 180)),
        (Status::Unknown, false) => ("Not finished (or no status line)", crate::theme::rgb(120, 125, 135)),
    };
    let mut go: Option<u64> = None;
    egui::Grid::new(tab.id.with("sum")).num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
        ui.label("Status");
        let r = ui.add(egui::Label::new(RichText::new(st_txt).strong().color(st_col)).sense(Sense::click()));
        if let (true, Some(l)) = (r.clicked(), s.status_line) {
            go = Some(l);
        }
        ui.end_row();
        if let Some(p) = &s.product {
            ui.label("Product");
            ui.label(RichText::new(p).small());
            ui.end_row();
        }
        let mut count = |ui: &mut egui::Ui, name: &str, n: usize, c: Color32, sev: u8| {
            ui.label(name);
            let txt = RichText::new(util::fmt_int(n as u64)).strong().color(if n > 0 { c } else { crate::theme::rgb(140, 145, 155) });
            let r = ui.add(egui::Label::new(txt).sense(Sense::click())).on_hover_text("click: first one");
            if r.clicked() {
                go = tab.data.msgs.iter().find(|m| m.sev == sev).map(|m| m.line);
            }
            ui.end_row();
        };
        count(ui, "Errors", s.errors, crate::theme::t(crate::syntax::Palette::ERR), C_ERR);
        count(ui, "Warnings", s.warnings, crate::theme::rgb(196, 110, 0), C_WARN);
        if s.notes > 0 {
            count(ui, "Notes", s.notes, crate::theme::rgb(0, 110, 140), C_NOTE);
        }
        if s.step.is_some() || s.inc.is_some() {
            ui.label("Last step / inc.");
            ui.label(format!("{} / {}", s.step.map(|x| x.to_string()).unwrap_or("-".into()), s.inc.map(|x| x.to_string()).unwrap_or("-".into())));
            ui.end_row();
        }
        if s.cutbacks > 0 {
            ui.label("Cutbacks");
            ui.label(RichText::new(util::fmt_int(s.cutbacks as u64)).color(crate::theme::rgb(196, 110, 0)));
            ui.end_row();
        }
        if let Some(t) = &s.step_time {
            ui.label("Step time");
            ui.label(t);
            ui.end_row();
        }
        if let Some(t) = &s.total_time {
            ui.label("Total time");
            ui.label(t);
            ui.end_row();
        }
        for (k, v) in &s.problem {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
        if let Some(w) = &s.wallclock {
            ui.label("Wallclock (s)");
            ui.label(w);
            ui.end_row();
        }
        if let Some(w) = &s.cpu {
            ui.label("CPU time (s)");
            ui.label(w);
            ui.end_row();
        }
    });
    ui.separator();
    // tree
    if tab.tree_ver != tab.data.ver {
        tab.tree = build_tree(&tab.data);
        tab.tree_ver = tab.data.ver;
    }
    let key = (tab.tree_ver, tab.open.len(), tab.open_rev);
    if tab.rows_key != key {
        let mut rows = Vec::new();
        flatten(&tab.tree, 0, "", &mut Vec::new(), &tab.open, &mut rows);
        tab.rows = rows;
        tab.rows_key = key;
    }
    let row_h = 21.0;
    let font = FontId::proportional(12.5);
    let small = FontId::proportional(11.0);
    let cur = tab.view.sel.map(|s| s.1);
    let mut toggles: Vec<(String, bool)> = Vec::new();
    if tab.rows.is_empty() {
        ui.label(RichText::new("No errors, warnings or steps found (yet).").weak());
    }
    egui::ScrollArea::vertical().id_salt(tab.id.with("tree")).auto_shrink([false, false]).show_rows(ui, row_h, tab.rows.len(), |ui, range| {
        for r in &tab.rows[range] {
            let Some(n) = node_at(&tab.tree, &r.path) else { continue };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            let p = ui.painter();
            if n.line.is_some() && n.line == cur && n.children.is_empty() {
                p.rect_filled(rect, 3.0, crate::theme::rgb(220, 232, 252));
            } else if resp.hovered() {
                p.rect_filled(rect, 3.0, crate::theme::rgb(236, 239, 244));
            }
            let mut x = rect.left() + 4.0 + r.depth as f32 * 14.0;
            let cy = rect.center().y;
            if !n.children.is_empty() {
                let c = crate::theme::rgb(120, 125, 135);
                let pts = if r.open {
                    vec![Pos2::new(x + 2.0, cy - 2.5), Pos2::new(x + 10.0, cy - 2.5), Pos2::new(x + 6.0, cy + 3.0)]
                } else {
                    vec![Pos2::new(x + 3.5, cy - 4.0), Pos2::new(x + 3.5, cy + 4.0), Pos2::new(x + 9.0, cy)]
                };
                p.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
            }
            x += 16.0;
            let (badge, col) = match n.kind {
                NKind::Err => ("E", crate::theme::t(crate::syntax::Palette::ERR)),
                NKind::Warn => ("W", crate::theme::rgb(220, 140, 0)),
                NKind::Note => ("N", crate::theme::rgb(0, 130, 160)),
                NKind::Step => ("S", crate::theme::rgb(40, 150, 70)),
                NKind::Inc => ("", crate::theme::rgb(150, 190, 160)),
                NKind::Cut => ("", crate::theme::rgb(230, 140, 0)),
                NKind::Group => ("", crate::theme::rgb(110, 115, 125)),
            };
            if !badge.is_empty() {
                let br = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::new(15.0, 15.0));
                p.rect_filled(br, 3.0, col);
                p.text(br.center(), egui::Align2::CENTER_CENTER, badge, FontId::proportional(10.0), crate::theme::t(Color32::WHITE));
                x += 20.0;
            } else if n.kind != NKind::Group {
                p.circle_filled(Pos2::new(x + 4.0, cy), 3.0, col);
                x += 12.0;
            }
            let detail_w = if n.detail.is_empty() { 0.0 } else { ui.fonts_mut(|f| f.layout_no_wrap(n.detail.clone(), small.clone(), Color32::GRAY).size().x) + 10.0 };
            let max_w = (rect.right() - x - detail_w - 6.0).max(20.0);
            let color = if n.kind == NKind::Group { crate::theme::rgb(60, 64, 72) } else { crate::theme::rgb(40, 44, 52) };
            let galley = ui.fonts_mut(|f| {
                let mut job = egui::text::LayoutJob::simple_singleline(n.label.clone(), if n.kind == NKind::Group { FontId::proportional(13.0) } else { font.clone() }, color);
                job.wrap.max_width = max_w;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                f.layout_job(job)
            });
            p.galley(Pos2::new(x, cy - galley.size().y / 2.0), galley, color);
            if !n.detail.is_empty() {
                p.text(Pos2::new(rect.right() - 6.0, cy), egui::Align2::RIGHT_CENTER, &n.detail, small.clone(), crate::theme::rgb(140, 145, 155));
            }
            let resp = resp.on_hover_text(&n.label);
            if resp.clicked() {
                // a click opens a closed entry and closes an open one
                if !n.children.is_empty() {
                    toggles.push((r.key.clone(), !r.open));
                }
                if let Some(l) = n.line {
                    if n.kind != NKind::Group {
                        go = Some(l);
                    }
                }
            }
        }
    });
    for (k, v) in toggles {
        tab.open.insert(k, v);
        tab.open_rev += 1;
    }
    if let Some(l) = go {
        tab.view.go(l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_msg_and_sta() {
        let dir = std::env::temp_dir().join(format!("inplite_log_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let msg = dir.join("job.msg");
        std::fs::write(
            &msg,
            "\n                        S T E P       1     S T A T I C   A N A L Y S I S\n\n INCREMENT     1 STARTS. ATTEMPT NUMBER  1, TIME INCREMENT   1.000E-01\n\n ***WARNING: THE STRAIN INCREMENT HAS EXCEEDED FIFTY TIMES THE STRAIN TO CAUSE\n             FIRST YIELD AT 12 POINTS\n\n INCREMENT     1 STARTS. ATTEMPT NUMBER  2, TIME INCREMENT   2.500E-02\n\n  TIME INCREMENT COMPLETED  2.500E-02,  FRACTION OF STEP COMPLETED  2.500E-02\n  STEP TIME COMPLETED       2.500E-02,  TOTAL TIME COMPLETED        2.500E-02\n\n ***WARNING: THE STRAIN INCREMENT HAS EXCEEDED FIFTY TIMES THE STRAIN TO CAUSE\n             FIRST YIELD AT 3 POINTS\n\n ***ERROR: TOO MANY ATTEMPTS MADE FOR THIS INCREMENT\n\n THE ANALYSIS HAS NOT BEEN COMPLETED\n",
        )
        .unwrap();
        let d = LogData::read(&msg).unwrap();
        assert_eq!(d.summary.warnings, 2);
        assert_eq!(d.summary.errors, 1);
        assert_eq!(d.summary.cutbacks, 1);
        assert_eq!(d.summary.status, Status::Failed);
        assert_eq!(d.steps.len(), 1);
        assert_eq!(d.steps[0].label, "STEP 1 STATIC ANALYSIS");
        assert_eq!(d.steps[0].incs.len(), 1);
        assert_eq!(d.steps[0].incs[0].cut, 1);
        assert_eq!(d.summary.total_time.as_deref(), Some("2.500E-02"));
        assert_eq!(d.cls[6], C_WARN_CONT);
        let tree = build_tree(&d);
        let w = tree.iter().find(|n| n.label == "Warnings").unwrap();
        assert_eq!(w.children.len(), 1, "the two warnings differ only in numbers: one group");
        assert_eq!(w.children[0].detail, "×2");
        // a status file that grows
        let sta = dir.join("job.sta");
        let head = "Abaqus/Standard 2024                  DATE 26-Oct-2026 TIME 18:00:00\n SUMMARY OF JOB INFORMATION:\n STEP  INC ATT SEVERE EQUIL TOTAL  TOTAL      STEP       INC OF       DOF    IF\n               DISCON ITERS ITERS  TIME/      TIME/LPF   TIME/LPF     MONITOR RIKS\n               ITERS               FREQ\n";
        std::fs::write(&sta, format!("{head}   1     1   1     0     3     3  0.100      0.100      0.1000    \n   1     2   1U    0     6     6  0.100      0.100      0.1000    \n")).unwrap();
        let mut d = LogData::read(&sta).unwrap();
        assert_eq!(d.summary.cutbacks, 1);
        assert_eq!(d.summary.inc, Some(2));
        assert_eq!(d.summary.status, Status::Unknown);
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&sta).unwrap();
        write!(f, "   1     2   2     0     2     2  0.125      0.125      0.02500   \n THE ANALYSIS HAS COMPLETED").unwrap();
        drop(f);
        // a file written in the same second may keep its time stamp: force a re-read by length
        assert_eq!(d.refresh(), Ok(true));
        assert_eq!(d.summary.total_time.as_deref(), Some("0.125"));
        assert_eq!(d.summary.status, Status::Unknown, "last line incomplete");
        let mut f = std::fs::OpenOptions::new().append(true).open(&sta).unwrap();
        write!(f, " SUCCESSFULLY\n").unwrap();
        drop(f);
        assert_eq!(d.refresh(), Ok(true));
        assert_eq!(d.summary.status, Status::Completed);
        assert_eq!(d.lines(), 9);
        assert_eq!(d.steps[0].incs.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod append_tests {
    use super::*;
    #[test]
    fn appends_line_by_line() {
        let mut src = String::from("Abaqus/Standard 2024\n SUMMARY OF JOB INFORMATION:\n STEP  INC ATT SEVERE EQUIL TOTAL  TOTAL      STEP       INC OF       DOF    IF\n");
        for i in 1..60 {
            src += &format!("   1  {i:4}   1     0     3     3  {:9.3}  {:9.3}  0.1\n", i as f64 * 0.1, i as f64 * 0.1);
        }
        src += " THE ANALYSIS HAS COMPLETED SUCCESSFULLY\n";
        let src = src.into_bytes();
        let lines: Vec<&[u8]> = src.split_inclusive(|c| *c == b'\n').collect();
        let dir = std::env::temp_dir().join(format!("inplite_app_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("a.sta");
        std::fs::write(&p, lines[..12].concat()).unwrap();
        // (the first 12 lines, then one line per write)
        let mut d = LogData::read(&p).unwrap();
        for k in 12..lines.len() {
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            std::io::Write::write_all(&mut f, lines[k]).unwrap();
            drop(f);
            let _ = d.refresh();
        }
        for i in 0..d.lines() {
            assert!(!d.line_bytes(i).contains(&b'\n'), "line {i}: {:?}", String::from_utf8_lossy(d.line_bytes(i)));
        }
        assert_eq!(d.lines() as usize, lines.len());
    }
}
