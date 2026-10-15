//! Document-level analysis (runs in a background thread after edits):
//! block structure, outline tree, definitions/references, structural checks, model summary.

use crate::schema::{KeywordDef, SCHEMA};
use crate::store::{placeholder_id, STORE};
use crate::syntax::{line_kind, parse_keyword, parse_params, Diag, LineKind, Sev};
use crate::util::{self, norm, norm_label};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DefKind {
    Nset,
    Elset,
    Surface,
    Material,
    Amplitude,
    Orientation,
    Interaction,
    Part,
    Instance,
    SectionControls,
    Step,
    Param,
}

impl DefKind {
    pub fn label(self) -> &'static str {
        match self {
            DefKind::Nset => "node set",
            DefKind::Elset => "element set",
            DefKind::Surface => "surface",
            DefKind::Material => "material",
            DefKind::Amplitude => "amplitude",
            DefKind::Orientation => "orientation",
            DefKind::Interaction => "surface interaction",
            DefKind::Part => "part",
            DefKind::Instance => "instance",
            DefKind::SectionControls => "section controls",
            DefKind::Step => "step",
            DefKind::Param => "parameter",
        }
    }
    fn global(self) -> bool {
        matches!(
            self,
            DefKind::Material
                | DefKind::Amplitude
                | DefKind::Interaction
                | DefKind::Part
                | DefKind::Instance
                | DefKind::SectionControls
                | DefKind::Step
                | DefKind::Param
        )
    }
}

#[derive(Clone, Debug)]
pub struct Def {
    pub kind: DefKind,
    pub scope: String,
    pub name: String,
    pub disp: String,
    pub line: usize,
    pub start: usize,
    pub end: usize,
    /// definition located in an include file: (path, 1-based line in that file)
    pub file: Option<(PathBuf, u64)>,
}

