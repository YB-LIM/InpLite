//! Comment out / uncomment lines with the Abaqus comment marker `**`, including data blocks that
//! are kept on disk (they are rewritten into a temporary file and read back).

use crate::load::{load_with, Progress};
use crate::store::{placeholder_id, Bulk, STORE};
use crate::syntax::{line_kind, LineKind};
use crate::util;
use std::io::{self, BufWriter, Write};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Comment,
    Uncomment,
    /// uncomment if every non-blank line is a comment, otherwise comment
    Toggle,
}

/// `**` in front of the line (blank lines stay blank).
pub fn comment_line(l: &str) -> String {
    if l.trim().is_empty() { l.to_string() } else { format!("**{l}") }
}

/// The line without its leading `**` (and the blank of `** *Keyword`); None if it is no comment.
pub fn uncomment_line(l: &str) -> Option<String> {
    let r = l.strip_prefix("**")?;
    if r.starts_with(" *") && !r.starts_with(" **") {
        return Some(r[1..].to_string());
    }
    Some(r.to_string())
}

fn uncomment_bytes(l: &[u8]) -> &[u8] {
    match l.strip_prefix(b"**") {
        Some(r) if r.starts_with(b" *") && !r.starts_with(b" **") => &r[1..],
        Some(r) => r,
        None => l,
    }
}

/// One piece of the new text of the commented lines.
pub enum Piece {
    Text(String),
    /// a data block on disk, to be rewritten (`comment`: add `**`, otherwise remove it);
    /// `kw`: the keyword line its data lines belong to
    Bulk { id: u64, comment: bool, kw: Option<String> },
}

pub struct Plan {
    /// byte range of whole lines (without the line break of the last line)
    pub start: usize,
    pub end: usize,
    pub pieces: Vec<Piece>,
    /// resolved mode (Comment or Uncomment)
    pub mode: Mode,
    pub lines: usize,
    /// bytes of data blocks to rewrite
    pub bulk_bytes: u64,
}

impl Plan {
    pub fn has_bulks(&self) -> bool {
        self.pieces.iter().any(|p| matches!(p, Piece::Bulk { .. }))
    }
    /// The new text, with `bulk_text` for the rewritten data blocks (in order).
    pub fn assemble(&self, bulk_text: &[String]) -> String {
        let mut out = String::new();
        let mut k = 0;
        for (i, p) in self.pieces.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match p {
                Piece::Text(t) => out.push_str(t),
                Piece::Bulk { .. } => {
                    out.push_str(bulk_text.get(k).map(|s| s.as_str()).unwrap_or(""));
                    k += 1;
                }
            }
        }
        out
    }
}

fn line_at(text: &str, starts: &[usize], l: usize) -> (usize, usize) {
    let s = starts[l];
    let e = starts.get(l + 1).map(|x| x - 1).unwrap_or(text.len());
    (s, e.max(s))
}

/// Plan commenting / uncommenting lines `l0..=l1`.
pub fn plan(text: &str, starts: &[usize], l0: usize, l1: usize, mode: Mode) -> Plan {
    let l1 = l1.min(starts.len().saturating_sub(1));
    let l0 = l0.min(l1);
    let mode = match mode {
        Mode::Toggle => {
            let mut all = true;
            let mut any = false;
            for l in l0..=l1 {
                let (s, e) = line_at(text, starts, l);
                let t = &text[s..e];
                if t.trim().is_empty() {
                    continue;
                }
                any = true;
                let c = match placeholder_id(t) {
                    Some((id, _)) => STORE.get(id).is_some_and(|b| b.comment),
                    None => t.starts_with("**"),
                };
                if !c {
                    all = false;
                    break;
                }
            }
            if any && all { Mode::Uncomment } else { Mode::Comment }
        }
        m => m,
    };
    // keyword line governing the first line (for data blocks that become data again)
    let mut last_kw: Option<String> = None;
    let mut k = l0;
    let mut guard = 0;
    while k > 0 && guard < 200_000 {
        k -= 1;
        guard += 1;
        let (s, e) = line_at(text, starts, k);
        if line_kind(&text[s..e]) == LineKind::Keyword {
            last_kw = Some(text[s..e].to_string());
            break;
        }
    }
    let mut pieces = Vec::new();
    let mut bulk_bytes = 0;
    for l in l0..=l1 {
        let (s, e) = line_at(text, starts, l);
        let t = &text[s..e];
        if let Some((id, _)) = placeholder_id(t) {
            let b = STORE.get(id);
            let is_comment = b.as_ref().is_some_and(|b| b.comment);
            let rewrite = match mode {
                Mode::Comment => b.is_some(),
                _ => is_comment,
            };
            if rewrite {
                bulk_bytes += b.map(|b| b.size()).unwrap_or(0);
                pieces.push(Piece::Bulk { id, comment: mode == Mode::Comment, kw: last_kw.clone() });
            } else {
                pieces.push(Piece::Text(t.to_string()));
            }
            continue;
        }
        let new = match mode {
            Mode::Comment => comment_line(t),
            _ => uncomment_line(t).unwrap_or_else(|| t.to_string()),
        };
        if line_kind(&new) == LineKind::Keyword {
            last_kw = Some(new.clone());
        }
        pieces.push(Piece::Text(new));
    }
    Plan { start: starts[l0], end: line_at(text, starts, l1).1, pieces, mode, lines: l1 - l0 + 1, bulk_bytes }
}

