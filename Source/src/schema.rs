//! Keyword schema derived from the Abaqus Keywords Guide (see tools/extract_schema.py).

use crate::util::{edit_distance, norm};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Deserialize)]
struct RawSchema {
    src: String,
    k: Vec<RawKw>,
}

#[derive(Deserialize)]
struct RawKw {
    n: String,
    s: String,
    pr: Vec<String>,
    t: String,
    l: Vec<String>,
    pg: u32,
    r: Vec<String>,
    g: Vec<RawGroup>,
    p: HashMap<String, RawParam>,
    #[serde(default)]
    u: Vec<String>,
}

#[derive(Deserialize)]
struct RawGroup {
    kind: String,
    params: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawParam {
    #[serde(default)]
    v: Vec<String>,
    #[serde(default)]
    f: u8,
    #[serde(default)]
    d: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KwType {
    Model,
    History,
    Both,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupKind {
    OneOf,
    AtLeastOne,
    Exclusive,
}

pub struct Group {
    pub kind: GroupKind,
    pub params: Vec<String>, // normalized
    pub display: Vec<String>,
}

pub struct ParamDef {
    pub name: String,
    pub norm: String,
    pub values: Vec<String>,      // display form
    pub values_norm: Vec<String>, // normalized
    pub flag: bool,
    pub doc: String,
}

pub struct KeywordDef {
    pub name: String,
    pub norm: String,
    pub summary: String,
    pub standard: bool,
    pub explicit: bool,
    pub products: Vec<String>,
    pub kind: KwType,
    pub level: Vec<String>,
    pub page: u32,
    pub required: Vec<String>, // normalized
    pub groups: Vec<Group>,
    pub params: Vec<ParamDef>,
    pmap: HashMap<String, usize>,
    pub usecases: Vec<String>,
}

impl KeywordDef {
    /// Parameter by normalised name. Abaqus also accepts abbreviations that the Keywords Guide
    /// does not list (e.g. VAR=PRESELECT for VARIABLE=PRESELECT on *OUTPUT), so a unique prefix
    /// of at least 3 characters is accepted as that parameter.
    pub fn param(&self, norm: &str) -> Option<&ParamDef> {
        if let Some(&i) = self.pmap.get(norm) {
            return Some(&self.params[i]);
        }
        if norm.len() >= 3 {
            let mut it = self.params.iter().filter(|p| p.norm.starts_with(norm));
            let first = it.next()?;
            if it.all(|p| p.norm == first.norm) {
                return Some(first);
            }
        }
        None
    }
    /// History data that may only appear inside *STEP.
    pub fn step_only(&self) -> bool {
        self.kind == KwType::History && self.level.len() == 1 && self.level[0] == "Step"
    }
    /// Model data that may not appear inside *STEP.
    pub fn model_only(&self) -> bool {
        self.kind == KwType::Model && !self.level.iter().any(|l| l.contains("Step"))
    }
    pub fn level_has(&self, what: &str) -> bool {
        self.level.iter().any(|l| l == what)
    }
    pub fn display_name(&self) -> String {
        crate::util::title_case(&self.name)
    }
    pub fn product_label(&self) -> String {
        let mut v = Vec::new();
        if self.standard {
            v.push("Standard");
        }
        if self.explicit {
            v.push("Explicit");
        }
        for p in &self.products {
            if p != "S" && p != "E" {
                v.push(p.as_str());
            }
        }
        if v.is_empty() {
            "-".into()
        } else {
            v.join(", ")
        }
    }
    pub fn type_label(&self) -> &'static str {
        match self.kind {
            KwType::Model => "Model data",
            KwType::History => "History data",
            KwType::Both => "Model or history data",
            KwType::Unknown => "-",
        }
    }
}

pub struct Schema {
    pub source: String,
    pub kws: Vec<KeywordDef>,
    map: HashMap<String, usize>,
}

pub static SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::load);

const RAW: &str = include_str!("../assets/keywords.json");

impl Schema {
    fn load() -> Schema {
        let raw: RawSchema = serde_json::from_str(RAW).expect("embedded keyword schema");
        let mut kws = Vec::with_capacity(raw.k.len());
        for k in raw.k {
            let mut params = Vec::new();
            let mut pmap = HashMap::new();
            let mut names: Vec<_> = k.p.into_iter().collect();
            names.sort_by(|a, b| a.0.cmp(&b.0));
            for (pname, p) in names {
                let pn = norm(&pname);
                pmap.insert(pn.clone(), params.len());
                params.push(ParamDef {
                    values_norm: p.v.iter().map(|v| norm(v)).collect(),
                    values: p.v,
                    flag: p.f != 0,
                    doc: p.d,
                    norm: pn,
                    name: pname,
                });
            }
            let groups = k
                .g
                .into_iter()
                .map(|g| Group {
                    kind: match g.kind.as_str() {
                        "one_of" => GroupKind::OneOf,
                        "at_least_one" => GroupKind::AtLeastOne,
                        _ => GroupKind::Exclusive,
                    },
                    params: g.params.iter().map(|p| norm(p)).collect(),
                    display: g.params,
                })
                .collect();
            kws.push(KeywordDef {
                norm: norm(&k.n),
                standard: k.pr.iter().any(|p| p == "S"),
                explicit: k.pr.iter().any(|p| p == "E"),
                products: k.pr,
                kind: match k.t.as_str() {
                    "model" => KwType::Model,
                    "history" => KwType::History,
                    "both" => KwType::Both,
                    _ => KwType::Unknown,
                },
                level: k.l,
                page: k.pg,
                required: k.r.iter().map(|p| norm(p)).collect(),
                groups,
                params,
                pmap,
                usecases: k.u,
                summary: k.s,
                name: k.n,
            });
        }
        let map = kws.iter().enumerate().map(|(i, k)| (k.norm.clone(), i)).collect();
        Schema { source: raw.src, kws, map }
    }

