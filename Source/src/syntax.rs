//! Line lexer, syntax highlighting and instant (line-local) syntax checks.

use crate::schema::{GroupKind, KeywordDef, SCHEMA};
use crate::store::{placeholder_id, STORE};
use crate::util::{self, norm};
use egui::text::{LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontId, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Sev {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub sev: Sev,
    pub msg: String,
    /// absolute byte range in the skeleton text (for underlines)
    pub start: usize,
    pub end: usize,
    /// skeleton line index
    pub line: usize,
    /// for problems inside bulk data: (bulk id, row)
    pub bulk: Option<(u64, u64)>,
    /// for problems in other files (includes)
    pub file: Option<std::path::PathBuf>,
}

impl Diag {
    pub fn new(sev: Sev, msg: impl Into<String>, line: usize, start: usize, end: usize) -> Diag {
        Diag { sev, msg: msg.into(), start, end, line, bulk: None, file: None }
    }
}

// ---------------------------------------------------------------- lexing

#[derive(Clone, Debug)]
pub struct PTok {
    pub name: (usize, usize),
    pub norm: String,
    pub eq: Option<usize>,
    pub value: Option<(usize, usize)>,
}

impl PTok {
    pub fn value_str<'a>(&self, line: &'a str) -> Option<&'a str> {
        self.value.map(|(s, e)| &line[s..e])
    }
}

#[derive(Clone, Debug)]
pub struct KwParse {
    pub name: (usize, usize),
    pub norm: String,
    pub params: Vec<PTok>,
    pub cont: bool,
    pub bad_quote: bool,
    pub commas: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Blank,
    Comment,
    Placeholder,
    Keyword,
    Data,
}

pub fn line_kind(line: &str) -> LineKind {
    let t = line.trim_start_matches([' ', '\t']);
    if t.is_empty() {
        LineKind::Blank
    } else if t.starts_with("**") {
        if line.starts_with(crate::store::PH_PREFIX) {
            LineKind::Placeholder
        } else {
            LineKind::Comment
        }
    } else if t.starts_with('*') {
        LineKind::Keyword
    } else {
        LineKind::Data
    }
}

