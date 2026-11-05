//! Small helpers shared across modules.

/// 1234567 -> "1,234,567"
pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Human readable byte size.
pub fn fmt_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / K / K)
    } else {
        format!("{:.2} GB", f / K / K / K)
    }
}

/// Uppercase and drop blanks: Abaqus ignores blanks in keyword and parameter names.
pub fn norm(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if c != ' ' && c != '\t' {
            o.push(c.to_ascii_uppercase());
        }
    }
    o
}

/// Normalize a label (set/surface/material name): strip quotes, trim, uppercase.
/// Quoted labels keep inner blanks; unquoted labels drop surrounding blanks.
pub fn norm_label(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(t);
    t.trim().to_ascii_uppercase()
}

/// Iterate lines of a str: yields (line_index, byte_start, byte_end_excluding_newline).
pub struct LineIter<'a> {
    s: &'a str,
    pos: usize,
    idx: usize,
    done: bool,
}

pub fn lines(s: &str) -> LineIter<'_> {
    LineIter { s, pos: 0, idx: 0, done: false }
}

impl Iterator for LineIter<'_> {
    type Item = (usize, usize, usize);
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let b = self.s.as_bytes();
        let start = self.pos;
        match memchr::memchr(b'\n', &b[start..]) {
            Some(i) => {
                let end = start + i;
                self.pos = end + 1;
                let idx = self.idx;
                self.idx += 1;
                Some((idx, start, end))
            }
            None => {
                self.done = true;
                let idx = self.idx;
                self.idx += 1;
                Some((idx, start, b.len()))
            }
        }
    }
}

/// Byte offsets of the start of every line.
pub fn line_starts(s: &str) -> Vec<usize> {
    let mut v = Vec::with_capacity(s.len() / 40 + 1);
    v.push(0);
    for i in memchr::memchr_iter(b'\n', s.as_bytes()) {
        v.push(i + 1);
    }
    v
}

/// Line index containing byte offset `off` (binary search over line starts).
pub fn line_of(starts: &[usize], off: usize) -> usize {
    match starts.binary_search(&off) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

pub fn char_to_byte(s: &str, ascii: bool, ci: usize) -> usize {
    if ascii {
        ci.min(s.len())
    } else {
        s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
    }
}

pub fn byte_to_char(s: &str, ascii: bool, bi: usize) -> usize {
    if ascii {
        bi.min(s.len())
    } else {
        let bi = bi.min(s.len());
        s[..floor_char_boundary(s, bi)].chars().count()
    }
}

pub fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Is this field an Abaqus-style number? Accepts Fortran-ish forms: 1, -1., .5, 1.e5, 1.0D-3, 1.0-3
pub fn is_number(f: &str) -> bool {
    let b = f.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    if b[i] == b'+' || b[i] == b'-' {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - ds;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - fs;
    }
    if digits == 0 {
        return false;
    }
    if i == b.len() {
        return true;
    }
    // exponent
    if matches!(b[i], b'e' | b'E' | b'd' | b'D') {
        i += 1;
    } else if !(b[i] == b'+' || b[i] == b'-') {
        return false;
    }
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let es = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i == b.len() && i > es
}

pub fn is_integer(f: &str) -> bool {
    let b = f.as_bytes();
    let s = if !b.is_empty() && (b[0] == b'+' || b[0] == b'-') { 1 } else { 0 };
    b.len() > s && b[s..].iter().all(|c| c.is_ascii_digit())
}

/// Parse a leading unsigned integer from bytes (skipping blanks). Returns None for empty/invalid.
pub fn parse_u64(b: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let s = i;
    let mut v: u64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v.checked_mul(10)?.checked_add((b[i] - b'0') as u64)?;
        i += 1;
    }
    if i == s {
        return None;
    }
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t' || b[i] == b'\r') {
        i += 1;
    }
    if i != b.len() {
        return None;
    }
    Some(v)
}

/// Levenshtein distance for short ASCII strings.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let c = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + c);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Fast non-cryptographic 64-bit hash (FNV-1a variant over bytes).
#[derive(Clone, Copy)]
pub struct Fnv(pub u64);
impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf29ce484222325)
    }
}
impl Fnv {
    #[inline]
    pub fn byte(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(0x100000001b3);
    }
    pub fn bytes(&mut self, bs: &[u8]) {
        for &b in bs {
            self.byte(b);
        }
    }
}

/// Split a line into comma separated fields, honoring double quotes. Returns trimmed byte spans.
pub fn split_fields(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut inq = false;
    for (i, &c) in b.iter().enumerate() {
        if c == b'"' {
            inq = !inq;
        } else if c == b',' && !inq {
            out.push(trim_span(b, start, i));
            start = i + 1;
        }
    }
    out.push(trim_span(b, start, b.len()));
    out
}

pub fn trim_span(b: &[u8], mut s: usize, mut e: usize) -> (usize, usize) {
    while s < e && (b[s] == b' ' || b[s] == b'\t' || b[s] == b'\r') {
        s += 1;
    }
    while e > s && (b[e - 1] == b' ' || b[e - 1] == b'\t' || b[e - 1] == b'\r') {
        e -= 1;
    }
    (s, e)
}

/// Title-case a keyword name the way Abaqus/CAE writes it: "SOLID SECTION" -> "Solid Section".
pub fn title_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut new_word = true;
    for c in name.chars() {
        if c == ' ' {
            new_word = true;
            out.push(c);
        } else if new_word {
            out.push(c.to_ascii_uppercase());
            new_word = false;
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Folder for this session's temporary files (data blocks re-written after commenting, re-reads
/// with a new data-block threshold, …). Data blocks stay memory-mapped from these files, so they
/// are only removed by `cleanup_temp` at a later start.
pub fn temp_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("InpLite-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A new, unique temporary file name in `temp_dir`.
pub fn temp_file(tag: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    temp_dir().join(format!("{tag}-{n}.tmp"))
}

/// Remove temporary folders of earlier sessions (older than a day; best effort).
pub fn cleanup_temp() {
    let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let own = format!("InpLite-{}", std::process::id());
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("InpLite-") || name == own || !e.path().is_dir() {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 86_400);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers() {
        for s in ["1", "-1.", ".5", "1.e5", "1.0D-3", "1.0-3", "+2.5E+04", "0."] {
            assert!(is_number(s), "{s}");
        }
        for s in ["", "1.2.3", "abc", "1e", "-", "1,2", "e5"] {
            assert!(!is_number(s), "{s}");
        }
    }
    #[test]
    fn ints() {
        assert_eq!(fmt_int(1234567), "1,234,567");
        assert_eq!(fmt_int(12), "12");
        assert_eq!(parse_u64(b" 123 "), Some(123));
        assert_eq!(parse_u64(b"12a"), None);
    }
    #[test]
    fn fields() {
        let l = r#"a, "b, c" ,d"#;
        let f: Vec<&str> = split_fields(l).iter().map(|&(s, e)| &l[s..e]).collect();
        assert_eq!(f, vec!["a", "\"b, c\"", "d"]);
    }
}