/// Rewrite a data block with `**` added (`comment`) or removed, into a temporary file, and read it
/// back like an input file: returns the text that replaces its placeholder line and the new
/// data blocks. `th`: data-block threshold.
pub fn rewrite_bulk(b: &Bulk, comment: bool, th: u64, kw: Option<&str>, prog: Option<&Progress>) -> io::Result<(String, Vec<u64>)> {
    let path = util::temp_file("lines");
    {
        let f = std::fs::File::create(&path)?;
        let mut w = BufWriter::with_capacity(4 << 20, f);
        let bytes = b.bytes();
        let mut pos = 0;
        let mut done_report = 0usize;
        while pos < bytes.len() {
            let nl = memchr::memchr(b'\n', &bytes[pos..]).map(|i| pos + i);
            let le = nl.unwrap_or(bytes.len());
            let mut ce = le;
            if ce > pos && bytes[ce - 1] == b'\r' {
                ce -= 1;
            }
            let line = &bytes[pos..ce];
            if comment {
                if !line.iter().all(|c| c.is_ascii_whitespace()) {
                    w.write_all(b"**")?;
                }
                w.write_all(line)?;
            } else {
                w.write_all(uncomment_bytes(line))?;
            }
            w.write_all(&bytes[ce..le])?; // a CR before the LF
            if let Some(n) = nl {
                w.write_all(b"\n")?;
                pos = n + 1;
            } else {
                pos = bytes.len();
            }
            if let Some(p) = prog {
                if pos - done_report > (4 << 20) {
                    p.done.fetch_add((pos - done_report) as u64, std::sync::atomic::Ordering::Relaxed);
                    done_report = pos;
                }
            }
        }
        if let Some(p) = prog {
            p.done.fetch_add((pos - done_report) as u64, std::sync::atomic::Ordering::Relaxed);
        }
        w.flush()?;
    }
    let l = load_with(&path, th, &Progress::new(), kw)?;
    Ok((l.text, l.bulks))
}

/// Rewrite all data blocks of a plan (background thread).
pub fn rewrite_all(plan_bulks: &[(u64, bool, Option<String>)], th: u64, prog: &Arc<Progress>) -> Vec<io::Result<(String, Vec<u64>)>> {
    plan_bulks
        .iter()
        .map(|(id, comment, kw)| match STORE.get(*id) {
            Some(b) => rewrite_bulk(&b, *comment, th, kw.as_deref(), Some(prog)),
            None => Err(io::Error::other(format!("data block #{id} not found"))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines() {
        assert_eq!(comment_line("*Node"), "***Node");
        assert_eq!(comment_line("   "), "   ");
        assert_eq!(uncomment_line("***Node").as_deref(), Some("*Node"));
        assert_eq!(uncomment_line("** *Node").as_deref(), Some("*Node"));
        assert_eq!(uncomment_line("**** text").as_deref(), Some("** text"));
        assert_eq!(uncomment_line("*Node"), None);
        let t = "*Node\n1, 0., 0.\n\n** note\n*Element\n";
        let st = util::line_starts(t);
        let p = plan(t, &st, 0, 4, Mode::Toggle);
        assert_eq!(p.mode, Mode::Comment);
        let new = p.assemble(&[]);
        assert_eq!(new, "***Node\n**1, 0., 0.\n\n**** note\n***Element");
        let mut t2 = t.to_string();
        t2.replace_range(p.start..p.end, &new);
        let st2 = util::line_starts(&t2);
        let p2 = plan(&t2, &st2, 0, 4, Mode::Toggle);
        assert_eq!(p2.mode, Mode::Uncomment);
        let mut t3 = t2.clone();
        t3.replace_range(p2.start..p2.end, &p2.assemble(&[]));
        assert_eq!(t3, t);
    }
}