#[derive(Clone, Debug)]
pub struct Ref {
    pub kind: DefKind,
    /// alternative kind (e.g. node number or node set)
    pub ctx: Ctx,
    pub name: String,
    pub disp: String,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ctx {
    pub part: Option<String>,
    pub instance: Option<(String, String)>,
    pub assembly: bool,
    pub step: Option<usize>,
}

impl Ctx {
    fn scope_tag(&self) -> String {
        if let Some((i, _)) = &self.instance {
            format!("I:{i}")
        } else if let Some(p) = &self.part {
            format!("P:{p}")
        } else {
            String::new()
        }
    }
    pub fn label(&self) -> String {
        if let Some((i, _)) = &self.instance {
            format!("Instance {i}")
        } else if let Some(p) = &self.part {
            format!("Part {p}")
        } else if self.assembly {
            "Assembly".into()
        } else {
            String::new()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub line: usize,
    pub end_line: usize,
    pub start: usize,
    pub end: usize,
    pub norm: String,
    pub disp: String,
    pub params: Vec<(String, String, Option<String>)>,
    pub data_lines: u64,
    pub bulks: Vec<u64>,
    pub ctx: Ctx,
}

impl Block {
    pub fn param(&self, n: &str) -> Option<&str> {
        self.params.iter().find(|p| p.0 == n).and_then(|p| p.2.as_deref())
    }
    pub fn has_param(&self, n: &str) -> bool {
        self.params.iter().any(|p| p.0 == n)
    }
    pub fn label(&self) -> String {
        let mut s = self.disp.clone();
        let mut shown = 0;
        for key in ["NAME", "TYPE", "ELSET", "NSET", "INPUT", "MATERIAL", "INTERACTION", "PART"] {
            if let Some(v) = self.param(key) {
                if shown == 0 {
                    s.push_str("  ");
                } else {
                    s.push_str(" · ");
                }
                if key == "NAME" || key == "INPUT" {
                    s.push_str(v.trim_matches('"'));
                } else {
                    s.push_str(&format!("{}={}", key.to_ascii_lowercase(), v));
                }
                shown += 1;
                if shown == 2 {
                    break;
                }
            }
        }
        s
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Part,
    Assembly,
    Instance,
    Material,
    Interaction,
    Step,
    Block,
    Include,
    Bulk,
}

#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub label: String,
    pub detail: String,
    pub line: usize,
    pub block: Option<usize>,
    pub kind: NodeKind,
    pub children: Vec<OutlineNode>,
    pub err: bool,
}

#[derive(Clone, Debug)]
pub struct StepInfo {
    pub name: String,
    pub procedure: String,
    pub nlgeom: bool,
    pub line: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub keywords: usize,
    pub parts: Vec<String>,
    pub instances: Vec<(String, String)>,
    pub materials: Vec<String>,
    pub steps: Vec<StepInfo>,
    pub nsets: usize,
    pub elsets: usize,
    pub surfaces: usize,
    pub amplitudes: usize,
    pub sections: usize,
    pub interactions: usize,
    pub contact_pairs: usize,
    pub general_contact: bool,
    pub ties: usize,
    pub couplings: usize,
    pub includes: Vec<(String, bool)>,
    pub parameters: usize,
    pub product: Option<&'static str>,
    pub assembly_based: bool,
}

#[derive(Clone, Debug)]
pub struct IncludeInfo {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub raw: String,
    pub path: PathBuf,
    pub exists: bool,
}

pub struct Analysis {
    pub ver: u64,
    pub blocks: Vec<Block>,
    pub outline: Vec<OutlineNode>,
    pub defs: Vec<Def>,
    pub def_index: HashMap<(DefKind, String, String), Vec<usize>>,
    pub refs: Vec<Ref>,
    pub diags: Vec<Diag>,
    pub summary: Summary,
    pub instances: HashMap<String, String>,
    pub includes: Vec<IncludeInfo>,
    pub elapsed_ms: u64,
    /// instant syntax checks (syntax::check) of the same text version
    pub live: Vec<Diag>,
}

impl Analysis {
    pub fn block_at_line(&self, line: usize) -> Option<usize> {
        // blocks are sorted by line
        let i = match self.blocks.binary_search_by_key(&line, |b| b.line) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let b = &self.blocks[i];
        if line >= b.line && line <= b.end_line { Some(i) } else { None }
    }

    /// Find definitions for a name, trying the kinds in order.
    pub fn find_defs(&self, kinds: &[DefKind], name: &str, ctx: &Ctx) -> Vec<&Def> {
        let n = norm_label(name);
        let mut out = Vec::new();
        for &k in kinds {
            for scope in self.scopes_for(k, &n, ctx) {
                let (scope, nm) = scope;
                if let Some(v) = self.def_index.get(&(k, scope, nm)) {
                    out.extend(v.iter().map(|&i| &self.defs[i]));
                }
            }
            if !out.is_empty() {
                break;
            }
        }
        out
    }

    fn scopes_for(&self, k: DefKind, n: &str, ctx: &Ctx) -> Vec<(String, String)> {
        if k.global() {
            return vec![(String::new(), n.to_string())];
        }
        let mut v = Vec::new();
        if let Some((i, p)) = &ctx.instance {
            v.push((format!("I:{i}"), n.to_string()));
            v.push((format!("P:{p}"), n.to_string()));
        } else if let Some(p) = &ctx.part {
            v.push((format!("P:{p}"), n.to_string()));
        } else {
            if let Some(dot) = n.find('.') {
                let (inst, rest) = (&n[..dot], &n[dot + 1..]);
                v.push((format!("I:{inst}"), rest.to_string()));
                if let Some(p) = self.instances.get(inst) {
                    v.push((format!("P:{p}"), rest.to_string()));
                }
            }
            v.push((String::new(), n.to_string()));
        }
        v
    }

    /// All references whose name resolves to the same thing as `name` (by kind and normalized name).
    pub fn find_refs(&self, kind: DefKind, name: &str) -> Vec<&Ref> {
        let n = norm_label(name);
        let tail = n.rsplit('.').next().unwrap_or(&n).to_string();
        self.refs
            .iter()
            .filter(|r| (r.kind == kind || (matches!(kind, DefKind::Nset) && r.kind == DefKind::Nset)) && (r.name == n || r.name.rsplit('.').next() == Some(tail.as_str())))
            .collect()
    }
}

// ---------------------------------------------------------------- include scanning (cached)

#[derive(Clone)]
struct IncKw {
    norm: String,
    params: Vec<(String, Option<String>)>,
    line: u64,
}

type IncCache = HashMap<PathBuf, (Option<SystemTime>, u64, Arc<Vec<IncKw>>)>;
static INC_CACHE: LazyLock<Mutex<IncCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn include_keywords(path: &Path) -> Option<Arc<Vec<IncKw>>> {
    let md = std::fs::metadata(path).ok()?;
    let key = (md.modified().ok(), md.len());
    if let Some((m, l, v)) = INC_CACHE.lock().unwrap().get(path) {
        if (*m, *l) == key {
            return Some(v.clone());
        }
    }
    let src = crate::store::Source::open(path).ok()?;
    let b = src.bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    let mut line_no: u64 = 0;
    let mut cont: Option<usize> = None;
    while pos < b.len() {
        let nl = memchr::memchr(b'\n', &b[pos..]).map(|i| pos + i).unwrap_or(b.len());
        line_no += 1;
        let first = b[pos..nl].iter().position(|c| *c != b' ' && *c != b'\t');
        if let Some(fi) = first {
            let c0 = b[pos + fi];
            let is_kw = c0 == b'*' && b.get(pos + fi + 1) != Some(&b'*');
            if is_kw || cont.is_some() {
                let l = String::from_utf8_lossy(&b[pos..nl]).trim_end_matches('\r').to_string();
                if is_kw {
                    if let Some(k) = parse_keyword(&l) {
                        let ends = k.cont;
                        out.push(IncKw {
                            norm: k.norm.clone(),
                            params: k.params.iter().map(|p| (p.norm.clone(), p.value_str(&l).map(|s| s.to_string()))).collect(),
                            line: line_no,
                        });
                        cont = if ends { Some(out.len() - 1) } else { None };
                    }
                } else if let Some(ci) = cont {
                    let (ps, _, c, _) = parse_params(&l, 0);
                    for p in ps {
                        out[ci].params.push((p.norm.clone(), p.value_str(&l).map(|s| s.to_string())));
                    }
                    cont = if c { Some(ci) } else { None };
                }
            }
        }
        pos = nl + 1;
    }
    let v = Arc::new(out);
    INC_CACHE.lock().unwrap().insert(path.to_path_buf(), (key.0, key.1, v.clone()));
    Some(v)
}

pub fn resolve_include(dir: &Path, raw: &str) -> PathBuf {
    let r = raw.trim().trim_matches('"').trim();
    let p = Path::new(r);
    if p.is_absolute() { p.to_path_buf() } else { dir.join(p) }
}

// ---------------------------------------------------------------- analysis

const MATERIAL_BREAK: &[&str] = &[
    "ASSEMBLY", "ENDASSEMBLY", "ENDPART", "PART", "HEADING", "PREPRINT", "MATERIAL", "INITIALCONDITIONS",
    "PHYSICALCONSTANTS", "SECTIONCONTROLS", "DISTRIBUTIONTABLE", "TIMEPOINTS", "SPECTRUM", "SURFACEPROPERTY",
    "SURFACESMOOTHING", "CONNECTORBEHAVIOR", "CYCLICSYMMETRYMODEL", "DESIGNPARAMETER", "PARAMETERTABLETYPE",
    "PROPERTYTABLETYPE", "TABLECOLLECTION", "UNITSYSTEM", "MATRIXASSEMBLE", "MATRIXINPUT", "MAPSOLUTION",
    "IMPORTCONTROLS", "IMPORTELSET", "IMPORTNSET", "IMPORTSURFACE", "AQUA", "WAVE", "WIND", "PSD-DEFINITION",
    "FILTER", "FLUIDBEHAVIOR", "FLUIDCAVITY", "FLUIDEXCHANGE", "FLUIDEXCHANGEPROPERTY", "CAVITYDEFINITION",
    "ADAPTIVEMESHREFINEMENT", "FIELDMAPPERCONTROLS", "FIELDOPERATIONS", "MANIFEST", "SUBCYCLING",
    "SUBSTRUCTUREMODALDAMPING", "TRS", "CONTACTCLEARANCE", "CONTACTINITIALIZATIONDATA", "CONWEPCHARGEPROPERTY",
    "DETONATIONPOINT", "IMPEDANCEPROPERTY", "INCIDENTWAVEFLUIDPROPERTY", "INCIDENTWAVEINTERACTIONPROPERTY",
    "INCIDENTWAVEPROPERTY", "USERDEFINEDFIELD", "LOADINGDATA", "UNLOADINGDATA", "FILMPROPERTY", "INCLUDE",
    "PARAMETER", "IMPERFECTION", "ACOUSTICWAVEFORMULATION", "GAPFLOW", "FLUIDLEAKOFF",
];

const INTERACTION_CHILDREN: &[&str] = &[
    "FRICTION", "SURFACEBEHAVIOR", "CONTACTDAMPING", "GAPCONDUCTANCE", "GAPRADIATION", "GAPHEATGENERATION",
    "COHESIVEBEHAVIOR", "DAMAGEINITIATION", "DAMAGEEVOLUTION", "DAMAGESTABILIZATION", "CONTACTPERMEABILITY",
    "GAPELECTRICALCONDUCTANCE", "THERMALCONDUCTANCE", "CHANGEFRICTION", "CONTACTDAMAGE", "GAPFLOW",
];

const ELEMENT_DEF_KW: &[&str] = &["ELSET", "ELEMENT", "ELGEN", "ELCOPY"];
const NODE_DEF_KW: &[&str] = &["NSET", "NODE", "NGEN", "NFILL", "NCOPY", "NMAP"];

struct Builder<'a> {
    text: &'a str,
    ver: u64,
    dir: PathBuf,
    blocks: Vec<Block>,
    defs: Vec<Def>,
    refs: Vec<Ref>,
    diags: Vec<Diag>,
    ctx: Ctx,
    open_part: Option<usize>,
    open_asm: Option<usize>,
    open_inst: Option<usize>,
    open_step: Option<usize>,
    instances: HashMap<String, String>,
    includes: Vec<IncludeInfo>,
    steps: Vec<StepInfo>,
    param_uses: Vec<(String, usize, usize, usize)>,
    assembly_based: bool,
}

pub fn analyze(text: &str, ver: u64, dir: &Path) -> Analysis {
    let t0 = std::time::Instant::now();
    let assembly_based = {
        // cheap pre-check
        let mut ab = false;
        for (_, s, e) in util::lines(text) {
            let l = &text[s..e];
            if l.starts_with('*') && !l.starts_with("**") {
                let n = norm(l[1..].split(',').next().unwrap_or(""));
                if n == "PART" || n == "ASSEMBLY" {
                    ab = true;
                    break;
                }
            }
        }
        ab
    };
    let mut b = Builder {
        text,
        ver,
        dir: dir.to_path_buf(),
        blocks: Vec::new(),
        defs: Vec::new(),
        refs: Vec::new(),
        diags: Vec::new(),
        ctx: Ctx::default(),
        open_part: None,
        open_asm: None,
        open_inst: None,
        open_step: None,
        instances: HashMap::new(),
        includes: Vec::new(),
        steps: Vec::new(),
        param_uses: Vec::new(),
        assembly_based,
    };
    b.collect_blocks();
    b.walk();
    b.finish(t0)
}

impl<'a> Builder<'a> {
    fn collect_blocks(&mut self) {
        let text = self.text;
        let mut cur: Option<Block> = None;
        let mut kw_cont = false;
        let mut last_content_line = 0usize;
        let mut last_content_end = 0usize;
        for (li, s, e) in util::lines(text) {
            let line = &text[s..e];
            let kind = line_kind(line);
            if kw_cont && kind == LineKind::Data {
                let (ps, _, c, _) = parse_params(line, 0);
                if let Some(b) = cur.as_mut() {
                    for p in ps {
                        b.params.push((p.norm.clone(), line[p.name.0..p.name.1].to_string(), p.value_str(line).map(|v| v.to_string())));
                        if let Some((vs, ve)) = p.value {
                            self.note_param_uses(&line[vs..ve], li, s + vs);
                        }
                    }
                    b.end_line = li;
                    b.end = (e + 1).min(text.len());
                }
                kw_cont = c;
                last_content_line = li;
                last_content_end = e;
                continue;
            }
            kw_cont = false;
            match kind {
                LineKind::Keyword => {
                    if let Some(mut b) = cur.take() {
                        b.end_line = b.end_line.max(b.line);
                        self.blocks.push(b);
                    }
                    let k = parse_keyword(line).unwrap();
                    let disp = match SCHEMA.get(&k.norm) {
                        Some(d) => format!("*{}", d.display_name()),
                        None => format!("*{}", line[k.name.0..k.name.1].trim()),
                    };
                    let mut params = Vec::new();
                    for p in &k.params {
                        params.push((p.norm.clone(), line[p.name.0..p.name.1].to_string(), p.value_str(line).map(|v| v.to_string())));
                        if let Some((vs, ve)) = p.value {
                            self.note_param_uses(&line[vs..ve], li, s + vs);
                        }
                    }
                    kw_cont = k.cont;
                    cur = Some(Block {
                        line: li,
                        end_line: li,
                        start: s,
                        end: (e + 1).min(text.len()),
                        norm: k.norm,
                        disp,
                        params,
                        data_lines: 0,
                        bulks: Vec::new(),
                        ctx: Ctx::default(),
                    });
                    last_content_line = li;
                    last_content_end = e;
                }
                LineKind::Data => {
                    self.note_param_uses(line, li, s);
                    if let Some(b) = cur.as_mut() {
                        b.data_lines += 1;
                        b.end_line = li;
                        b.end = (e + 1).min(text.len());
                    }
                    last_content_line = li;
                    last_content_end = e;
                }
                LineKind::Placeholder => {
                    if let Some(b) = cur.as_mut() {
                        if let Some((id, _)) = placeholder_id(line) {
                            b.bulks.push(id);
                            if let Some(bk) = STORE.get(id) {
                                b.data_lines += bk.lines;
                            }
                        }
                        b.end_line = li;
                        b.end = (e + 1).min(text.len());
                    }
                    last_content_line = li;
                    last_content_end = e;
                }
                _ => {}
            }
        }
        let _ = (last_content_line, last_content_end);
        if let Some(b) = cur.take() {
            self.blocks.push(b);
        }
    }