/// Split a list of `name[=value]` items separated by commas (quotes respected).
/// Returns (params, comma positions, ends_with_comma, unbalanced_quote).
pub fn parse_params(line: &str, from: usize) -> (Vec<PTok>, Vec<usize>, bool, bool) {
    let b = line.as_bytes();
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut commas = Vec::new();
    let mut inq = false;
    let mut s = from;
    for i in from..b.len() {
        match b[i] {
            b'"' => inq = !inq,
            b',' if !inq => {
                items.push((s, i));
                commas.push(i);
                s = i + 1;
            }
            _ => {}
        }
    }
    items.push((s, b.len()));
    let mut out = Vec::new();
    for (s, e) in items {
        let (s, e) = util::trim_span(b, s, e);
        if s >= e {
            continue;
        }
        // find '=' outside quotes
        let mut eq = None;
        let mut q = false;
        for i in s..e {
            match b[i] {
                b'"' => q = !q,
                b'=' if !q => {
                    eq = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let (name, value) = match eq {
            Some(p) => {
                let n = util::trim_span(b, s, p);
                let v = util::trim_span(b, p + 1, e);
                (n, Some(v))
            }
            None => ((s, e), None),
        };
        out.push(PTok { norm: norm(&line[name.0..name.1]), name, eq, value });
    }
    let cont = line[from..].trim_end().ends_with(',');
    (out, commas, cont, inq)
}

pub fn parse_keyword(line: &str) -> Option<KwParse> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i >= b.len() || b[i] != b'*' || (i + 1 < b.len() && b[i + 1] == b'*') {
        return None;
    }
    let ns = i + 1;
    let mut ne = ns;
    while ne < b.len() && b[ne] != b',' {
        ne += 1;
    }
    let (s, e) = util::trim_span(b, ns, ne);
    let (params, mut commas, cont, bad_quote) =
        if ne < b.len() { parse_params(line, ne + 1) } else { (vec![], vec![], false, false) };
    if ne < b.len() {
        commas.insert(0, ne);
    }
    Some(KwParse { name: (s, e), norm: norm(&line[s..e]), params, cont: cont || (ne < b.len() && line[ne + 1..].trim().is_empty()), bad_quote, commas })
}

// ---------------------------------------------------------------- colors

pub struct Palette;
impl Palette {
    pub const KEYWORD: Color32 = Color32::from_rgb(0, 84, 200);
    pub const PARAM: Color32 = Color32::from_rgb(128, 38, 160);
    pub const VALUE: Color32 = Color32::from_rgb(176, 84, 0);
    pub const STRING: Color32 = Color32::from_rgb(163, 21, 21);
    pub const PUNCT: Color32 = Color32::from_rgb(140, 140, 140);
    pub const COMMENT: Color32 = Color32::from_rgb(92, 140, 80);
    pub const DATA: Color32 = Color32::from_rgb(40, 40, 46);
    pub const NAME: Color32 = Color32::from_rgb(0, 120, 110);
    pub const PREF: Color32 = Color32::from_rgb(0, 128, 160);
    pub const PH_TOKEN: Color32 = Color32::from_rgb(170, 176, 190);
    pub const PH_FG: Color32 = Color32::from_rgb(40, 70, 130);
    pub const PH_BG: Color32 = Color32::from_rgb(226, 236, 252);
    pub const ERR: Color32 = Color32::from_rgb(220, 30, 30);
    pub const ERR_BG: Color32 = Color32::from_rgb(255, 228, 228);
    pub const WARN: Color32 = Color32::from_rgb(230, 140, 0);
    pub const WARN_BG: Color32 = Color32::from_rgb(255, 243, 214);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Plain,
    Keyword,
    Param,
    Value,
    Str,
    Punct,
    Comment,
    Data,
    Name,
    PRef,
    PhToken,
    PhChip,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    s: usize,
    e: usize,
    st: St,
    sev: Option<Sev>,
}

fn fmt(st: St, sev: Option<Sev>, font: &FontId) -> TextFormat {
    let mut f = TextFormat { font_id: font.clone(), ..Default::default() };
    f.color = match st {
        St::Plain => crate::theme::t(Palette::DATA),
        St::Keyword => crate::theme::t(Palette::KEYWORD),
        St::Param => crate::theme::t(Palette::PARAM),
        St::Value => crate::theme::t(Palette::VALUE),
        St::Str => crate::theme::t(Palette::STRING),
        St::Punct => crate::theme::t(Palette::PUNCT),
        St::Comment => crate::theme::t(Palette::COMMENT),
        St::Data => crate::theme::t(Palette::DATA),
        St::Name => crate::theme::t(Palette::NAME),
        St::PRef => crate::theme::t(Palette::PREF),
        St::PhToken => crate::theme::t(Palette::PH_TOKEN),
        St::PhChip => crate::theme::t(Palette::PH_FG),
    };
    if st == St::Comment {
        f.italics = true;
    }
    if st == St::PhChip || st == St::PhToken {
        f.background = crate::theme::t(Palette::PH_BG);
    }
    match sev {
        Some(Sev::Error) => {
            f.underline = Stroke::new(1.5_f32, crate::theme::t(Palette::ERR));
            f.background = crate::theme::t(Palette::ERR_BG);
            f.color = crate::theme::t(Palette::ERR);
        }
        Some(Sev::Warning) => {
            f.underline = Stroke::new(1.5_f32, crate::theme::t(Palette::WARN));
            f.background = crate::theme::t(Palette::WARN_BG);
        }
        None => {}
    }
    f
}

// ---------------------------------------------------------------- highlighter

struct KwCtx {
    norm: String,
    def: Option<&'static KeywordDef>,
    params: Vec<(String, Option<String>, (usize, usize))>,
    name_abs: (usize, usize),
    line: usize,
    unknown: bool,
}

pub struct Highlighter<'a> {
    text: &'a str,
    toks: Vec<Tok>,
    pending: Vec<Tok>,
    diags: Vec<Diag>,
    ext: &'a [Diag],
    ext_i: usize,
    kw: Option<KwCtx>,
    kw_cont: bool,
    data_kw: String,
}

pub struct HlResult {
    pub job: LayoutJob,
    pub diags: Vec<Diag>,
}

fn suggest_param(def: &KeywordDef, n: &str) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for p in &def.params {
        let d = util::edit_distance(n, &p.norm);
        if d <= (n.len() / 3).clamp(1, 3) && best.map(|b| d < b.0).unwrap_or(true) {
            best = Some((d, &p.name));
        }
    }
    best.map(|b| b.1.to_string())
}