    pub fn get(&self, norm: &str) -> Option<&KeywordDef> {
        self.map.get(norm).map(|&i| &self.kws[i])
    }

    /// Closest known keyword for an unknown one (typo suggestion).
    pub fn suggest(&self, unknown_norm: &str) -> Option<&KeywordDef> {
        let mut best: Option<(usize, &KeywordDef)> = None;
        for k in &self.kws {
            let d = edit_distance(unknown_norm, &k.norm);
            let limit = (unknown_norm.len() / 4).clamp(1, 3);
            if d <= limit && best.map(|b| d < b.0).unwrap_or(true) {
                best = Some((d, k));
            }
        }
        best.map(|b| b.1)
    }

    /// Keywords whose normalized name starts with (or contains) the prefix, ranked.
    pub fn complete(&self, prefix_norm: &str) -> Vec<&KeywordDef> {
        let mut starts: Vec<&KeywordDef> = Vec::new();
        let mut contains: Vec<&KeywordDef> = Vec::new();
        for k in &self.kws {
            if k.norm.starts_with(prefix_norm) {
                starts.push(k);
            } else if prefix_norm.len() >= 2 && k.norm.contains(prefix_norm) {
                contains.push(k);
            }
        }
        let rank = |k: &KeywordDef| -> (u8, usize, String) {
            (if COMMON.contains(&k.norm.as_str()) { 0 } else { 1 }, k.norm.len(), k.norm.clone())
        };
        starts.sort_by_key(|k| rank(k));
        contains.sort_by_key(|k| rank(k));
        starts.extend(contains);
        starts
    }
}

/// Frequently used keywords get ranked first in completion.
const COMMON: &[&str] = &[
    "NODE", "ELEMENT", "NSET", "ELSET", "SURFACE", "PART", "ENDPART", "ASSEMBLY", "ENDASSEMBLY",
    "INSTANCE", "ENDINSTANCE", "MATERIAL", "ELASTIC", "PLASTIC", "DENSITY", "EXPANSION",
    "CONDUCTIVITY", "SPECIFICHEAT", "SOLIDSECTION", "SHELLSECTION", "STEP", "ENDSTEP", "STATIC",
    "DYNAMIC", "BOUNDARY", "CLOAD", "DLOAD", "DSLOAD", "OUTPUT", "NODEOUTPUT", "ELEMENTOUTPUT",
    "CONTACTOUTPUT", "RESTART", "AMPLITUDE", "CONTACTPAIR", "CONTACT", "SURFACEINTERACTION",
    "FRICTION", "TIE", "COUPLING", "KINEMATIC", "INCLUDE", "PARAMETER", "HEADING", "PREPRINT",
    "INITIALCONDITIONS", "ORIENTATION", "HEATTRANSFER", "FREQUENCY", "TEMPERATURE",
];

/// Number of nodes for common Abaqus element types (None = unknown/variable).
pub fn element_nodes(etype: &str) -> Option<u32> {
    let t = norm(etype);
    let t = t.as_str();
    let fixed: &[(&str, u32)] = &[
        ("B21", 2), ("B21H", 2), ("B22", 3), ("B22H", 3), ("B23", 2), ("B23H", 2), ("B31", 2),
        ("B31H", 2), ("B31OS", 2), ("B31OSH", 2), ("B32", 3), ("B32H", 3), ("B32OS", 3),
        ("B32OSH", 3), ("B33", 2), ("B33H", 2), ("PIPE21", 2), ("PIPE22", 3), ("PIPE31", 2),
        ("PIPE32", 3), ("PIPE21H", 2), ("PIPE22H", 3), ("PIPE31H", 2), ("PIPE32H", 3),
        ("T2D2", 2), ("T2D3", 3), ("T3D2", 2), ("T3D3", 3), ("T2D2H", 2), ("T3D2H", 2),
        ("T3D2T", 2), ("STRI3", 3), ("STRI65", 6), ("S4", 4), ("S4R", 4), ("S4R5", 4),
        ("S4RS", 4), ("S4RSW", 4), ("S4T", 4), ("S4RT", 4), ("S3", 3), ("S3R", 3), ("S3RS", 3),
        ("S3T", 3), ("S3RT", 3), ("S8R", 8), ("S8R5", 8), ("S8RT", 8), ("S9R5", 9), ("SC6R", 6),
        ("SC8R", 8), ("SC6RT", 6), ("SC8RT", 8), ("CONN3D2", 2), ("CONN2D2", 2), ("SPRINGA", 2),
        ("SPRING1", 1), ("SPRING2", 2), ("DASHPOTA", 2), ("DASHPOT1", 1), ("DASHPOT2", 2),
        ("MASS", 1), ("ROTARYI", 1), ("HEATCAP", 1), ("GAPUNI", 2), ("GAPCYL", 2),
        ("GAPSPHER", 2), ("R2D2", 2), ("RB2D2", 2), ("RB3D2", 2), ("RAX2", 2), ("R3D3", 3),
        ("R3D4", 4), ("SFM3D3", 3), ("SFM3D4", 4), ("SFM3D4R", 4), ("SFM3D6", 6), ("SFM3D8", 8),
        ("SFM3D8R", 8), ("DS3", 3), ("DS4", 4), ("DS6", 6), ("DS8", 8), ("PC3D", 1),
    ];
    for (n, c) in fixed {
        if *n == t {
            return Some(*c);
        }
    }
    // Continuum families: [D|AC|DCC|EC|Q]C3D8R, CPE4, CPS8R, CAX4, CGAX8, COH3D8, CIN3D8, GK3D8, M3D4, ...
    let prefixes = [
        "DCC3D", "DCCAX", "AC3D", "ACAX", "AC2D", "DC3D", "DC2D", "DCAX", "EC3D", "Q3D", "C3D",
        "CPEG", "CPE", "CPS", "CGAX", "CAX", "COH3D", "COH2D", "COHAX", "CIN3D", "CINPE", "CINPS",
        "CINAX", "GK3D", "GKPE", "GKPS", "GKAX", "M3D", "MAX", "MGAX", "SAX", "DCOUP3D",
    ];
    for p in prefixes {
        if let Some(rest) = t.strip_prefix(p) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(v) = digits.parse::<u32>() {
                    if (1..=27).contains(&v) {
                        return Some(v);
                    }
                }
            }
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_loads() {
        let s = &*SCHEMA;
        assert!(s.kws.len() > 500);
        let k = s.get("SOLIDSECTION").unwrap();
        assert!(k.param("ELSET").is_some());
        assert!(k.required.contains(&"ELSET".to_string()));
        assert!(s.get("ENDSTEP").is_some());
        assert_eq!(s.suggest("ELASTIK").unwrap().norm, "ELASTIC");
    }
    #[test]
    fn elem_nodes() {
        assert_eq!(element_nodes("C3D8R"), Some(8));
        assert_eq!(element_nodes("c3d10m"), Some(10));
        assert_eq!(element_nodes("S4R"), Some(4));
        assert_eq!(element_nodes("CPE4R"), Some(4));
        assert_eq!(element_nodes("B32"), Some(3));
        assert_eq!(element_nodes("U1"), None);
    }
}
