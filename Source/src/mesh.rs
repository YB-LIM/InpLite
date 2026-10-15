//! Mesh index & checks (background): node/element ID lookup, counts, duplicate IDs,
//! elements referencing undefined nodes, element connectivity length.

use crate::load::Progress;
use crate::schema::element_nodes;
use crate::store::{placeholder_id, Bulk, SPARSE, STORE};
use crate::syntax::{line_kind, parse_keyword, Diag, LineKind, Sev};
use crate::util::{self, norm_label};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum IdKind {
    Node,
    Element,
}

#[derive(Clone, Debug)]
pub struct IdBlock {
    pub kind: IdKind,
    pub scope: String,
    pub etype: String,
    pub bulk: u64,
    pub count: u64,
    pub min: u64,
    pub max: u64,
    pub sorted: bool,
    /// (id, physical row) of every SPARSE-th record
    pub samples: Vec<(u64, u64)>,
}

#[derive(Clone, Debug)]
pub enum Hit {
    Text { line: usize },
    Bulk { id: u64, row: u64 },
}

pub struct MeshIndex {
    pub ver: u64,
    pub bulk_blocks: Vec<IdBlock>,
    pub nodes: Vec<(String, u64)>,
    pub elements: Vec<(String, String, u64)>,
    pub total_nodes: u64,
    pub total_elements: u64,
    pub diags: Vec<Diag>,
    pub dup_nodes: u64,
    pub dup_elements: u64,
    pub missing_refs: u64,
    pub bad_conn: u64,
    pub elapsed_ms: u64,
}

#[derive(Default)]
struct Bits(Vec<u64>);
impl Bits {
    /// set bit, return previous value
    fn set(&mut self, i: u64) -> bool {
        if i > (1 << 33) {
            return false;
        }
        let w = (i / 64) as usize;
        if w >= self.0.len() {
            self.0.resize((w + 1).max(self.0.len() * 3 / 2), 0);
        }
        let m = 1u64 << (i % 64);
        let old = self.0[w] & m != 0;
        self.0[w] |= m;
        old
    }
    fn get(&self, i: u64) -> bool {
        let w = (i / 64) as usize;
        w < self.0.len() && self.0[w] & (1u64 << (i % 64)) != 0
    }
}

const LIMIT: usize = 200;

enum Src<'a> {
    Text(Vec<(usize, &'a str)>),
    Bulk(Arc<Bulk>, usize),
}

struct ElemBlock<'a> {
    scope: String,
    src: Src<'a>,
}

/// Iterate logical records (continuation lines joined) of a data block.
/// Callback gets (fields, first physical row, skeleton line for text records).
fn for_records(src: &Src, prog: Option<&Progress>, mut f: impl FnMut(&[&[u8]], u64, usize)) {
    let mut fields: Vec<&[u8]> = Vec::with_capacity(32);
    let mut row0: u64 = 0;
    let mut line0: usize = 0;
    let mut pending = false;
    let mut handle = |line: &[u8], row: u64, sl: usize, fields: &mut Vec<&[u8]>, pending: &mut bool, row0: &mut u64, line0: &mut usize| {
        // skip comments / blank lines
        let t = line.iter().position(|c| *c != b' ' && *c != b'\t');
        let Some(t) = t else { return };
        if line[t] == b'*' {
            return;
        }
        if !*pending {
            fields.clear();
            *row0 = row;
            *line0 = sl;
        }
        let mut end = line.len();
        while end > 0 && matches!(line[end - 1], b' ' | b'\t' | b'\r') {
            end -= 1;
        }
        let cont = end > 0 && line[end - 1] == b',';
        let body = if cont { &line[..end - 1] } else { &line[..end] };
        // SAFETY of lifetimes: fields borrow from the source which outlives the callback
        let body: &[u8] = unsafe { std::slice::from_raw_parts(body.as_ptr(), body.len()) };
        for p in body.split(|c| *c == b',') {
            fields.push(p);
        }
        *pending = cont;
        if !cont {
            f(fields, *row0, *line0);
        }
    };
    match src {
        Src::Text(lines) => {
            for (i, (sl, l)) in lines.iter().enumerate() {
                handle(l.as_bytes(), i as u64, *sl, &mut fields, &mut pending, &mut row0, &mut line0);
            }
        }
        Src::Bulk(b, sl) => {
            let bytes = b.bytes();
            let mut pos = 0usize;
            let mut row: u64 = 0;
            let mut last = 0usize;
            while pos <= bytes.len() {
                let nl = memchr::memchr(b'\n', &bytes[pos..]).map(|i| pos + i).unwrap_or(bytes.len());
                handle(&bytes[pos..nl], row, *sl, &mut fields, &mut pending, &mut row0, &mut line0);
                row += 1;
                pos = nl + 1;
                if let Some(p) = prog {
                    if pos - last > (16 << 20) {
                        p.done.fetch_add((pos - last) as u64, Ordering::Relaxed);
                        last = pos;
                    }
                }
            }
            if let Some(p) = prog {
                p.done.fetch_add((bytes.len().saturating_sub(last)) as u64, Ordering::Relaxed);
            }
        }
    }
    if pending {
        f(&fields, row0, line0);
    }
}

