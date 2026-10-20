//! Keyword-aware diff: compares two input files block by block (keyword line + its data),
//! ignoring blanks, letter case and comments.

use crate::analysis::{Analysis, Block};
use crate::store::{placeholder_id, STORE};
use crate::syntax::{line_kind, LineKind};
use crate::util::{self, norm, Fnv};
use similar::{capture_diff_slices, Algorithm, DiffOp};
use std::collections::{HashMap, VecDeque};

pub struct DBlock {
    pub label: String,
    pub ctx: String,
    pub key: String,
    pub sig: u64,
    pub kw_sig: u64,
    pub data_sig: u64,
    pub line: usize,
    pub data_lines: u64,
    pub params: Vec<(String, String)>,
    /// original data lines (only for small text blocks)
    pub data: Option<Vec<String>>,
    pub has_bulk: bool,
    pub bulks: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Same,
    Modified,
    Added,
    Removed,
}

pub struct DiffRow {
    pub kind: RowKind,
    pub a: Option<usize>,
    pub b: Option<usize>,
    /// for Same rows: number of identical blocks in the run
    pub run: usize,
    pub changes: Vec<String>,
    pub lines: Vec<(char, String)>,
    pub note: Option<String>,
}

pub struct DiffResult {
    pub a_name: String,
    pub b_name: String,
    pub a: Vec<DBlock>,
    pub b: Vec<DBlock>,
    pub rows: Vec<DiffRow>,
    pub same: usize,
    pub modified: usize,
    pub added: usize,
    pub removed: usize,
    pub elapsed_ms: u64,
}

fn norm_data_line(l: &[u8], h: &mut Fnv) {
    for &c in l {
        if c != b' ' && c != b'\t' && c != b'\r' {
            h.byte(c.to_ascii_uppercase());
        }
    }
    h.byte(b'\n');
}

fn norm_value(v: &str) -> String {
    let t = v.trim();
    if t.starts_with('"') { t.to_ascii_uppercase() } else { norm(t) }
}

