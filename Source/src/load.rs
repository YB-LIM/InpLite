//! Fast scanner: turns a (possibly multi-GB) .inp file into a small editable "skeleton" text.
//!
//! Every run of data lines longer than `threshold` lines becomes a bulk block that stays in the
//! memory map; everything else (keyword lines, comments, small data blocks) is decoded into text.

use crate::store::{Bulk, Source, STORE};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::CrLf => b"\r\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
        }
    }
}

pub struct Loaded {
    pub src: Arc<Source>,
    pub text: String,
    pub eol: Eol,
    pub mixed_eol: bool,
    pub latin1: bool,
    pub bom: bool,
    pub bulks: Vec<u64>,
    pub total_lines: u64,
    /// threshold actually used (may be lower than asked for, to keep the editable text small)
    pub threshold: u64,
    /// threshold asked for
    pub wanted: u64,
    pub elapsed_ms: u64,
}

pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress { done: AtomicU64::new(0), total: AtomicU64::new(0), cancel: AtomicBool::new(false) })
    }
    pub fn frac(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 {
            0.0
        } else {
            self.done.load(Ordering::Relaxed) as f32 / t as f32
        }
    }
}

enum Region {
    Text(usize, usize),
    Bulk { start: usize, end: usize, lines: u64, kw: Option<(usize, usize)>, comment: bool },
}

/// A run of consecutive data lines following a keyword line, or (`comment`) of consecutive
/// comment lines (e.g. a commented-out mesh).
struct Run {
    start: usize,
    end: usize,
    lines: u64,
    kw: Option<(usize, usize)>,
    comment: bool,
}

/// Comment runs shorter than this are never kept on disk (a few comment lines are common).
pub const COMMENT_RUN_MIN: u64 = 500;

struct Scan {
    runs: Vec<Run>,
    total_lines: u64,
    crlf: u64,
    lf: u64,
}

/// Target size of the editable text. egui re-lays out the whole text on each edit, so keeping the
/// skeleton around this many lines keeps typing responsive even for huge models.
pub const SKELETON_TARGET_LINES: u64 = 10_000;

fn ends_with_comma(line: &[u8]) -> bool {
    let mut e = line.len();
    while e > 0 && (line[e - 1] == b' ' || line[e - 1] == b'\t') {
        e -= 1;
    }
    e > 0 && line[e - 1] == b','
}

fn scan(b: &[u8], prog: &Progress) -> io::Result<Scan> {
    struct Cur {
        start: usize,
        start_line: u64,
        last_end: usize,
        last_line: u64,
    }
    let n = b.len();
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut line_no: u64 = 0;
    let mut kw_cont = false;
    let mut cur_kw: Option<(usize, usize)> = None;
    let mut run: Option<Cur> = None;
    // consecutive comment lines (blank lines end them)
    let mut crun: Option<Cur> = None;
    let (mut crlf, mut lf) = (0u64, 0u64);
    let mut last_report = 0usize;

    macro_rules! finish_run {
        () => {
            if let Some(r) = run.take() {
                runs.push(Run { start: r.start, end: r.last_end, lines: r.last_line - r.start_line + 1, kw: cur_kw, comment: false });
            }
        };
    }
    // a long comment run ends the data run before it (the data after it starts a new run)
    macro_rules! finish_crun {
        () => {
            if let Some(c) = crun.take() {
                let lines = c.last_line - c.start_line + 1;
                if lines >= COMMENT_RUN_MIN {
                    finish_run!();
                    runs.push(Run { start: c.start, end: c.last_end, lines, kw: None, comment: true });
                }
            }
        };
    }

    while pos < n {
        let nl = memchr::memchr(b'\n', &b[pos..]).map(|i| pos + i);
        let le = nl.unwrap_or(n);
        let mut ce = le;
        if ce > pos && b[ce - 1] == b'\r' {
            ce -= 1;
            if nl.is_some() {
                crlf += 1;
            }
        } else if nl.is_some() {
            lf += 1;
        }
        let line = &b[pos..ce];
        let mut i = 0;
        while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
            i += 1;
        }
        let blank = i == line.len();
        let star = !blank && line[i] == b'*';
        let comment = star && i + 1 < line.len() && line[i + 1] == b'*';
        if kw_cont && !blank && !comment && !star {
            kw_cont = ends_with_comma(line);
        } else if comment {
            // comments do not end a data run (trailing ones are trimmed)
            match &mut crun {
                Some(c) => {
                    c.last_end = ce;
                    c.last_line = line_no;
                }
                None => crun = Some(Cur { start: pos, start_line: line_no, last_end: ce, last_line: line_no }),
            }
        } else if blank {
            finish_crun!();
        } else if star {
            finish_run!();
            finish_crun!();
            cur_kw = Some((pos, ce));
            kw_cont = ends_with_comma(line);
        } else {
            finish_crun!();
            kw_cont = false;
            match &mut run {
                Some(r) => {
                    r.last_end = ce;
                    r.last_line = line_no;
                }
                None => run = Some(Cur { start: pos, start_line: line_no, last_end: ce, last_line: line_no }),
            }
        }
        line_no += 1;
        pos = match nl {
            Some(x) => x + 1,
            None => n,
        };
        if pos - last_report > (8 << 20) {
            last_report = pos;
            prog.done.store(pos as u64, Ordering::Relaxed);
            if prog.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
    }
    finish_run!();
    finish_crun!();
    Ok(Scan { runs, total_lines: line_no, crlf, lf })
}