fn new_hl<'a>(text: &'a str, ext: &'a [Diag], cap: usize) -> Highlighter<'a> {
    Highlighter { text, toks: Vec::with_capacity(cap), pending: Vec::new(), diags: Vec::new(), ext, ext_i: 0, kw: None, kw_cont: false, data_kw: String::new() }
}

/// Highlight the whole skeleton text. `ext` are diagnostics from the background analysis
/// (sorted by start offset) that belong to this exact text version.
pub fn highlight(text: &str, font: &FontId, ext: &[Diag]) -> HlResult {
    let mut h = new_hl(text, ext, text.len() / 24 + 16);
    for (li, s, e) in util::lines(text) {
        h.line(li, s, e);
    }
    h.finish_kw();
    let toks = std::mem::take(&mut h.toks);
    let mut job = LayoutJob { text: text.to_owned(), ..Default::default() };
    job.wrap.max_width = f32::INFINITY;
    job.sections = sections_for(&mut h, &toks, 0, text.len(), 0, font);
    HlResult { job, diags: h.diags }
}

/// The instant syntax checks over the whole text (no colouring); run in the background.
pub fn check(text: &str) -> Vec<Diag> {
    let mut h = new_hl(text, &[], 1024);
    for (li, s, e) in util::lines(text) {
        h.line(li, s, e);
        if h.toks.len() > 4096 && h.pending.is_empty() {
            h.toks.clear();
        }
    }
    h.finish_kw();
    h.diags
}

/// Layout sections for tokens inside [from, to) (byte offsets in the text); `base` is subtracted
/// from the offsets (0 for a whole-text job, the line start for a single-line job).
fn sections_for(h: &mut Highlighter, toks: &[Tok], from: usize, to: usize, base: usize, font: &FontId) -> Vec<LayoutSection> {
    let mut sections: Vec<LayoutSection> = Vec::new();
    let mut last_end = from;
    let mut prev: Option<(St, Option<Sev>)> = None;
    for t in toks {
        let mut t = *t;
        t.s = t.s.max(from);
        t.e = t.e.min(to);
        if t.s >= t.e {
            continue;
        }
        if t.s > last_end {
            push_sec(&mut sections, &mut prev, last_end - base, t.s - base, St::Plain, None, font);
        }
        if t.s < last_end {
            t.s = last_end;
            if t.s >= t.e {
                continue;
            }
        }
        let ext_sev = h.ext_overlap(t.s, t.e);
        let sev = match (t.sev, ext_sev) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        push_sec(&mut sections, &mut prev, t.s - base, t.e - base, t.st, sev, font);
        last_end = t.e;
    }
    if last_end < to {
        push_sec(&mut sections, &mut prev, last_end - base, to - base, St::Plain, None, font);
    }
    sections
}