    fn note_param_uses(&mut self, s: &str, line: usize, base: usize) {
        if !s.contains('<') {
            return;
        }
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'<' {
                if let Some(j) = s[i + 1..].find('>') {
                    let name = &s[i + 1..i + 1 + j];
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        self.param_uses.push((name.to_ascii_uppercase(), line, base + i, base + i + j + 2));
                    }
                    i += j + 2;
                    continue;
                }
            }
            i += 1;
        }
    }

    fn line_span(&self, line: usize) -> (usize, usize) {
        // only used for blocks: start of keyword line
        let b = self.blocks.iter().find(|b| b.line == line);
        b.map(|b| (b.start, b.start + self.text[b.start..].find('\n').unwrap_or(self.text.len() - b.start)))
            .unwrap_or((0, 0))
    }

    fn err(&mut self, sev: Sev, msg: String, bi: usize) {
        let b = &self.blocks[bi];
        let (s, _) = (b.start, b.end);
        let le = s + self.text[s..].find('\n').unwrap_or(self.text.len() - s);
        self.diags.push(Diag::new(sev, msg, b.line, s, le));
    }

    fn add_def(&mut self, kind: DefKind, name: &str, bi: usize) {
        let b = &self.blocks[bi];
        let n = norm_label(name);
        if n.is_empty() || n.starts_with('<') {
            return;
        }
        let scope = if kind.global() { String::new() } else { self.ctx.scope_tag() };
        let (s, e) = self.find_in_block(bi, name);
        self.defs.push(Def { kind, scope, name: n, disp: name.trim().trim_matches('"').to_string(), line: b.line, start: s, end: e, file: None });
    }

    fn find_in_block(&self, bi: usize, needle: &str) -> (usize, usize) {
        let b = &self.blocks[bi];
        let seg = &self.text[b.start..b.end.min(self.text.len())];
        let nd = needle.trim();
        match seg.find(nd) {
            Some(p) => (b.start + p, b.start + p + nd.len()),
            None => {
                let (s, e) = self.line_span(b.line);
                (s, e)
            }
        }
    }

    fn add_ref(&mut self, kind: DefKind, name: &str, line: usize, s: usize, e: usize) {
        let n = norm_label(name);
        if n.is_empty() || n.starts_with('<') {
            return;
        }
        // node / element labels, or instance.label
        let tail = n.rsplit('.').next().unwrap_or(&n);
        if util::is_integer(tail) {
            return;
        }
        let mut ctx = self.ctx.clone();
        ctx.step = None;
        self.refs.push(Ref { kind, ctx, name: n, disp: name.trim().to_string(), line, start: s, end: e });
    }