/// A run is kept on disk with threshold `th`.
fn on_disk(r: &Run, th: u64) -> bool {
    if r.comment { r.lines >= th.max(COMMENT_RUN_MIN) } else { r.lines >= th }
}

/// Pick the largest threshold <= `wanted` that keeps the skeleton near SKELETON_TARGET_LINES.
fn choose_threshold(sc: &Scan, wanted: u64) -> u64 {
    let data_lines: u64 = sc.runs.iter().map(|r| r.lines).sum();
    // all runs (data and comment lines); the rest of the lines always stay in the text
    let other = sc.total_lines.saturating_sub(data_lines);
    let mut th = wanted.max(2);
    loop {
        let mut lines = other;
        for r in &sc.runs {
            lines += if on_disk(r, th) { 1 } else { r.lines };
        }
        if lines <= SKELETON_TARGET_LINES || th <= 50 {
            return th;
        }
        th = (th / 2).max(50);
    }
}

fn regions(sc: &Scan, th: u64, n: usize) -> (Vec<Region>, usize) {
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut text_bytes = 0;
    for r in &sc.runs {
        if on_disk(r, th) {
            out.push(Region::Text(text_start, r.start));
            text_bytes += r.start - text_start;
            out.push(Region::Bulk { start: r.start, end: r.end, lines: r.lines, kw: r.kw, comment: r.comment });
            text_start = r.end;
        }
    }
    out.push(Region::Text(text_start, n));
    text_bytes += n - text_start;
    (out, text_bytes)
}

fn push_decoded(out: &mut String, b: &[u8], latin1: bool) {
    // normalize CRLF -> LF; keep lone CR
    let mut s = 0;
    let mut i = 0;
    let flush = |out: &mut String, seg: &[u8]| {
        if latin1 {
            out.extend(seg.iter().map(|&c| c as char));
        } else {
            out.push_str(std::str::from_utf8(seg).unwrap_or(""));
        }
    };
    while let Some(k) = memchr::memchr(b'\r', &b[i..]) {
        let p = i + k;
        if p + 1 < b.len() && b[p + 1] == b'\n' {
            flush(out, &b[s..p]);
            s = p + 1;
        }
        i = p + 1;
        if i >= b.len() {
            break;
        }
    }
    flush(out, &b[s..]);
}

pub fn load(path: &Path, threshold: u64, prog: &Progress) -> io::Result<Loaded> {
    load_with(path, threshold, prog, None)
}