/// Highlight only lines `first..=last` (one layout job per line) plus the live diagnostics of
/// those lines. Starts at the governing keyword line, so the result equals the whole-text pass.
pub fn highlight_lines(text: &str, starts: &[usize], first: usize, last: usize, font: &FontId, ext: &[Diag]) -> (Vec<LayoutJob>, Vec<Diag>) {
    let n = starts.len();
    if n == 0 || first >= n {
        return (Vec::new(), Vec::new());
    }
    let last = last.min(n - 1);
    let span = |l: usize| -> (usize, usize) {
        let s = starts[l];
        let e = if l + 1 < n { starts[l + 1] - 1 } else { text.len() };
        (s, e.max(s))
    };
    // governing keyword line at or above `first`
    let mut k = first;
    loop {
        let (s, e) = span(k);
        if line_kind(&text[s..e]) == LineKind::Keyword || k == 0 {
            break;
        }
        k -= 1;
    }
    let ext_from = ext.partition_point(|d| d.start < starts[k]);
    let mut h = new_hl(text, &ext[ext_from..], 256);
    let mut l = k;
    // the keyword line and its continuation lines
    if k < first {
        let (s, e) = span(k);
        h.line(k, s, e);
        l = k + 1;
        while h.kw_cont && l < first {
            let (s, e) = span(l);
            h.line(l, s, e);
            l += 1;
        }
        l = l.max(first);
    }
    while l <= last {
        let (s, e) = span(l);
        h.line(l, s, e);
        l += 1;
    }
    // complete a keyword whose continuation lines go past the window
    while h.kw_cont && l < n && l < last + 200 {
        let (s, e) = span(l);
        h.line(l, s, e);
        l += 1;
    }
    h.finish_kw();
    let toks = std::mem::take(&mut h.toks);
    let mut jobs = Vec::with_capacity(last + 1 - first);
    let mut ti = toks.partition_point(|t| t.e <= starts[first]);
    for li in first..=last {
        let (s, e) = span(li);
        let j0 = ti;
        while ti < toks.len() && toks[ti].s < e {
            ti += 1;
        }
        // a token may continue on the next line only if it spans the newline (not produced)
        let secs = sections_for(&mut h, &toks[j0..ti], s, e, s, font);
        let mut job = LayoutJob { text: text[s..e].to_string(), sections: secs, ..Default::default() };
        job.wrap.max_width = f32::INFINITY;
        jobs.push(job);
        if ti > j0 && toks[ti - 1].e > e {
            ti -= 1;
        }
    }
    (jobs, h.diags)
}

fn push_sec(
    secs: &mut Vec<LayoutSection>,
    prev: &mut Option<(St, Option<Sev>)>,
    s: usize,
    e: usize,
    st: St,
    sev: Option<Sev>,
    font: &FontId,
) {
    if s >= e {
        return;
    }
    if let (Some(last), Some(p)) = (secs.last_mut(), prev.as_ref()) {
        if p.0 == st && p.1 == sev && last.byte_range.end == s {
            last.byte_range.end = e;
            return;
        }
    }
    secs.push(LayoutSection { leading_space: 0.0, byte_range: s..e, format: fmt(st, sev, font) });
    *prev = Some((st, sev));
}

impl<'a> Highlighter<'a> {
    fn ext_overlap(&mut self, s: usize, e: usize) -> Option<Sev> {
        while self.ext_i < self.ext.len() && self.ext[self.ext_i].end <= s && self.ext[self.ext_i].start < s {
            self.ext_i += 1;
        }
        let mut best: Option<Sev> = None;
        let mut j = self.ext_i;
        while j < self.ext.len() && self.ext[j].start < e {
            let d = &self.ext[j];
            if d.end > s || (d.start == d.end && d.start >= s && d.start < e) {
                best = Some(best.map_or(d.sev, |b| b.min(d.sev)));
            }
            j += 1;
        }
        best
    }

    fn tok(&mut self, s: usize, e: usize, st: St) {
        if s < e {
            let st = if st == St::Data { St::Plain } else { st };
            let t = Tok { s, e, st, sev: None };
            if self.kw_cont || self.kw.is_some() && !self.pending.is_empty() {
                self.pending.push(t);
            } else {
                self.toks.push(t);
            }
        }
    }

    fn diag(&mut self, sev: Sev, msg: String, line: usize, s: usize, e: usize) {
        self.diags.push(Diag::new(sev, msg, line, s, e));
        // mark tokens overlapping
        for t in self.pending.iter_mut().chain(self.toks.iter_mut().rev().take(64)) {
            if t.s < e.max(s + 1) && t.e > s {
                t.sev = Some(t.sev.map_or(sev, |x| x.min(sev)));
            }
        }
    }

    fn flush_pending(&mut self) {
        let mut p = std::mem::take(&mut self.pending);
        p.sort_by_key(|t| t.s);
        self.toks.append(&mut p);
    }