    fn param_ref(&mut self, kind: DefKind, bi: usize, pnorm: &str) {
        let b = &self.blocks[bi];
        if let Some(v) = b.param(pnorm).map(|s| s.to_string()) {
            let (s, e) = self.value_span(bi, pnorm, &v);
            let line = b.line;
            self.add_ref(kind, &v, line, s, e);
        }
    }

    fn value_span(&self, bi: usize, pnorm: &str, v: &str) -> (usize, usize) {
        let b = &self.blocks[bi];
        let seg_end = b.end.min(self.text.len());
        // keyword line(s) region: up to first data line; search "name=value"
        let seg = &self.text[b.start..seg_end];
        let up = seg.to_ascii_uppercase();
        let mut from = 0;
        while let Some(p) = up[from..].find('=') {
            let eqp = from + p;
            let before = norm(up[..eqp].rsplit(',').next().unwrap_or(""));
            if before == pnorm {
                let after = &seg[eqp + 1..];
                let off = after.len() - after.trim_start().len();
                let vs = b.start + eqp + 1 + off;
                return (vs, vs + v.len());
            }
            from = eqp + 1;
        }
        self.line_span(b.line)
    }

    fn data_fields(&self, bi: usize) -> Vec<(usize, Vec<(usize, usize)>)> {
        // (line index, absolute field spans) of the textual data lines of a block
        let b = &self.blocks[bi];
        let mut out = Vec::new();
        let mut in_kw = true;
        let seg = &self.text[b.start..b.end.min(self.text.len())];
        for (i, s, e) in util::lines(seg) {
            let line = &seg[s..e];
            if i == 0 {
                in_kw = line.trim_end().ends_with(',');
                continue;
            }
            if in_kw {
                if line_kind(line) == LineKind::Data {
                    in_kw = line.trim_end().ends_with(',');
                    continue;
                }
                in_kw = false;
            }
            if line_kind(line) != LineKind::Data {
                continue;
            }
            let f: Vec<(usize, usize)> = util::split_fields(line).into_iter().map(|(fs, fe)| (b.start + s + fs, b.start + s + fe)).collect();
            out.push((b.line + i, f));
        }
        out
    }

    fn walk(&mut self) {
        let n = self.blocks.len();
        for bi in 0..n {
            let kn = self.blocks[bi].norm.clone();
            let def = SCHEMA.get(&kn);
            // --- structure
            match kn.as_str() {
                "PART" => {
                    if let Some(p) = self.open_part {
                        let l = self.blocks[p].line;
                        self.err(Sev::Error, format!("*PART opened before the previous *PART (line {}) was closed with *END PART", l + 1), bi);
                    }
                    if self.open_asm.is_some() {
                        self.err(Sev::Error, "*PART cannot appear inside *ASSEMBLY".into(), bi);
                    }
                    if self.open_step.is_some() {
                        self.err(Sev::Error, "*PART cannot appear inside a step".into(), bi);
                    }
                    let name = self.blocks[bi].param("NAME").map(norm_label).unwrap_or_default();
                    self.open_part = Some(bi);
                    self.ctx.part = Some(name.clone());
                    self.blocks[bi].ctx = self.ctx.clone();
                    if let Some(v) = self.blocks[bi].param("NAME").map(|s| s.to_string()) {
                        self.ctx.part = None;
                        self.add_def(DefKind::Part, &v, bi);
                        self.ctx.part = Some(name);
                    }
                    continue;
                }
                "ENDPART" => {
                    if self.open_part.is_none() {
                        self.err(Sev::Error, "*END PART without a matching *PART".into(), bi);
                    }
                    self.blocks[bi].ctx = self.ctx.clone();
                    self.open_part = None;
                    self.ctx.part = None;
                    continue;
                }
                "ASSEMBLY" => {
                    if self.open_asm.is_some() {
                        self.err(Sev::Error, "Nested *ASSEMBLY".into(), bi);
                    }
                    if let Some(p) = self.open_part {
                        let l = self.blocks[p].line;
                        self.err(Sev::Error, format!("*ASSEMBLY before *END PART (part opened at line {})", l + 1), bi);
                        self.open_part = None;
                        self.ctx.part = None;
                    }
                    self.open_asm = Some(bi);
                    self.ctx.assembly = true;
                    self.blocks[bi].ctx = self.ctx.clone();
                    continue;
                }
                "ENDASSEMBLY" => {
                    if self.open_asm.is_none() {
                        self.err(Sev::Error, "*END ASSEMBLY without a matching *ASSEMBLY".into(), bi);
                    }
                    if let Some(i) = self.open_inst {
                        let l = self.blocks[i].line;
                        self.err(Sev::Error, format!("*END ASSEMBLY before *END INSTANCE (instance opened at line {})", l + 1), bi);
                        self.open_inst = None;
                        self.ctx.instance = None;
                    }
                    self.blocks[bi].ctx = self.ctx.clone();
                    self.open_asm = None;
                    self.ctx.assembly = false;
                    continue;
                }
                "INSTANCE" => {
                    if self.open_asm.is_none() {
                        self.err(Sev::Error, "*INSTANCE must be inside *ASSEMBLY".into(), bi);
                    }
                    if self.open_inst.is_some() {
                        self.err(Sev::Error, "*INSTANCE opened before the previous *END INSTANCE".into(), bi);
                    }
                    let name = self.blocks[bi].param("NAME").map(norm_label).unwrap_or_default();
                    let part = self.blocks[bi].param("PART").map(norm_label).unwrap_or_default();
                    if let Some(v) = self.blocks[bi].param("NAME").map(|s| s.to_string()) {
                        self.add_def(DefKind::Instance, &v, bi);
                    }
                    self.param_ref(DefKind::Part, bi, "PART");
                    self.instances.insert(name.clone(), part.clone());
                    self.open_inst = Some(bi);
                    self.ctx.instance = Some((name, part));
                    self.blocks[bi].ctx = self.ctx.clone();
                    continue;
                }
                "ENDINSTANCE" => {
                    if self.open_inst.is_none() {
                        self.err(Sev::Error, "*END INSTANCE without a matching *INSTANCE".into(), bi);
                    }
                    self.blocks[bi].ctx = self.ctx.clone();
                    self.open_inst = None;
                    self.ctx.instance = None;
                    continue;
                }
                "STEP" => {
                    if let Some(s) = self.open_step {
                        let l = self.blocks[s].line;
                        self.err(Sev::Error, format!("*STEP inside another step (missing *END STEP for the step at line {})", l + 1), bi);
                    }
                    if self.open_part.is_some() {
                        self.err(Sev::Error, "*STEP inside *PART (missing *END PART)".into(), bi);
                    }
                    if self.open_asm.is_some() {
                        self.err(Sev::Error, "*STEP inside *ASSEMBLY (missing *END ASSEMBLY)".into(), bi);
                    }
                    let name = self.blocks[bi].param("NAME").map(|s| s.trim_matches('"').to_string()).unwrap_or_else(|| format!("Step-{}", self.steps.len() + 1));
                    let nlgeom = self.blocks[bi].param("NLGEOM").map(|v| norm(v) == "YES").unwrap_or(self.blocks[bi].has_param("NLGEOM"));
                    if self.blocks[bi].param("NAME").is_some() {
                        let v = self.blocks[bi].param("NAME").unwrap().to_string();
                        self.add_def(DefKind::Step, &v, bi);
                    }
                    self.steps.push(StepInfo { name, procedure: String::new(), nlgeom, line: self.blocks[bi].line });
                    self.open_step = Some(bi);
                    self.ctx.step = Some(self.steps.len() - 1);
                    self.blocks[bi].ctx = self.ctx.clone();
                    continue;
                }
                "ENDSTEP" => {
                    if self.open_step.is_none() {
                        self.err(Sev::Error, "*END STEP without a matching *STEP".into(), bi);
                    }
                    self.blocks[bi].ctx = self.ctx.clone();
                    self.open_step = None;
                    self.ctx.step = None;
                    continue;
                }
                _ => {}
            }
            self.blocks[bi].ctx = self.ctx.clone();
            // procedure of the step = first keyword after *STEP
            if let Some(si) = self.ctx.step {
                if self.steps[si].procedure.is_empty() {
                    let mut p = self.blocks[bi].disp.trim_start_matches('*').to_string();
                    if self.blocks[bi].has_param("EXPLICIT") {
                        p.push_str(", Explicit");
                    }
                    self.steps[si].procedure = p;
                }
            }
            if let Some(d) = def {
                self.placement_checks(bi, d);
            }
            self.defs_and_refs(bi, &kn);
        }
        // unclosed
        if let Some(p) = self.open_part {
            self.err(Sev::Error, "*PART is never closed with *END PART".into(), p);
        }
        if let Some(a) = self.open_asm {
            self.err(Sev::Error, "*ASSEMBLY is never closed with *END ASSEMBLY".into(), a);
        }
        if let Some(i) = self.open_inst {
            self.err(Sev::Error, "*INSTANCE is never closed with *END INSTANCE".into(), i);
        }
        if let Some(s) = self.open_step {
            self.err(Sev::Error, "*STEP is never closed with *END STEP".into(), s);
        }
    }