fn field_u64(f: &[u8]) -> Option<u64> {
    util::parse_u64(f)
}

pub fn build(text: &str, ver: u64, prog: &Progress) -> MeshIndex {
    let t0 = std::time::Instant::now();
    // ---- collect blocks
    struct Blk<'a> {
        kind: IdKind,
        scope: String,
        etype: String,
        src: Src<'a>,
    }
    let mut blks: Vec<Blk> = Vec::new();
    let mut part: Option<String> = None;
    let mut inst: Option<String> = None;
    let mut cur: Option<Blk> = None;
    let mut kw_cont = false;
    let mut total_bulk_bytes = 0u64;
    let mut incomplete: std::collections::HashSet<String> = std::collections::HashSet::new();
    let starts = util::line_starts(text);
    let _ = &starts;
    for (li, s, e) in util::lines(text) {
        let line = &text[s..e];
        let k = line_kind(line);
        if kw_cont {
            if k == LineKind::Data {
                kw_cont = line.trim_end().ends_with(',');
                continue;
            }
            kw_cont = false;
        }
        match k {
            LineKind::Keyword => {
                if let Some(b) = cur.take() {
                    blks.push(b);
                }
                let kp = parse_keyword(line).unwrap();
                kw_cont = kp.cont;
                let get = |n: &str| kp.params.iter().find(|p| p.norm == n).and_then(|p| p.value_str(line)).map(|s| s.to_string());
                match kp.norm.as_str() {
                    "PART" => part = get("NAME").map(|s| norm_label(&s)),
                    "ENDPART" => part = None,
                    "INSTANCE" => inst = get("NAME").map(|s| norm_label(&s)),
                    "ENDINSTANCE" => inst = None,
                    // nodes may come from an include file or an INPUT= file: don't report
                    // "undefined node" for that scope
                    "INCLUDE" | "NODE" | "NGEN" | "NFILL" | "NCOPY" if kp.norm == "INCLUDE" || get("INPUT").is_some() || kp.norm != "NODE" => {
                        incomplete.insert(match (&inst, &part) {
                            (Some(i), _) => format!("Instance {i}"),
                            (None, Some(p)) => format!("Part {p}"),
                            _ => "Model".to_string(),
                        });
                    }
                    "NODE" | "ELEMENT" if get("INPUT").is_none() => {
                        let scope = match (&inst, &part) {
                            (Some(i), _) => format!("Instance {i}"),
                            (None, Some(p)) => format!("Part {p}"),
                            _ => "Model".to_string(),
                        };
                        cur = Some(Blk {
                            kind: if kp.norm == "NODE" { IdKind::Node } else { IdKind::Element },
                            scope,
                            etype: get("TYPE").map(|s| s.to_ascii_uppercase()).unwrap_or_default(),
                            src: Src::Text(Vec::new()),
                        });
                    }
                    _ => {}
                }
            }
            LineKind::Data => {
                if let Some(b) = cur.as_mut() {
                    if let Src::Text(v) = &mut b.src {
                        v.push((li, line));
                    }
                }
            }
            LineKind::Placeholder => {
                if let Some(b) = cur.as_mut() {
                    if let Some((id, _)) = placeholder_id(line) {
                        if let Some(bk) = STORE.get(id) {
                            total_bulk_bytes += bk.size();
                            let kind = b.kind;
                            let scope = b.scope.clone();
                            let etype = b.etype.clone();
                            let old = std::mem::replace(&mut b.src, Src::Bulk(bk, li));
                            if let Src::Text(v) = old {
                                if !v.is_empty() {
                                    blks.push(Blk { kind, scope: scope.clone(), etype: etype.clone(), src: Src::Text(v) });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(b) = cur.take() {
        blks.push(b);
    }
    let elem_bulk_bytes: u64 = blks.iter().filter(|b| b.kind == IdKind::Element).map(|b| if let Src::Bulk(bk, _) = &b.src { bk.size() } else { 0 }).sum();
    prog.total.store(total_bulk_bytes + elem_bulk_bytes, Ordering::Relaxed);
    prog.done.store(0, Ordering::Relaxed);

    // ---- pass 1
    let mut node_bits: HashMap<String, Bits> = HashMap::new();
    let mut elem_bits: HashMap<String, Bits> = HashMap::new();
    let mut nodes: HashMap<String, u64> = HashMap::new();
    let mut elems: HashMap<(String, String), u64> = HashMap::new();
    let mut diags = Vec::new();
    let (mut dup_n, mut dup_e, mut bad_conn, mut missing) = (0u64, 0u64, 0u64, 0u64);
    let mut bulk_blocks = Vec::new();
    let mut elem_blocks: Vec<ElemBlock> = Vec::new();

    for b in blks {
        let is_node = b.kind == IdKind::Node;
        let mut ib = IdBlock {
            kind: b.kind,
            scope: b.scope.clone(),
            etype: b.etype.clone(),
            bulk: 0,
            count: 0,
            min: u64::MAX,
            max: 0,
            sorted: true,
            samples: Vec::new(),
        };
        let mut prev = 0u64;
        let expected = if is_node { None } else { element_nodes(&b.etype) };
        let bits = if is_node { node_bits.entry(b.scope.clone()).or_default() } else { elem_bits.entry(b.scope.clone()).or_default() };
        let bulk_id = if let Src::Bulk(bk, _) = &b.src { Some(bk.id) } else { None };
        let (bulk_sl, bulk_arc) = if let Src::Bulk(bk, sl) = &b.src { (*sl, Some(bk.clone())) } else { (0, None) };
        let mut sparse_rows: Vec<u64> = Vec::new();
        for_records(&b.src, Some(prog), |fields, row, sl| {
            let Some(id) = fields.first().and_then(|f| field_u64(f)) else { return };
            if ib.count % SPARSE == 0 {
                ib.samples.push((id, row));
            }
            if ib.count > 0 && id <= prev {
                ib.sorted = false;
            }
            prev = id;
            ib.count += 1;
            ib.min = ib.min.min(id);
            ib.max = ib.max.max(id);
            if bits.set(id) {
                let what = if is_node { "node" } else { "element" };
                if is_node {
                    dup_n += 1;
                } else {
                    dup_e += 1;
                }
                if diags.len() < LIMIT {
                    let mut d = Diag::new(Sev::Error, format!("Duplicate {what} label {id} ({})", b.scope), sl, 0, 0);
                    if let Some(bid) = bulk_id {
                        d.bulk = Some((bid, row));
                        d.line = bulk_sl;
                    }
                    diags.push(d);
                }
            }
            if let Some(exp) = expected {
                let n = fields[1..].iter().filter(|f| !f.iter().all(|c| *c == b' ' || *c == b'\t')).count() as u32;
                if n != exp {
                    bad_conn += 1;
                    if diags.len() < LIMIT {
                        let sev = if n > exp { Sev::Error } else { Sev::Warning };
                        let mut d = Diag::new(sev, format!("Element {id} ({}) has {n} nodes, expected {exp}", b.etype), sl, 0, 0);
                        if let Some(bid) = bulk_id {
                            d.bulk = Some((bid, row));
                            d.line = bulk_sl;
                        }
                        diags.push(d);
                    }
                }
            }
            if bulk_id.is_some() {
                sparse_rows.push(row);
            }
        });
        let _ = sparse_rows;
        if is_node {
            *nodes.entry(b.scope.clone()).or_default() += ib.count;
        } else {
            *elems.entry((b.scope.clone(), b.etype.clone())).or_default() += ib.count;
        }
        if let Some(bk) = bulk_arc {
            ib.bulk = bk.id;
            bulk_blocks.push(ib);
            if !bk.sparse_ready() {
                let _ = bk.sparse.set(Arc::new(crate::store::build_sparse(bk.bytes())));
            }
        }
        if !is_node {
            elem_blocks.push(ElemBlock { scope: b.scope, src: b.src });
        }
    }

    // ---- pass 2: element -> node references
    for eb in &elem_blocks {
        if incomplete.contains(&eb.scope) {
            continue;
        }
        let Some(bits) = node_bits.get(&eb.scope) else {
            continue;
        };
        if bits.0.is_empty() {
            continue;
        }
        let bulk_id = if let Src::Bulk(bk, _) = &eb.src { Some(bk.id) } else { None };
        let bulk_sl = if let Src::Bulk(_, sl) = &eb.src { *sl } else { 0 };
        for_records(&eb.src, Some(prog), |fields, row, sl| {
            let Some(id) = fields.first().and_then(|f| field_u64(f)) else { return };
            for f in &fields[1..] {
                if let Some(n) = field_u64(f) {
                    if n > 0 && !bits.get(n) {
                        missing += 1;
                        if diags.len() < LIMIT * 2 {
                            let mut d = Diag::new(Sev::Error, format!("Element {id} references undefined node {n} ({})", eb.scope), sl, 0, 0);
                            if let Some(bid) = bulk_id {
                                d.bulk = Some((bid, row));
                                d.line = bulk_sl;
                            }
                            diags.push(d);
                        }
                    }
                }
            }
        });
    }
    let mut nodes: Vec<(String, u64)> = nodes.into_iter().collect();
    nodes.sort();
    let mut elements: Vec<(String, String, u64)> = elems.into_iter().map(|((s, t), c)| (s, t, c)).collect();
    elements.sort();
    MeshIndex {
        ver,
        total_nodes: nodes.iter().map(|n| n.1).sum(),
        total_elements: elements.iter().map(|e| e.2).sum(),
        nodes,
        elements,
        bulk_blocks,
        diags,
        dup_nodes: dup_n,
        dup_elements: dup_e,
        missing_refs: missing,
        bad_conn,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    }
}

impl MeshIndex {
    /// Find a node/element label: bulk blocks via the index, text blocks by scanning the current text.
    pub fn find(&self, kind: IdKind, id: u64, text: &str) -> Vec<(String, Hit)> {
        let mut out = Vec::new();
        for ib in &self.bulk_blocks {
            if ib.kind != kind || id < ib.min || id > ib.max {
                continue;
            }
            let Some(bk) = STORE.get(ib.bulk) else { continue };
            let _ = bk.sparse_blocking();
            let start_row = if ib.sorted {
                let k = match ib.samples.binary_search_by_key(&id, |s| s.0) {
                    Ok(k) => k,
                    Err(0) => 0,
                    Err(k) => k - 1,
                };
                ib.samples[k].1
            } else {
                0
            };
            let mut row = start_row;
            let mut scanned = 0u64;
            while row < bk.lines {
                let Some(l) = bk.line(row) else { break };
                let first = l.split(|c| *c == b',').next().unwrap_or(&[]);
                if let Some(v) = util::parse_u64(first) {
                    if v == id {
                        out.push((ib.scope.clone(), Hit::Bulk { id: ib.bulk, row }));
                        break;
                    }
                    if ib.sorted && v > id {
                        break;
                    }
                }
                row += 1;
                scanned += 1;
                if ib.sorted && scanned > SPARSE * 4 {
                    break;
                }
            }
        }
        // text blocks
        let want = if kind == IdKind::Node { "NODE" } else { "ELEMENT" };
        let mut active = false;
        let mut part: Option<String> = None;
        let mut inst: Option<String> = None;
        let mut prev_cont = false;
        for (li, s, e) in util::lines(text) {
            let line = &text[s..e];
            match line_kind(line) {
                LineKind::Keyword => {
                    let kp = parse_keyword(line).unwrap();
                    let get = |n: &str| kp.params.iter().find(|p| p.norm == n).and_then(|p| p.value_str(line)).map(|s| norm_label(s));
                    match kp.norm.as_str() {
                        "PART" => part = get("NAME"),
                        "ENDPART" => part = None,
                        "INSTANCE" => inst = get("NAME"),
                        "ENDINSTANCE" => inst = None,
                        _ => {}
                    }
                    active = kp.norm == want;
                    prev_cont = kp.cont;
                }
                LineKind::Data if active => {
                    if prev_cont {
                        prev_cont = line.trim_end().ends_with(',');
                        continue;
                    }
                    let first = line.split(',').next().unwrap_or("");
                    if util::parse_u64(first.as_bytes()) == Some(id) {
                        let scope = match (&inst, &part) {
                            (Some(i), _) => format!("Instance {i}"),
                            (None, Some(p)) => format!("Part {p}"),
                            _ => "Model".to_string(),
                        };
                        out.push((scope, Hit::Text { line: li }));
                    }
                    prev_cont = line.trim_end().ends_with(',');
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mesh_checks() {
        let t = "*Part, name=P\n*Node\n1, 0., 0., 0.\n2, 1., 0., 0.\n2, 1., 1., 0.\n*Element, type=T3D2\n1, 1, 2\n2, 1, 9\n3, 1\n*End Part\n";
        let p = Progress::new();
        let m = build(t, 1, &p);
        assert_eq!(m.total_nodes, 3);
        assert_eq!(m.total_elements, 3);
        assert_eq!(m.dup_nodes, 1);
        assert_eq!(m.missing_refs, 1);
        assert_eq!(m.bad_conn, 1);
        let h = m.find(IdKind::Node, 2, t);
        assert_eq!(h.len(), 2);
    }
}