pub fn blocks(text: &str, an: &Analysis) -> Vec<DBlock> {
    let mut out = Vec::with_capacity(an.blocks.len());
    let step_of = |b: &Block| -> String {
        b.ctx.step.and_then(|i| an.summary.steps.get(i)).map(|s| format!("Step {}", s.name)).unwrap_or_default()
    };
    for b in &an.blocks {
        let mut params: Vec<(String, String)> =
            b.params.iter().map(|(n, _, v)| (n.clone(), v.as_deref().map(norm_value).unwrap_or_default())).collect();
        params.sort();
        let mut kh = Fnv::default();
        kh.bytes(b.norm.as_bytes());
        for (n, v) in &params {
            kh.bytes(b",");
            kh.bytes(n.as_bytes());
            kh.bytes(b"=");
            kh.bytes(v.as_bytes());
        }
        let mut dh = Fnv::default();
        let seg = &text[b.start..b.end.min(text.len())];
        let mut data_lines_txt: Vec<String> = Vec::new();
        let mut has_bulk = false;
        let mut bulk_ids = Vec::new();
        let mut in_kw = true;
        for (i, s, e) in util::lines(seg) {
            let l = &seg[s..e];
            if i == 0 {
                in_kw = l.trim_end().ends_with(',');
                continue;
            }
            let k = line_kind(l);
            if in_kw && k == LineKind::Data {
                in_kw = l.trim_end().ends_with(',');
                continue;
            }
            in_kw = false;
            match k {
                LineKind::Data => {
                    norm_data_line(l.as_bytes(), &mut dh);
                    if data_lines_txt.len() < 20000 {
                        data_lines_txt.push(l.trim_end().to_string());
                    }
                }
                LineKind::Placeholder => {
                    has_bulk = true;
                    if let Some((id, _)) = placeholder_id(l) {
                        bulk_ids.push(id);
                        if let Some(bk) = STORE.get(id) {
                            for line in bk.bytes().split(|c| *c == b'\n') {
                                norm_data_line(line, &mut dh);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let ident = ["NAME", "ELSET", "NSET", "TYPE", "INPUT", "INTERACTION"]
            .iter()
            .find_map(|k| params.iter().find(|p| p.0 == *k).map(|p| format!("{k}={}", p.1)))
            .unwrap_or_default();
        let ctx = [b.ctx.label(), step_of(b)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ");
        let key = format!("{}|{}|{}", b.norm, ident, ctx);
        let mut sh = Fnv(kh.0);
        sh.bytes(&dh.0.to_le_bytes());
        out.push(DBlock {
            label: b.label(),
            ctx,
            key,
            sig: sh.0,
            kw_sig: kh.0,
            data_sig: dh.0,
            line: b.line,
            data_lines: b.data_lines,
            params,
            data: if has_bulk || data_lines_txt.len() >= 20000 { None } else { Some(data_lines_txt) },
            has_bulk,
            bulks: bulk_ids,
        });
    }
    out
}

pub fn compute(a_name: String, a: Vec<DBlock>, b_name: String, b: Vec<DBlock>) -> DiffResult {
    let t0 = std::time::Instant::now();
    let sa: Vec<u64> = a.iter().map(|x| x.sig).collect();
    let sb: Vec<u64> = b.iter().map(|x| x.sig).collect();
    let ops = capture_diff_slices(Algorithm::Myers, &sa, &sb);
    let mut rows = Vec::new();
    let (mut same, mut modified, mut added, mut removed) = (0, 0, 0, 0);
    for op in ops {
        match op {
            DiffOp::Equal { old_index, new_index, len } => {
                same += len;
                rows.push(DiffRow { kind: RowKind::Same, a: Some(old_index), b: Some(new_index), run: len, changes: vec![], lines: vec![], note: None });
            }
            DiffOp::Delete { old_index, old_len, new_index } => pair(&a, &b, old_index..old_index + old_len, new_index..new_index, &mut rows),
            DiffOp::Insert { old_index, new_index, new_len } => pair(&a, &b, old_index..old_index, new_index..new_index + new_len, &mut rows),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                pair(&a, &b, old_index..old_index + old_len, new_index..new_index + new_len, &mut rows)
            }
        }
    }
    for r in &rows {
        match r.kind {
            RowKind::Modified => modified += 1,
            RowKind::Added => added += 1,
            RowKind::Removed => removed += 1,
            RowKind::Same => {}
        }
    }
    DiffResult { a_name, b_name, a, b, rows, same, modified, added, removed, elapsed_ms: t0.elapsed().as_millis() as u64 }
}

fn pair(a: &[DBlock], b: &[DBlock], ra: std::ops::Range<usize>, rb: std::ops::Range<usize>, rows: &mut Vec<DiffRow>) {
    let mut by_key: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for j in rb.clone() {
        by_key.entry(b[j].key.as_str()).or_default().push_back(j);
    }
    let mut used = vec![false; rb.len()];
    for i in ra {
        if let Some(j) = by_key.get_mut(a[i].key.as_str()).and_then(|q| q.pop_front()) {
            used[j - rb.start] = true;
            rows.push(detail(a, b, i, j));
        } else {
            rows.push(DiffRow { kind: RowKind::Removed, a: Some(i), b: None, run: 1, changes: vec![], lines: vec![], note: None });
        }
    }
    for (k, u) in used.iter().enumerate() {
        if !u {
            rows.push(DiffRow { kind: RowKind::Added, a: None, b: Some(rb.start + k), run: 1, changes: vec![], lines: vec![], note: None });
        }
    }
}

/// Line-by-line comparison of two large data blocks with the same number of lines.
/// Returns (number of differing lines, number shown).
fn bulk_line_diff(x: &DBlock, y: &DBlock, out: &mut Vec<(char, String)>) -> (Option<u64>, u64) {
    if x.bulks.len() != 1 || y.bulks.len() != 1 || x.data_lines != y.data_lines {
        return (None, 0);
    }
    let (Some(ba), Some(bb)) = (STORE.get(x.bulks[0]), STORE.get(y.bulks[0])) else { return (None, 0) };
    let same = |p: &[u8], q: &[u8]| {
        let f = |v: &[u8]| v.iter().filter(|c| !matches!(c, b' ' | b'\t' | b'\r')).map(|c| c.to_ascii_uppercase()).collect::<Vec<u8>>();
        p == q || f(p) == f(q)
    };
    let mut total = 0u64;
    let mut shown = 0u64;
    for (pa, pb) in ba.bytes().split(|c| *c == b'\n').zip(bb.bytes().split(|c| *c == b'\n')) {
        if !same(pa, pb) {
            total += 1;
            if shown < 20 {
                out.push(('-', String::from_utf8_lossy(pa).trim_end().to_string()));
                out.push(('+', String::from_utf8_lossy(pb).trim_end().to_string()));
                shown += 1;
            }
        }
    }
    (Some(total), shown)
}

fn detail(a: &[DBlock], b: &[DBlock], i: usize, j: usize) -> DiffRow {
    let (x, y) = (&a[i], &b[j]);
    let mut changes = Vec::new();
    if x.kw_sig != y.kw_sig {
        let ma: HashMap<&str, &str> = x.params.iter().map(|p| (p.0.as_str(), p.1.as_str())).collect();
        let mb: HashMap<&str, &str> = y.params.iter().map(|p| (p.0.as_str(), p.1.as_str())).collect();
        for (k, va) in &ma {
            match mb.get(k) {
                None => changes.push(format!("- {}{}", k, if va.is_empty() { String::new() } else { format!("={va}") })),
                Some(vb) if vb != va => changes.push(format!("{k}: {va} -> {vb}")),
                _ => {}
            }
        }
        for (k, vb) in &mb {
            if !ma.contains_key(k) {
                changes.push(format!("+ {}{}", k, if vb.is_empty() { String::new() } else { format!("={vb}") }));
            }
        }
        changes.sort();
    }
    let mut lines = Vec::new();
    let mut note = None;
    if x.data_sig != y.data_sig {
        match (&x.data, &y.data) {
            (Some(da), Some(db)) => {
                let na: Vec<String> = da.iter().map(|l| norm(l)).collect();
                let nb: Vec<String> = db.iter().map(|l| norm(l)).collect();
                let ops = capture_diff_slices(Algorithm::Myers, &na, &nb);
                for op in ops {
                    match op {
                        DiffOp::Equal { .. } => {}
                        DiffOp::Delete { old_index, old_len, .. } => {
                            for k in old_index..old_index + old_len {
                                lines.push(('-', da[k].clone()));
                            }
                        }
                        DiffOp::Insert { new_index, new_len, .. } => {
                            for k in new_index..new_index + new_len {
                                lines.push(('+', db[k].clone()));
                            }
                        }
                        DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                            for k in old_index..old_index + old_len {
                                lines.push(('-', da[k].clone()));
                            }
                            for k in new_index..new_index + new_len {
                                lines.push(('+', db[k].clone()));
                            }
                        }
                    }
                    if lines.len() > 400 {
                        lines.truncate(400);
                        note = Some("(only the first 400 changed lines are shown)".into());
                        break;
                    }
                }
            }
            _ => {
                let (bl, n) = bulk_line_diff(x, y, &mut lines);
                note = Some(match bl {
                    Some(total) => format!(
                        "{} of {} data lines differ{}",
                        util::fmt_int(total),
                        util::fmt_int(x.data_lines),
                        if n < total { format!(" (first {n} shown)") } else { String::new() }
                    ),
                    None => format!("Data differs ({} vs {} lines)", util::fmt_int(x.data_lines), util::fmt_int(y.data_lines)),
                });
            }
        }
    }
    DiffRow { kind: RowKind::Modified, a: Some(i), b: Some(j), run: 1, changes, lines, note }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyze;
    use std::path::Path;
    #[test]
    fn diff_basic() {
        let a = "*Material, name=STEEL\n*Elastic\n210000., 0.3\n*Step, name=S1\n*Static\n*Boundary\nFIX, 1, 3\n*End Step\n";
        let b = "*material, NAME=steel\n*Elastic\n200000., 0.3\n*Step, name=S1\n*Static\n*Boundary\nFIX, 1, 3\n*Cload\nLOAD, 2, -100.\n*End Step\n";
        let aa = analyze(a, 1, Path::new("."));
        let ab = analyze(b, 1, Path::new("."));
        let r = compute("a".into(), blocks(a, &aa), "b".into(), blocks(b, &ab));
        assert_eq!(r.modified, 1, "elastic changed");
        assert_eq!(r.added, 1, "cload added");
        assert_eq!(r.removed, 0);
        let m = r.rows.iter().find(|x| x.kind == RowKind::Modified).unwrap();
        assert_eq!(m.lines.len(), 2);
    }
}