    fn placement_checks(&mut self, bi: usize, d: &KeywordDef) {
        let in_step = self.ctx.step.is_some();
        if in_step && d.model_only() {
            self.err(Sev::Error, format!("*{} is model data and cannot appear inside a step", d.name), bi);
            return;
        }
        if !in_step && d.step_only() {
            self.err(Sev::Error, format!("*{} is history data: it must be inside *STEP ... *END STEP", d.name), bi);
            return;
        }
        // Level checks (part / assembly / instance / model level) are not reported: flat input
        // files and generated files mix levels legitimately, so they produced mostly noise.
    }

    fn defs_and_refs(&mut self, bi: usize, kn: &str) {
        let params = self.blocks[bi].params.clone();
        let def = SCHEMA.get(kn);
        for (pn, _, val) in &params {
            let Some(v) = val.clone() else { continue };
            match pn.as_str() {
                "ELSET" => {
                    if ELEMENT_DEF_KW.contains(&kn) {
                        self.add_def(DefKind::Elset, &v, bi);
                    } else {
                        self.param_ref(DefKind::Elset, bi, "ELSET");
                    }
                }
                "NSET" => {
                    if NODE_DEF_KW.contains(&kn) {
                        self.add_def(DefKind::Nset, &v, bi);
                    } else {
                        self.param_ref(DefKind::Nset, bi, "NSET");
                    }
                }
                "MATERIAL" => self.param_ref(DefKind::Material, bi, "MATERIAL"),
                "AMPLITUDE" => {
                    let is_enum = def.and_then(|d| d.param("AMPLITUDE")).map(|p| !p.values.is_empty()).unwrap_or(false);
                    if !is_enum && kn != "AMPLITUDE" {
                        self.param_ref(DefKind::Amplitude, bi, "AMPLITUDE");
                    }
                }
                "INTERACTION" => self.param_ref(DefKind::Interaction, bi, "INTERACTION"),
                "ORIENTATION" if kn != "ORIENTATION" => self.param_ref(DefKind::Orientation, bi, "ORIENTATION"),
                "SURFACE" if kn != "SURFACE" => self.param_ref(DefKind::Surface, bi, "SURFACE"),
                "REFNODE" | "TIENSET" | "PINNSET" | "REFERENCENODE" => self.param_ref(DefKind::Nset, bi, pn),
                "CONTROLS" if kn.ends_with("SECTION") => self.param_ref(DefKind::SectionControls, bi, "CONTROLS"),
                "NAME" => {
                    let k = match kn {
                        "MATERIAL" => Some(DefKind::Material),
                        "AMPLITUDE" => Some(DefKind::Amplitude),
                        "SURFACE" => Some(DefKind::Surface),
                        "SURFACEINTERACTION" => Some(DefKind::Interaction),
                        "ORIENTATION" => Some(DefKind::Orientation),
                        "SECTIONCONTROLS" => Some(DefKind::SectionControls),
                        _ => None,
                    };
                    if let Some(k) = k {
                        self.add_def(k, &v, bi);
                    }
                }
                _ => {}
            }
        }
        // data-line references
        let first_field_kind: Option<DefKind> = match kn {
            "BOUNDARY" | "CLOAD" | "TEMPERATURE" | "CFLUX" => Some(DefKind::Nset),
            "DLOAD" | "DFLUX" | "FILM" => Some(DefKind::Elset),
            "DSLOAD" | "DSFLUX" | "SFILM" | "DSECHARGE" | "DSECURRENT" => Some(DefKind::Surface),
            "SURFACE" => {
                let t = self.blocks[bi].param("TYPE").map(norm).unwrap_or_else(|| "ELEMENT".into());
                match t.as_str() {
                    "ELEMENT" => Some(DefKind::Elset),
                    "NODE" => Some(DefKind::Nset),
                    _ => None,
                }
            }
            _ => None,
        };
        let pair_surfaces = matches!(kn, "CONTACTPAIR" | "TIE" | "CONTACTINCLUSIONS" | "CONTACTEXCLUSIONS" | "CONTACTPROPERTYASSIGNMENT");
        let set_members = match kn {
            "NSET" if !self.blocks[bi].has_param("GENERATE") => Some(DefKind::Nset),
            "ELSET" if !self.blocks[bi].has_param("GENERATE") => Some(DefKind::Elset),
            _ => None,
        };
        if first_field_kind.is_none() && !pair_surfaces && set_members.is_none() && kn != "PARAMETER" {
            return;
        }
        let rows = self.data_fields(bi);
        let saved_ctx = self.ctx.clone();
        if set_members.is_some() {
            if let Some(inst) = self.blocks[bi].param("INSTANCE").map(norm_label) {
                let part = self.instances.get(&inst).cloned().unwrap_or_default();
                self.ctx.instance = Some((inst, part));
            }
        }
        for (line, fields) in rows {
            if kn == "PARAMETER" {
                if let Some(&(fs, fe)) = fields.first() {
                    let f = &self.text[fs..fe];
                    if let Some(eq) = f.find('=') {
                        let name = f[..eq].trim().to_string();
                        if !name.is_empty() {
                            let n = name.to_ascii_uppercase();
                            self.defs.push(Def { kind: DefKind::Param, scope: String::new(), name: n, disp: name.clone(), line, start: fs, end: fs + eq, file: None });
                        }
                    }
                }
                continue;
            }
            if let Some(k) = first_field_kind {
                if let Some(&(fs, fe)) = fields.first() {
                    let f = self.text[fs..fe].to_string();
                    self.add_ref(k, &f, line, fs, fe);
                }
            }
            if pair_surfaces {
                let take = if kn == "CONTACTPROPERTYASSIGNMENT" { 2 } else { 2 };
                for &(fs, fe) in fields.iter().take(take) {
                    let f = self.text[fs..fe].to_string();
                    self.add_ref(DefKind::Surface, &f, line, fs, fe);
                }
                if kn == "CONTACTPROPERTYASSIGNMENT" {
                    if let Some(&(fs, fe)) = fields.get(2) {
                        let f = self.text[fs..fe].to_string();
                        self.add_ref(DefKind::Interaction, &f, line, fs, fe);
                    }
                }
            }
            if let Some(k) = set_members {
                for &(fs, fe) in &fields {
                    let f = self.text[fs..fe].to_string();
                    if !f.is_empty() && !util::is_integer(&f) {
                        self.add_ref(k, &f, line, fs, fe);
                    }
                }
            }
        }
        self.ctx = saved_ctx;
    }