    fn finish_kw(&mut self) {
        if let Some(k) = self.kw.take() {
            if let Some(def) = k.def {
                let have: Vec<&str> = k.params.iter().map(|p| p.0.as_str()).collect();
                let (ns, ne) = k.name_abs;
                for r in &def.required {
                    if !have.contains(&r.as_str()) {
                        let disp = def.param(r).map(|p| p.name.clone()).unwrap_or_else(|| r.clone());
                        self.diag(Sev::Error, format!("*{} requires the {} parameter", def.name, disp), k.line, ns, ne);
                    }
                }
                for g in &def.groups {
                    let present: Vec<&String> = g.params.iter().filter(|p| have.contains(&p.as_str())).collect();
                    match g.kind {
                        GroupKind::OneOf if present.is_empty() => self.diag(
                            Sev::Error,
                            format!("*{} requires one of: {}", def.name, g.display.join(", ")),
                            k.line,
                            ns,
                            ne,
                        ),
                        GroupKind::AtLeastOne if present.is_empty() => self.diag(
                            Sev::Error,
                            format!("*{} requires at least one of: {}", def.name, g.display.join(", ")),
                            k.line,
                            ns,
                            ne,
                        ),
                        GroupKind::OneOf | GroupKind::Exclusive if present.len() > 1 => {
                            let names: Vec<String> = present
                                .iter()
                                .map(|p| def.param(p).map(|d| d.name.clone()).unwrap_or_else(|| (*p).clone()))
                                .collect();
                            let span = k.params.iter().find(|p| &p.0 == present[1]).map(|p| p.2).unwrap_or((ns, ne));
                            self.diag(
                                Sev::Error,
                                format!("{} cannot be used together on *{}", names.join(" and "), def.name),
                                k.line,
                                span.0,
                                span.1,
                            );
                        }
                        _ => {}
                    }
                }
            }
            self.data_kw = k.norm;
        }
        self.flush_pending();
    }

    fn params_line(&mut self, li: usize, base: usize, line: &str, from: usize) {
        let (params, commas, cont, bad_quote) = parse_params(line, from);
        for c in commas {
            self.tok(base + c, base + c + 1, St::Punct);
        }
        self.param_toks(li, base, line, &params);
        if bad_quote {
            self.diag(Sev::Error, "Unbalanced double quote".into(), li, base + from, base + line.len());
        }
        self.kw_cont = cont;
    }

