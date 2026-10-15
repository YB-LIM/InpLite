//! Memory-mapped sources and the global store of "bulk" data blocks.
//!
//! Large data blocks (node/element tables, big sets, ...) never enter the editable text.
//! They stay on disk, are memory-mapped, and are represented in the editor by a single
//! placeholder comment line:  `**@@BULK <id>@@ *Node data: 1,234,567 lines, 48.2 MB`
//! On save, each placeholder line is replaced by the original bytes.

use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

pub struct Source {
    pub path: PathBuf,
    map: Option<Mmap>,
    pub len: u64,
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Arc<Source>> {
        let f = File::open(path)?;
        let len = f.metadata()?.len();
        let map = if len == 0 {
            None
        } else {
            // SAFETY: the file is opened read-only; InpLite never writes to a file it has mapped
            // (saves go to a temporary file which is then renamed into place).
            Some(unsafe { Mmap::map(&f)? })
        };
        Ok(Arc::new(Source { path: path.to_path_buf(), map, len }))
    }
    pub fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }
}

/// Every SPARSE-th line offset of a bulk block is remembered for random access.
pub const SPARSE: u64 = 256;

pub struct Bulk {
    pub id: u64,
    pub src: Arc<Source>,
    /// Byte range in `src`; excludes the line terminator of the last line.
    pub start: usize,
    pub end: usize,
    pub lines: u64,
    /// Keyword name as written, e.g. "*Node"
    pub kw_disp: String,
    /// Normalized keyword name, e.g. "NODE"
    pub kw_norm: String,
    /// The full (first) keyword line, e.g. "*Element, type=C3D8R, elset=ALL"
    pub kw_line: String,
    pub sparse: OnceLock<Arc<Vec<u64>>>,
    sparse_job: AtomicBool,
}

impl Bulk {
    #[allow(clippy::too_many_arguments)]
    pub fn new(id: u64, src: Arc<Source>, start: usize, end: usize, lines: u64, kw_disp: String, kw_norm: String, kw_line: String) -> Bulk {
        Bulk { id, src, start, end, lines, kw_disp, kw_norm, kw_line, sparse: OnceLock::new(), sparse_job: AtomicBool::new(false) }
    }
    /// Same block at a new location (after saving), keeping the line index.
    pub fn relocated(&self, src: Arc<Source>, start: usize, end: usize) -> Bulk {
        let b = Bulk::new(self.id, src, start, end, self.lines, self.kw_disp.clone(), self.kw_norm.clone(), self.kw_line.clone());
        if let Some(sp) = self.sparse.get() {
            let _ = b.sparse.set(sp.clone());
        }
        b
    }
    pub fn sparse_ready(&self) -> bool {
        self.sparse.get().is_some()
    }
    /// Start building the line index in a background thread (never blocks the UI). Returns true if ready.
    pub fn ensure_sparse(self: &Arc<Self>) -> bool {
        if self.sparse_ready() {
            return true;
        }
        if !self.sparse_job.swap(true, Ordering::AcqRel) {
            let me = self.clone();
            std::thread::spawn(move || {
                let sp = build_sparse(me.bytes());
                let _ = me.sparse.set(Arc::new(sp));
            });
        }
        false
    }
    pub fn bytes(&self) -> &[u8] {
        &self.src.bytes()[self.start..self.end]
    }
    pub fn size(&self) -> u64 {
        (self.end - self.start) as u64
    }
    /// Line index, building it if needed. Blocks: only call from background threads.
    pub fn sparse_blocking(&self) -> Arc<Vec<u64>> {
        self.sparse.get_or_init(|| Arc::new(build_sparse(self.bytes()))).clone()
    }
    /// Byte range (relative to block start) of physical line `row`.
    /// Never blocks: without the line index only the first few thousand lines are reachable.
    pub fn line_range(&self, row: u64) -> Option<(usize, usize)> {
        if row >= self.lines {
            return None;
        }
        let b = self.bytes();
        let (mut pos, mut r) = match self.sparse.get() {
            Some(sp) => {
                let k = (row / SPARSE) as usize;
                (*sp.get(k)? as usize, k as u64 * SPARSE)
            }
            None if row < 4096 => (0, 0),
            None => return None,
        };
        while r < row {
            match memchr::memchr(b'\n', &b[pos..]) {
                Some(i) => pos += i + 1,
                None => return None,
            }
            r += 1;
        }
        let end = memchr::memchr(b'\n', &b[pos..]).map(|i| pos + i).unwrap_or(b.len());
        let mut e = end;
        if e > pos && b[e - 1] == b'\r' {
            e -= 1;
        }
        Some((pos, e))
    }
    pub fn line(&self, row: u64) -> Option<&[u8]> {
        self.line_range(row).map(|(s, e)| &self.bytes()[s..e])
    }
    pub fn placeholder(&self) -> String {
        placeholder_text(self.id, &self.kw_disp, self.lines, self.size())
    }
}

pub fn placeholder_text(id: u64, kw: &str, lines: u64, size: u64) -> String {
    format!(
        "{PH_PREFIX}{id}@@ {kw} data: {} lines, {}",
        crate::util::fmt_int(lines),
        crate::util::fmt_bytes(size)
    )
}

pub const PH_PREFIX: &str = "**@@BULK ";

/// If `line` is a placeholder line, return its bulk id and the byte length of the "**@@BULK n@@" token.
pub fn placeholder_id(line: &str) -> Option<(u64, usize)> {
    let rest = line.strip_prefix(PH_PREFIX)?;
    let end = rest.find("@@")?;
    let id = rest[..end].parse().ok()?;
    Some((id, PH_PREFIX.len() + end + 2))
}

pub fn build_sparse(b: &[u8]) -> Vec<u64> {
    let mut v = Vec::with_capacity(b.len() / (SPARSE as usize * 30) + 2);
    v.push(0);
    let mut n: u64 = 0;
    for i in memchr::memchr_iter(b'\n', b) {
        n += 1;
        if n % SPARSE == 0 {
            v.push(i as u64 + 1);
        }
    }
    v
}

pub struct Store {
    map: RwLock<HashMap<u64, Arc<Bulk>>>,
    next: AtomicU64,
}

pub static STORE: LazyLock<Store> =
    LazyLock::new(|| Store { map: RwLock::new(HashMap::new()), next: AtomicU64::new(1) });

impl Store {
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    pub fn insert(&self, b: Bulk) -> Arc<Bulk> {
        let a = Arc::new(b);
        self.map.write().unwrap().insert(a.id, a.clone());
        a
    }
    pub fn get(&self, id: u64) -> Option<Arc<Bulk>> {
        self.map.read().unwrap().get(&id).cloned()
    }
    pub fn replace(&self, b: Arc<Bulk>) {
        self.map.write().unwrap().insert(b.id, b);
    }
}