    fn process_includes(&mut self) {
        // *INCLUDE: record, and harvest definitions from the included files (scope-aware)
        for bi in 0..self.blocks.len() {
            if self.blocks[bi].norm != "INCLUDE" {
                continue;
            }
            let Some(raw) = self.blocks[bi].param("INPUT").map(|s| s.to_string()) else { continue };
            let path = resolve_include(&self.dir, &raw);
            let exists = path.is_file();
            let (s, e) = self.value_span(bi, "INPUT", &raw);
            self.includes.push(IncludeInfo { line: self.blocks[bi].line, start: s, end: e, raw: raw.clone(), path: path.clone(), exists });
            if !exists {
                self.diags.push(Diag::new(Sev::Warning, format!("Include file not found: {}", path.display()), self.blocks[bi].line, s, e));
                continue;
            }
            let ctx = self.blocks[bi].ctx.clone();
            self.harvest_include(&path, ctx, 0);
        }
    }

    fn harvest_include(&mut self, path: &Path, ctx0: Ctx, depth: usize) {
        if depth > 6 {
            return;
        }
        let Some(kws) = include_keywords(path) else { return };
        let mut ctx = ctx0;
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for k in kws.iter() {
            let get = |n: &str| k.params.iter().find(|p| p.0 == n).and_then(|p| p.1.clone());
            match k.norm.as_str() {
                "PART" => ctx.part = get("NAME").map(|s| norm_label(&s)),
                "ENDPART" => ctx.part = None,
                "INSTANCE" => {
                    let n = get("NAME").map(|s| norm_label(&s)).unwrap_or_default();
                    let p = get("PART").map(|s| norm_label(&s)).unwrap_or_default();
                    self.instances.insert(n.clone(), p.clone());
                    if !n.is_empty() {
                        self.push_inc_def(DefKind::Instance, String::new(), &n, path, k.line);
                    }
                    ctx.instance = Some((n, p));
                }
                "ENDINSTANCE" => ctx.instance = None,
                "INCLUDE" => {
                    if let Some(r) = get("INPUT") {
                        let p = resolve_include(&dir, &r);
                        self.harvest_include(&p, ctx.clone(), depth + 1);
                    }
                    continue;
                }
                _ => {}
            }
            let scope = ctx.scope_tag();
            for (pn, v) in &k.params {
                let Some(v) = v else { continue };
                let kind = match (pn.as_str(), k.norm.as_str()) {
                    ("ELSET", kw) if ELEMENT_DEF_KW.contains(&kw) => Some(DefKind::Elset),
                    ("NSET", kw) if NODE_DEF_KW.contains(&kw) => Some(DefKind::Nset),
                    ("NAME", "MATERIAL") => Some(DefKind::Material),
                    ("NAME", "AMPLITUDE") => Some(DefKind::Amplitude),
                    ("NAME", "SURFACE") => Some(DefKind::Surface),
                    ("NAME", "SURFACEINTERACTION") => Some(DefKind::Interaction),
                    ("NAME", "ORIENTATION") => Some(DefKind::Orientation),
                    ("NAME", "PART") => Some(DefKind::Part),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let sc = if kind.global() { String::new() } else { scope.clone() };
                    self.push_inc_def(kind, sc, v, path, k.line);
                }
            }
        }
    }

    fn push_inc_def(&mut self, kind: DefKind, scope: String, name: &str, path: &Path, line: u64) {
        let n = norm_label(name);
        self.defs.push(Def { kind, scope, name: n, disp: name.trim_matches('"').to_string(), line: 0, start: 0, end: 0, file: Some((path.to_path_buf(), line)) });
    }