    fn param_toks(&mut self, li: usize, base: usize, line: &str, params: &[PTok]) {
        let def = self.kw.as_ref().and_then(|k| k.def);
        let unknown_kw = self.kw.as_ref().map(|k| k.unknown).unwrap_or(true);
        for p in params {
            self.tok(base + p.name.0, base + p.name.1, St::Param);
            if let Some(eq) = p.eq {
                self.tok(base + eq, base + eq + 1, St::Punct);
            }
            let val = p.value_str(line).map(|v| v.to_string());
            if let Some((vs, ve)) = p.value {
                let v = &line[vs..ve];
                let st = if v.starts_with('<') {
                    St::PRef
                } else if v.starts_with('"') {
                    St::Str
                } else {
                    St::Value
                };
                self.tok(base + vs, base + ve, st);
            }
            // abbreviated parameter names count as the full name (required / exclusive checks)
            let canon = def.and_then(|d| d.param(&p.norm)).map(|pd| pd.norm.clone()).unwrap_or_else(|| p.norm.clone());
            if let Some(k) = self.kw.as_mut() {
                k.params.push((canon, val.clone(), (base + p.name.0, base + p.name.1)));
            }
            if let Some(def) = def {
                match def.param(&p.norm) {
                    None if !unknown_kw => {
                        let hint = suggest_param(def, &p.norm).map(|s| format!(" (did you mean {s}?)")).unwrap_or_default();
                        self.diag(
                            Sev::Error,
                            format!("Unknown parameter {} for *{}{}", line[p.name.0..p.name.1].trim(), def.name, hint),
                            li,
                            base + p.name.0,
                            base + p.name.1,
                        );
                    }
                    Some(pd) if !pd.values_norm.is_empty() => {
                        if let (Some(v), Some((vs, ve))) = (&val, p.value) {
                            let vn = norm(v);
                            if !v.starts_with('<') && !v.starts_with('"') && !vn.is_empty() && !pd.values_norm.contains(&vn) && !util::is_number(v) {
                                self.diag(
                                    Sev::Warning,
                                    format!("{}={} is not a documented value (expected: {})", pd.name, v, pd.values.join(", ")),
                                    li,
                                    base + vs,
                                    base + ve,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn line(&mut self, li: usize, s: usize, e: usize) {
        let text = self.text;
        let line = &text[s..e];
        let kind = line_kind(line);
        // keyword continuation lines
        if self.kw_cont {
            if kind == LineKind::Data {
                self.params_line(li, s, line, 0);
                if !self.kw_cont {
                    self.finish_kw();
                }
                self.newline(e);
                return;
            }
            self.kw_cont = false;
            self.finish_kw();
        }
        if line.len() > 256 && kind != LineKind::Placeholder {
            // mark the overflow
            let cut = util::floor_char_boundary(line, 256);
            self.diags.push(Diag::new(Sev::Error, format!("Line has {} characters (Abaqus limit is 256)", line.chars().count()), li, s + cut, e));
        }
        match kind {
            LineKind::Blank => {}
            LineKind::Comment => self.tok(s, e, St::Comment),
            LineKind::Placeholder => {
                let (id, tl) = placeholder_id(line).unwrap_or((0, 0));
                self.tok(s, s + tl, St::PhToken);
                self.tok(s + tl, e, St::PhChip);
                if STORE.get(id).is_none() {
                    self.diag(Sev::Error, format!("Data block #{id} not found (placeholder line was edited)"), li, s, e);
                }
            }
            LineKind::Keyword => {
                self.finish_kw();
                let kp = parse_keyword(line).unwrap();
                let (ns, ne) = kp.name;
                let star = line.find('*').unwrap_or(0);
                let def = SCHEMA.get(&kp.norm);
                self.kw = Some(KwCtx {
                    norm: kp.norm.clone(),
                    def,
                    params: Vec::new(),
                    name_abs: (s + star, s + ne.max(star + 1)),
                    line: li,
                    unknown: def.is_none(),
                });
                // tokens go to pending until the logical keyword line is complete
                self.pending.push(Tok { s: s + star, e: s + ne.max(star + 1), st: St::Keyword, sev: None });
                if kp.norm.is_empty() {
                    self.diag(Sev::Error, "Keyword name missing after *".into(), li, s + star, s + star + 1);
                } else if def.is_none() {
                    let hint = SCHEMA.suggest(&kp.norm).map(|k| format!(" (did you mean *{}?)", k.name)).unwrap_or_default();
                    self.diag(Sev::Error, format!("Unknown keyword *{}{}", line[ns..ne].trim(), hint), li, s + star, s + ne);
                }
                for &c in &kp.commas {
                    self.pending.push(Tok { s: s + c, e: s + c + 1, st: St::Punct, sev: None });
                }
                let params = kp.params.clone();
                self.kw_cont = kp.cont;
                self.param_toks(li, s, line, &params);
                if kp.bad_quote {
                    self.diag(Sev::Error, "Unbalanced double quote".into(), li, s, e);
                }
                if !self.kw_cont {
                    self.finish_kw();
                }
                self.pending.sort_by_key(|t| t.s);
                self.toks.extend(self.pending.drain(..).collect::<Vec<_>>());
            }
            LineKind::Data => self.data_line(li, s, line),
        }
        self.newline(e);
    }

    fn newline(&mut self, _e: usize) {}

    fn data_line(&mut self, li: usize, s: usize, line: &str) {
        let kw = self.data_kw.as_str();
        let check_node = kw == "NODE";
        let check_elem = kw == "ELEMENT";
        let quick_plain = !line.contains(['<', '"']) && line.bytes().all(|c| c.is_ascii_digit() || matches!(c, b' ' | b',' | b'.' | b'-' | b'+' | b'e' | b'E' | b'\t' | b'd' | b'D'));
        if quick_plain && !check_node && !check_elem {
            self.tok(s, s + line.len(), St::Data);
            return;
        }
        let fields = util::split_fields(line);
        let mut errs: Vec<(usize, usize, String)> = Vec::new();
        let mut toks: Vec<(usize, usize, St)> = Vec::new();
        for (fi, &(fs, fe)) in fields.iter().enumerate() {
            let f = &line[fs..fe];
            if f.is_empty() {
                continue;
            }
            let st = if f.starts_with('<') {
                St::PRef
            } else if f.starts_with('"') {
                St::Str
            } else if util::is_number(f) {
                St::Data
            } else {
                St::Name
            };
            toks.push((fs, fe, st));
            if st == St::PRef {
                continue;
            }
            if check_node {
                if fi == 0 && !util::is_integer(f) {
                    errs.push((fs, fe, format!("Node label must be an integer, got '{f}'")));
                } else if fi > 0 && !util::is_number(f) {
                    errs.push((fs, fe, format!("Invalid number '{f}' in *NODE data")));
                }
            } else if check_elem && !util::is_integer(f) {
                errs.push((fs, fe, format!("Element data must be integers, got '{f}'")));
            }
        }
        let has_names = toks.iter().any(|t| t.2 != St::Data);
        if !has_names && errs.is_empty() {
            self.tok(s, s + line.len(), St::Data);
            return;
        }
        let mut last = 0;
        for (fs, fe, st) in toks {
            if fs > last {
                self.tok(s + last, s + fs, St::Data);
            }
            self.tok(s + fs, s + fe, st);
            last = fe;
        }
        if last < line.len() {
            self.tok(s + last, s + line.len(), St::Data);
        }
        for (fs, fe, m) in errs {
            self.diag(Sev::Error, m, li, s + fs, s + fe);
        }
    }
}

/// Find the keyword governing a given line: returns (keyword line index, parse) by scanning upward.
pub fn keyword_context(text: &str, starts: &[usize], line: usize) -> Option<(usize, String, KwParse)> {
    let mut l = line as isize;
    while l >= 0 {
        let s = starts[l as usize];
        let e = starts.get(l as usize + 1).map(|x| x - 1).unwrap_or(text.len());
        let t = &text[s..e.max(s)];
        if line_kind(t) == LineKind::Keyword {
            return parse_keyword(t).map(|k| (l as usize, t.to_string(), k));
        }
        l -= 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn diags(t: &str) -> Vec<Diag> {
        highlight(t, &FontId::monospace(12.0), &[]).diags
    }
    #[test]
    fn checks() {
        let d = diags("*Elastik\n200000., 0.3\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].msg.contains("ELASTIC"), "{}", d[0].msg);
        let d = diags("*Solid Section, elset=A, materal=STEEL\n,\n");
        assert!(d.iter().any(|x| x.msg.contains("MATERIAL")), "{d:?}");
        let d = diags("*Solid Section, material=STEEL\n,\n");
        assert!(d.iter().any(|x| x.msg.contains("requires the ELSET")), "{d:?}");
        let d = diags("*Solid Section, material=STEEL,\n elset=A\n,\n");
        assert!(d.is_empty(), "{d:?}");
        let d = diags("*Node\n1, 0., 0., 0.\n2a, 1., x, 0.\n");
        assert_eq!(d.len(), 2, "{d:?}");
        let d = diags("*Step, name=S1, nlgeom=MAYBE\n*Static\n*End Step\n");
        assert!(d.iter().any(|x| x.sev == Sev::Warning), "{d:?}");
        let d = diags("*Heading\n** comment\n*Part, name=P\n*End Part\n*Output, field\n*Node Output\nU, RF\n");
        assert!(d.is_empty(), "{d:?}");
        // undocumented abbreviation accepted by Abaqus
        let d = diags("*Output, field, var=preselect\n*Output, history, VAR=PRESELECT\n");
        assert!(d.is_empty(), "{d:?}");
        let d = diags("*Output, field, va=preselect\n");
        assert_eq!(d.len(), 1, "{d:?}");
    }
    #[test]
    fn job_sections_cover() {
        let t = "*Node, nset=A\n1, 2., 3., 4.\n** c\n*Elastic\n1, 2\n";
        let r = highlight(t, &FontId::monospace(12.0), &[]);
        let mut end = 0;
        for s in &r.job.sections {
            assert_eq!(s.byte_range.start, end);
            end = s.byte_range.end;
        }
        assert_eq!(end, t.len());
    }
}