/// `load`, for a piece of an input file: data lines before the first keyword line belong to the
/// keyword line `kw_hint` (shown in the data viewer).
pub fn load_with(path: &Path, threshold: u64, prog: &Progress, kw_hint: Option<&str>) -> io::Result<Loaded> {
    let t0 = Instant::now();
    let src = Source::open(path)?;
    let bytes = src.bytes();
    prog.total.store(bytes.len() as u64, Ordering::Relaxed);
    let (bom, body_off) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { (true, 3) } else { (false, 0) };
    let body = &bytes[body_off..];

    let sc = scan(body, prog)?;
    // Keep the editable text small: lower the bulk threshold if needed.
    let th = choose_threshold(&sc, threshold);
    let (regs, text_bytes) = regions(&sc, th, body.len());

    let mut latin1 = false;
    for r in &regs {
        if let Region::Text(s, e) = r {
            if std::str::from_utf8(&body[*s..*e]).is_err() {
                latin1 = true;
                break;
            }
        }
    }
    let nbulk = regs.iter().filter(|r| matches!(r, Region::Bulk { .. })).count();
    let mut text = String::with_capacity(text_bytes + nbulk * 96);
    let mut bulks = Vec::with_capacity(nbulk);
    for r in &regs {
        match *r {
            Region::Text(s, e) => push_decoded(&mut text, &body[s..e], latin1),
            Region::Bulk { start, end, lines, comment: true, .. } => {
                let l = &body[start..memchr::memchr(b'\n', &body[start..end]).map(|i| start + i).unwrap_or(end)];
                let first: String = if latin1 { l.iter().map(|&c| c as char).collect() } else { String::from_utf8_lossy(l).into_owned() };
                let id = STORE.next_id();
                let b = STORE.insert(Bulk::new_comment(id, src.clone(), start + body_off, end + body_off, lines, first.trim_end().to_string()));
                text.push_str(&b.placeholder());
                bulks.push(id);
            }
            Region::Bulk { start, end, lines, kw, comment: false } => {
                let kw_line = kw
                    .map(|(s, e)| {
                        let l = &body[s..e];
                        let t: String = if latin1 {
                            l.iter().map(|&c| c as char).collect()
                        } else {
                            String::from_utf8_lossy(l).into_owned()
                        };
                        t.trim().to_string()
                    })
                    .or_else(|| kw_hint.map(|h| h.trim().to_string()))
                    .unwrap_or_else(|| "(data)".into());
                let kw_disp = kw_line.split(',').next().unwrap_or("").trim().to_string();
                let kw_norm = crate::util::norm(kw_disp.trim_start_matches('*'));
                let id = STORE.next_id();
                let b = STORE.insert(Bulk::new(id, src.clone(), start + body_off, end + body_off, lines, kw_disp, kw_norm, kw_line));
                text.push_str(&b.placeholder());
                bulks.push(id);
            }
        }
    }
    prog.done.store(bytes.len() as u64, Ordering::Relaxed);
    let eol = if sc.crlf > sc.lf { Eol::CrLf } else { Eol::Lf };
    Ok(Loaded {
        src,
        text,
        eol,
        mixed_eol: sc.crlf > 0 && sc.lf > 0,
        latin1,
        bom,
        bulks,
        total_lines: sc.total_lines,
        threshold: th,
        wanted: threshold,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn skeleton_and_bulk() {
        let dir = std::env::temp_dir().join(format!("inplite_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.inp");
        let mut f = std::fs::File::create(&p).unwrap();
        write!(f, "*Heading\r\ntest\r\n*Node, nset=ALL\r\n").unwrap();
        for i in 1..=10 {
            write!(f, "{i}, 0., 0., 0.\r\n").unwrap();
        }
        write!(f, "** comment\r\n*Element, type=T3D2\r\n1, 1, 2\r\n").unwrap();
        drop(f);
        let pr = Progress::new();
        let l = load(&p, 5, &pr).unwrap();
        assert_eq!(l.eol, Eol::CrLf);
        assert_eq!(l.bulks.len(), 1);
        assert!(l.text.contains("**@@BULK"));
        assert!(l.text.contains("** comment\n*Element"));
        assert_eq!(l.total_lines, 16);
        let b = STORE.get(l.bulks[0]).unwrap();
        assert_eq!(b.lines, 10);
        assert_eq!(b.line(0).unwrap(), b"1, 0., 0., 0.");
        assert_eq!(b.line(9).unwrap(), b"10, 0., 0., 0.");
        assert_eq!(b.kw_norm, "NODE");
    }
}