    fn finish(mut self, t0: std::time::Instant) -> Analysis {
        self.process_includes();
        // index definitions
        let mut def_index: HashMap<(DefKind, String, String), Vec<usize>> = HashMap::new();
        for (i, d) in self.defs.iter().enumerate() {
            def_index.entry((d.kind, d.scope.clone(), d.name.clone())).or_default().push(i);
        }
        // duplicates (sets may legally be repeated)
        for ((k, _, _), v) in &def_index {
            if v.len() > 1 && matches!(k, DefKind::Material | DefKind::Part | DefKind::Instance | DefKind::Amplitude | DefKind::Interaction | DefKind::Step | DefKind::Surface) {
                for &i in &v[1..] {
                    let d = &self.defs[i];
                    if d.file.is_none() {
                        self.diags.push(Diag::new(Sev::Warning, format!("Duplicate {} name '{}'", k.label(), d.disp), d.line, d.start, d.end));
                    }
                }
            }
        }
        let mut a = Analysis {
            ver: self.ver,
            blocks: std::mem::take(&mut self.blocks),
            outline: Vec::new(),
            defs: std::mem::take(&mut self.defs),
            def_index,
            refs: std::mem::take(&mut self.refs),
            diags: std::mem::take(&mut self.diags),
            summary: Summary::default(),
            instances: std::mem::take(&mut self.instances),
            includes: std::mem::take(&mut self.includes),
            elapsed_ms: 0,
            live: Vec::new(),
        };
        // unresolved references (warnings; definitions inside unreadable includes are unknown)
        let any_missing_include = a.includes.iter().any(|i| !i.exists);
        let mut unresolved = Vec::new();
        for r in &a.refs {
            if a.find_defs(&[r.kind], &r.name, &r.ctx).is_empty() {
                // a node set reference may also be an element set or surface in some keywords
                let alt: &[DefKind] = match r.kind {
                    DefKind::Nset => &[DefKind::Nset],
                    DefKind::Elset => &[DefKind::Elset],
                    _ => &[],
                };
                if !alt.is_empty() && !a.find_defs(alt, &r.name, &r.ctx).is_empty() {
                    continue;
                }
                let hint = if any_missing_include { " (an include file is missing)" } else { "" };
                unresolved.push(Diag::new(Sev::Warning, format!("Undefined {} '{}'{}", r.kind.label(), r.disp, hint), r.line, r.start, r.end));
            }
        }
        a.diags.extend(unresolved);
        // <parameter> references
        let has_params = a.defs.iter().any(|d| d.kind == DefKind::Param);
        for (name, line, s, e) in &self.param_uses {
            if !a.def_index.contains_key(&(DefKind::Param, String::new(), name.clone())) {
                let why = if has_params { "" } else { " (no *PARAMETER definitions in this file)" };
                a.diags.push(Diag::new(Sev::Error, format!("Undefined parameter <{}>{}", name.to_ascii_lowercase(), why), *line, *s, *e));
            }
        }
        // summary + product checks
        let mut sm = Summary { keywords: a.blocks.len(), steps: self.steps.clone(), assembly_based: self.assembly_based, ..Default::default() };
        let mut explicit_steps = 0;
        let mut standard_steps = 0;
        for st in &sm.steps {
            if st.procedure.ends_with(", Explicit") {
                explicit_steps += 1;
            } else if !st.procedure.is_empty() {
                standard_steps += 1;
            }
        }
        sm.product = match (standard_steps, explicit_steps) {
            (0, 0) => None,
            (_, 0) => Some("Abaqus/Standard"),
            (0, _) => Some("Abaqus/Explicit"),
            _ => None,
        };
        for b in &a.blocks {
            match b.norm.as_str() {
                "PART" => sm.parts.push(b.param("NAME").unwrap_or("?").trim_matches('"').to_string()),
                "INSTANCE" => sm.instances.push((b.param("NAME").unwrap_or("?").to_string(), b.param("PART").unwrap_or("?").to_string())),
                "MATERIAL" => sm.materials.push(b.param("NAME").unwrap_or("?").trim_matches('"').to_string()),
                "NSET" => sm.nsets += 1,
                "ELSET" => sm.elsets += 1,
                "SURFACE" => sm.surfaces += 1,
                "AMPLITUDE" => sm.amplitudes += 1,
                "SURFACEINTERACTION" => sm.interactions += 1,
                "CONTACTPAIR" => sm.contact_pairs += 1,
                "CONTACT" => sm.general_contact = true,
                "TIE" => sm.ties += 1,
                "COUPLING" | "KINEMATICCOUPLING" => sm.couplings += 1,
                "PARAMETER" => sm.parameters += b.data_lines as usize,
                n if n.ends_with("SECTION") && n != "PRE-TENSIONSECTION" => sm.sections += 1,
                _ => {}
            }
        }
        for inc in &a.includes {
            sm.includes.push((inc.raw.clone(), inc.exists));
        }
        if let Some(prod) = sm.product {
            let explicit = prod.ends_with("Explicit");
            let mut extra = Vec::new();
            for b in &a.blocks {
                if let Some(d) = SCHEMA.get(&b.norm) {
                    if !(d.standard || d.explicit) {
                        continue;
                    }
                    if explicit && !d.explicit {
                        extra.push((b, format!("*{} is not available in Abaqus/Explicit", d.name)));
                    } else if !explicit && !d.standard {
                        extra.push((b, format!("*{} is not available in Abaqus/Standard", d.name)));
                    }
                }
            }
            let ds: Vec<Diag> = extra
                .into_iter()
                .map(|(b, m)| {
                    let le = b.start + self.text[b.start..].find('\n').unwrap_or(self.text.len() - b.start);
                    Diag::new(Sev::Warning, m, b.line, b.start, le)
                })
                .collect();
            a.diags.extend(ds);
        }
        a.summary = sm;
        a.diags.sort_by_key(|d| (d.start, d.end));
        a.outline = build_outline(&a);
        a.elapsed_ms = t0.elapsed().as_millis() as u64;
        a
    }
}

fn leaf(b: &Block, bi: usize) -> OutlineNode {
    let detail = if !b.bulks.is_empty() || b.data_lines > 50 { format!("{} lines", util::fmt_int(b.data_lines)) } else { String::new() };
    let kind = if b.norm == "INCLUDE" {
        NodeKind::Include
    } else if !b.bulks.is_empty() {
        NodeKind::Bulk
    } else {
        NodeKind::Block
    };
    OutlineNode { label: b.label(), detail, line: b.line, block: Some(bi), kind, children: Vec::new(), err: false }
}

/// Long lists (typical for flat input files without parts) are grouped by keyword:
/// e.g. 40,000 *Nset blocks become one "*Nset" entry that can be expanded.
fn group_big(list: &mut Vec<OutlineNode>, a: &Analysis) {
    for n in list.iter_mut() {
        if !n.children.is_empty() {
            group_big(&mut n.children, a);
        }
    }
    if list.len() <= 200 {
        return;
    }
    let kw = |n: &OutlineNode| -> Option<String> {
        if !n.children.is_empty() {
            return None;
        }
        n.block.map(|bi| a.blocks[bi].norm.clone())
    };
    let mut count: HashMap<String, usize> = HashMap::new();
    for n in list.iter() {
        if let Some(k) = kw(n) {
            *count.entry(k).or_default() += 1;
        }
    }
    if !count.values().any(|&c| c >= 20) {
        return;
    }
    let old = std::mem::take(list);
    let mut group_at: HashMap<String, usize> = HashMap::new();
    for n in old {
        match kw(&n) {
            Some(k) if count[&k] >= 20 => {
                if let Some(&gi) = group_at.get(&k) {
                    list[gi].children.push(n);
                } else {
                    let disp = n.block.map(|bi| a.blocks[bi].disp.clone()).unwrap_or_else(|| k.clone());
                    group_at.insert(k.clone(), list.len());
                    list.push(OutlineNode {
                        label: format!("*{}", disp.trim_start_matches('*')),
                        detail: format!("{} blocks", util::fmt_int(count[&k] as u64)),
                        line: n.line,
                        block: None,
                        kind: NodeKind::Block,
                        children: vec![n],
                        err: false,
                    });
                }
            }
            _ => list.push(n),
        }
    }
}

pub fn build_outline(a: &Analysis) -> Vec<OutlineNode> {
    let mut root: Vec<OutlineNode> = Vec::new();
    // stack of open containers: (kind, node)
    let mut stack: Vec<OutlineNode> = Vec::new();
    let close_until = |stack: &mut Vec<OutlineNode>, root: &mut Vec<OutlineNode>, keep: &dyn Fn(NodeKind) -> bool| {
        while let Some(top) = stack.last() {
            if keep(top.kind) {
                break;
            }
            let n = stack.pop().unwrap();
            match stack.last_mut() {
                Some(p) => p.children.push(n),
                None => root.push(n),
            }
        }
    };
    let push_leaf = |stack: &mut Vec<OutlineNode>, root: &mut Vec<OutlineNode>, n: OutlineNode| match stack.last_mut() {
        Some(p) => p.children.push(n),
        None => root.push(n),
    };
    for (bi, b) in a.blocks.iter().enumerate() {
        let name = b.param("NAME").map(|s| s.trim_matches('"').to_string()).unwrap_or_default();
        match b.norm.as_str() {
            "PART" => {
                close_until(&mut stack, &mut root, &|_| false);
                stack.push(OutlineNode { label: format!("Part  {name}"), detail: String::new(), line: b.line, block: Some(bi), kind: NodeKind::Part, children: vec![], err: false });
            }
            "ASSEMBLY" => {
                close_until(&mut stack, &mut root, &|_| false);
                stack.push(OutlineNode { label: format!("Assembly  {name}"), detail: String::new(), line: b.line, block: Some(bi), kind: NodeKind::Assembly, children: vec![], err: false });
            }
            "INSTANCE" => {
                close_until(&mut stack, &mut root, &|k| k == NodeKind::Assembly);
                let part = b.param("PART").unwrap_or("");
                stack.push(OutlineNode { label: format!("Instance  {name}"), detail: part.to_string(), line: b.line, block: Some(bi), kind: NodeKind::Instance, children: vec![], err: false });
            }
            "STEP" => {
                close_until(&mut stack, &mut root, &|_| false);
                let proc_ = a.summary.steps.iter().find(|s| s.line == b.line).map(|s| s.procedure.clone()).unwrap_or_default();
                let nm = if name.is_empty() { "(unnamed)".to_string() } else { name.clone() };
                stack.push(OutlineNode { label: format!("Step  {nm}"), detail: proc_, line: b.line, block: Some(bi), kind: NodeKind::Step, children: vec![], err: false });
            }
            "MATERIAL" => {
                close_until(&mut stack, &mut root, &|k| matches!(k, NodeKind::Part | NodeKind::Assembly | NodeKind::Instance | NodeKind::Step));
                stack.push(OutlineNode { label: format!("Material  {name}"), detail: String::new(), line: b.line, block: Some(bi), kind: NodeKind::Material, children: vec![], err: false });
            }
            "SURFACEINTERACTION" => {
                close_until(&mut stack, &mut root, &|k| matches!(k, NodeKind::Part | NodeKind::Assembly | NodeKind::Instance | NodeKind::Step));
                stack.push(OutlineNode { label: format!("Interaction  {name}"), detail: String::new(), line: b.line, block: Some(bi), kind: NodeKind::Interaction, children: vec![], err: false });
            }
            "ENDPART" | "ENDASSEMBLY" | "ENDINSTANCE" | "ENDSTEP" => {
                let target = match b.norm.as_str() {
                    "ENDPART" => NodeKind::Part,
                    "ENDASSEMBLY" => NodeKind::Assembly,
                    "ENDINSTANCE" => NodeKind::Instance,
                    _ => NodeKind::Step,
                };
                if stack.iter().any(|n| n.kind == target) {
                    close_until(&mut stack, &mut root, &|k| k == target);
                    let n = stack.pop().unwrap();
                    match stack.last_mut() {
                        Some(p) => p.children.push(n),
                        None => root.push(n),
                    }
                } else {
                    push_leaf(&mut stack, &mut root, leaf(b, bi));
                }
            }
            n => {
                // close material / interaction blocks when a non-member keyword arrives
                if let Some(top) = stack.last() {
                    let member = match top.kind {
                        NodeKind::Material => {
                            let d = SCHEMA.get(n);
                            !MATERIAL_BREAK.contains(&n)
                                && d.map(|d| d.kind == crate::schema::KwType::Model && d.level.len() == 1 && d.level[0] == "Model").unwrap_or(false)
                        }
                        NodeKind::Interaction => INTERACTION_CHILDREN.contains(&n),
                        _ => true,
                    };
                    if !member {
                        let n = stack.pop().unwrap();
                        match stack.last_mut() {
                            Some(p) => p.children.push(n),
                            None => root.push(n),
                        }
                    }
                }
                push_leaf(&mut stack, &mut root, leaf(b, bi));
            }
        }
    }
    close_until(&mut stack, &mut root, &|_| false);
    group_big(&mut root, a);
    // mark nodes with errors
    let err_lines: std::collections::HashSet<usize> = a.diags.iter().filter(|d| d.sev == Sev::Error).map(|d| d.line).collect();
    fn mark(n: &mut OutlineNode, a: &Analysis, el: &std::collections::HashSet<usize>) -> bool {
        let mut e = false;
        if let Some(bi) = n.block {
            let b = &a.blocks[bi];
            e = (b.line..=b.end_line).any(|l| el.contains(&l));
        }
        for c in &mut n.children {
            if mark(c, a, el) {
                e = true;
            }
        }
        n.err = e;
        e
    }
    for n in &mut root {
        mark(n, a, &err_lines);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    const M: &str = "*Heading
test
*Part, name=Plate
*Node
1, 0., 0., 0.
2, 1., 0., 0.
*Element, type=T3D2, elset=BAR
1, 1, 2
*Nset, nset=FIX
1,
*Solid Section, elset=BAR, material=STEEL
1.
*End Part
*Assembly, name=A
*Instance, name=Plate-1, part=Plate
*End Instance
*Nset, nset=LOAD, instance=Plate-1
2,
*End Assembly
*Material, name=STEEL
*Elastic
210000., 0.3
*Density
7.85e-9,
*Step, name=Load, nlgeom=YES
*Static
1., 1.
*Boundary
Plate-1.FIX, ENCASTRE
NOPE, 1, 1
*Cload
LOAD, 1, <force>
*End Step
";
    #[test]
    fn analysis_basic() {
        let a = analyze(M, 1, Path::new("."));
        let msgs: Vec<&str> = a.diags.iter().map(|d| d.msg.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("Undefined node set 'NOPE'")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("Undefined parameter <force>")), "{msgs:?}");
        assert_eq!(msgs.len(), 2, "{msgs:?}");
        assert_eq!(a.summary.steps.len(), 1);
        assert_eq!(a.summary.steps[0].procedure, "Static");
        assert_eq!(a.summary.product, Some("Abaqus/Standard"));
        let labels: Vec<String> = a.outline.iter().map(|n| n.label.clone()).collect();
        assert!(labels.contains(&"Part  Plate".to_string()), "{labels:?}");
        assert!(labels.contains(&"Material  STEEL".to_string()), "{labels:?}");
        let mat = a.outline.iter().find(|n| n.label == "Material  STEEL").unwrap();
        assert_eq!(mat.children.len(), 2);
    }
    #[test]
    fn structure_errors() {
        let t = "*Part, name=P\n*Node\n1,0,0,0\n*Step\n*Static\n*Boundary\n1, 1\n*Material, name=X\n";
        let a = analyze(t, 1, Path::new("."));
        let msgs: Vec<&str> = a.diags.iter().map(|d| d.msg.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("*STEP inside *PART")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("MATERIAL is model data")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("never closed")), "{msgs:?}");
    }
}
