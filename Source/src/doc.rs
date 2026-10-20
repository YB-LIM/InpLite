//! Document model: editable skeleton text + references to bulk blocks; safe saving.

use crate::load::{Eol, Loaded, Progress};
use crate::store::{placeholder_id, Bulk, Source, STORE};
use crate::util;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// One reversible text change: at byte `start`, `removed` was replaced by `inserted`.
#[derive(Clone, Debug)]
pub struct Change {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    /// typing changes merge with the following keystrokes into one undo step
    typing: bool,
    time: f64,
}

/// The editable text: a String with a version counter and an edit-based undo history
/// (memory proportional to what was changed, not to the size of the text).
pub struct VText {
    pub s: String,
    pub ver: u64,
    pub ascii: bool,
    undo: Vec<Change>,
    redo: Vec<Change>,
    undo_bytes: usize,
}

/// Undo history limit (bytes of removed + inserted text).
const UNDO_BYTES: usize = 192 << 20;
const UNDO_STEPS: usize = 2000;

impl VText {
    pub fn new(s: String) -> Self {
        let ascii = s.is_ascii();
        VText { s, ver: 1, ascii, undo: Vec::new(), redo: Vec::new(), undo_bytes: 0 }
    }
    /// Replace the whole text (one undo step).
    pub fn set(&mut self, s: String) {
        let old = std::mem::take(&mut self.s);
        self.ascii = s.is_ascii();
        self.s = s;
        self.ver += 1;
        let ins = self.s.clone();
        self.record(Change { start: 0, removed: old, inserted: ins, typing: false, time: 0.0 });
    }
    /// Replace a byte range (one undo step).
    pub fn replace_range(&mut self, r: std::ops::Range<usize>, with: &str) {
        self.change(r, with, false, 0.0);
    }
    /// Replace a byte range from typing in the editor: consecutive keystrokes become one undo step.
    pub fn type_range(&mut self, r: std::ops::Range<usize>, with: &str, now: f64) {
        self.change(r, with, true, now);
    }
    fn change(&mut self, r: std::ops::Range<usize>, with: &str, typing: bool, now: f64) {
        let r = r.start.min(self.s.len())..r.end.min(self.s.len());
        let r = r.start.min(r.end)..r.end;
        if r.is_empty() && with.is_empty() {
            return;
        }
        if !with.is_ascii() {
            self.ascii = false;
        }
        let removed = self.s[r.clone()].to_string();
        self.s.replace_range(r.clone(), with);
        self.ver += 1;
        self.record(Change { start: r.start, removed, inserted: with.to_string(), typing, time: now });
    }
    fn record(&mut self, c: Change) {
        self.redo.clear();
        if c.typing && !c.inserted.contains('\n') {
            if let Some(last) = self.undo.last_mut() {
                if last.typing && c.time - last.time < 1.5 {
                    let last_end = last.start + last.inserted.len();
                    // keep typing
                    if c.removed.is_empty() && c.start == last_end && !last.inserted.ends_with('\n') {
                        self.undo_bytes += c.inserted.len();
                        last.inserted.push_str(&c.inserted);
                        last.time = c.time;
                        return;
                    }
                    // backspace over what was just typed
                    if c.inserted.is_empty() && c.start + c.removed.len() == last_end && c.start >= last.start {
                        last.inserted.truncate(c.start - last.start);
                        last.time = c.time;
                        return;
                    }
                    // more backspaces / deletes
                    if c.inserted.is_empty() && last.inserted.is_empty() {
                        if c.start + c.removed.len() == last.start {
                            self.undo_bytes += c.removed.len();
                            last.removed.insert_str(0, &c.removed);
                            last.start = c.start;
                            last.time = c.time;
                            return;
                        }
                        if c.start == last.start {
                            self.undo_bytes += c.removed.len();
                            last.removed.push_str(&c.removed);
                            last.time = c.time;
                            return;
                        }
                    }
                }
            }
        }
        self.undo_bytes += c.removed.len() + c.inserted.len();
        self.undo.push(c);
        while (self.undo_bytes > UNDO_BYTES || self.undo.len() > UNDO_STEPS) && self.undo.len() > 1 {
            let d = self.undo.remove(0);
            self.undo_bytes -= (d.removed.len() + d.inserted.len()).min(self.undo_bytes);
        }
    }
    /// End the current typing group (e.g. the cursor was moved).
    pub fn break_group(&mut self) {
        if let Some(l) = self.undo.last_mut() {
            l.typing = false;
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Undo the last step; returns the byte range of the restored text.
    pub fn undo(&mut self) -> Option<(usize, usize)> {
        let c = self.undo.pop()?;
        self.undo_bytes -= (c.removed.len() + c.inserted.len()).min(self.undo_bytes);
        let end = (c.start + c.inserted.len()).min(self.s.len());
        self.s.replace_range(c.start..end, &c.removed);
        if !c.removed.is_ascii() {
            self.ascii = false;
        }
        self.ver += 1;
        let r = (c.start, c.start + c.removed.len());
        let mut c = c;
        c.typing = false;
        self.redo.push(c);
        Some(r)
    }
    /// Redo; returns the byte range of the re-inserted text.
    pub fn redo(&mut self) -> Option<(usize, usize)> {
        let c = self.redo.pop()?;
        let end = (c.start + c.removed.len()).min(self.s.len());
        self.s.replace_range(c.start..end, &c.inserted);
        if !c.inserted.is_ascii() {
            self.ascii = false;
        }
        self.ver += 1;
        let r = (c.start, c.start + c.inserted.len());
        self.undo_bytes += c.removed.len() + c.inserted.len();
        self.undo.push(c);
        Some(r)
    }
    pub fn as_str(&self) -> &str {
        &self.s
    }
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub src: Option<Arc<Source>>,
    pub text: VText,
    pub saved_ver: u64,
    pub eol: Eol,
    pub mixed_eol: bool,
    pub latin1: bool,
    pub bom: bool,
    pub known_bulks: HashSet<u64>,
    pub file_len: u64,
    pub total_lines_at_load: u64,
    pub load_ms: u64,
    pub threshold: u64,
    pub disk_stamp: Option<(u64, SystemTime)>,
    /// (length, hash) of the text as last loaded/saved, to tell "edited back to the original"
    saved_sig: (usize, u64),
    mod_cache: std::cell::Cell<(u64, bool)>,
}

fn text_sig(s: &str) -> (usize, u64) {
    let mut h = util::Fnv::default();
    h.bytes(s.as_bytes());
    (s.len(), h.0)
}

fn stamp(p: &Path) -> Option<(u64, SystemTime)> {
    let m = fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

impl Document {
    pub fn from_loaded(path: PathBuf, l: Loaded) -> Document {
        let text = VText::new(l.text);
        let ver = text.ver;
        let saved_sig = text_sig(&text.s);
        Document {
            saved_sig,
            mod_cache: std::cell::Cell::new((ver, false)),
            disk_stamp: stamp(&path),
            path: Some(path),
            file_len: l.src.len,
            src: Some(l.src),
            text,
            saved_ver: ver,
            eol: l.eol,
            mixed_eol: l.mixed_eol,
            latin1: l.latin1,
            bom: l.bom,
            known_bulks: l.bulks.into_iter().collect(),
            total_lines_at_load: l.total_lines,
            load_ms: l.elapsed_ms,
            threshold: l.threshold,
        }
    }
    /// A new, unsaved document (File → New).
    pub fn new_untitled(text: &str) -> Document {
        let text = VText::new(text.to_string());
        let ver = text.ver;
        Document {
            // an untouched template does not count as a change; any edit does
            saved_sig: text_sig(&text.s),
            mod_cache: std::cell::Cell::new((ver, false)),
            disk_stamp: None,
            path: None,
            file_len: 0,
            src: None,
            text,
            saved_ver: ver,
            eol: if cfg!(windows) { Eol::CrLf } else { Eol::Lf },
            mixed_eol: false,
            latin1: false,
            bom: false,
            known_bulks: Default::default(),
            total_lines_at_load: 0,
            load_ms: 0,
            threshold: 2000,
        }
    }
    pub fn modified(&self) -> bool {
        if self.text.ver == self.saved_ver {
            return false;
        }
        let (v, m) = self.mod_cache.get();
        if v == self.text.ver {
            return m;
        }
        let m = self.text.s.len() != self.saved_sig.0 || text_sig(&self.text.s) != self.saved_sig;
        self.mod_cache.set((self.text.ver, m));
        m
    }
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.inp".into())
    }
    pub fn dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn opts(&self) -> WriteOpts {
        WriteOpts { eol: self.eol, latin1: self.latin1, bom: self.bom, mode: WriteMode::Full }
    }
    /// Bulk ids referenced by placeholder lines in the current text.
    pub fn referenced_bulks(&self) -> HashSet<u64> {
        let mut set = HashSet::new();
        for (_, s, e) in util::lines(&self.text.s) {
            if let Some((id, _)) = placeholder_id(&self.text.s[s..e]) {
                set.insert(id);
            }
        }
        set
    }
    /// Known bulk blocks that were removed from the text (lines that would be dropped on save).
    pub fn missing_bulks(&self) -> Vec<Arc<Bulk>> {
        let r = self.referenced_bulks();
        let mut v: Vec<_> = self.known_bulks.iter().filter(|id| !r.contains(id)).filter_map(|id| STORE.get(*id)).collect();
        v.sort_by_key(|b| b.id);
        v
    }
    pub fn changed_on_disk(&self) -> bool {
        match (&self.path, &self.disk_stamp) {
            (Some(p), Some(st)) => stamp(p).map(|s| s != *st).unwrap_or(false),
            _ => false,
        }
    }

    /// Signature of the current text (to tell later whether it was edited back to this state).
    pub fn signature(&self) -> (usize, u64) {
        text_sig(&self.text.s)
    }

    /// Atomic save (synchronous): write to a temp file, keep the previous version as `.bak`,
    /// rename into place, then re-map the new file. The app runs the two phases separately so the
    /// slow part ([`write_temp`]) happens in a background thread.
    pub fn save_to(&mut self, target: &Path, keep_backup: bool) -> io::Result<SaveReport> {
        let tw = write_temp(&self.text.s, &self.opts(), target, None)?;
        let (ver, sig) = (self.text.ver, self.signature());
        self.finish_save(target, tw, ver, sig, keep_backup)
    }

    /// Phase 2 of saving (fast): move the old file aside (renaming works even while it is
    /// memory-mapped), move the temp file into place, and re-map bulk blocks to their new offsets.
    pub fn finish_save(&mut self, target: &Path, tw: TempWrite, ver: u64, sig: (usize, u64), keep_backup: bool) -> io::Result<SaveReport> {
        let dir = target.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let fname = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut backup = None;
        if target.exists() {
            let bak = dir.join(format!("{fname}.bak"));
            let bak = if fs::remove_file(&bak).is_ok() || !bak.exists() {
                bak
            } else {
                dir.join(format!("{fname}.{}.bak", util::now_secs()))
            };
            if let Err(e) = fs::rename(target, &bak) {
                let _ = fs::remove_file(&tw.tmp);
                return Err(io::Error::other(format!("could not move the old file aside: {e}")));
            }
            backup = Some(bak);
        }
        if let Err(e) = fs::rename(&tw.tmp, target) {
            if let Some(b) = &backup {
                let _ = fs::rename(b, target);
            }
            let _ = fs::remove_file(&tw.tmp);
            return Err(e);
        }
        let new_src = Source::open(target)?;
        for (id, s, e) in &tw.placed {
            if let Some(old) = STORE.get(*id) {
                STORE.replace(Arc::new(old.relocated(new_src.clone(), *s as usize, *e as usize)));
            }
        }
        self.known_bulks = tw.placed.iter().map(|p| p.0).collect();
        self.src = Some(new_src);
        self.path = Some(target.to_path_buf());
        self.saved_ver = ver;
        self.saved_sig = sig;
        self.mod_cache.set((0, false));
        self.file_len = tw.bytes;
        self.disk_stamp = stamp(target);
        let mut backup_kept = backup.clone();
        if !keep_backup {
            if let Some(b) = &backup {
                if fs::remove_file(b).is_ok() {
                    backup_kept = None;
                }
            }
        }
        Ok(SaveReport { bytes: tw.bytes, backup: backup_kept })
    }

    /// Write a copy without mesh data (node/element tables and all bulk blocks).
    pub fn export_without_mesh(&self, target: &Path) -> io::Result<u64> {
        export_nomesh(&self.text.s, self.opts(), &self.title(), target, None)
    }
}

/// Result of phase 1 of a save.
pub struct TempWrite {
    pub tmp: PathBuf,
    pub bytes: u64,
    pub placed: Vec<(u64, u64, u64)>,
}

/// Bytes the file will have (text + data blocks), for progress bars.
pub fn expected_size(text: &str) -> u64 {
    let mut n = text.len() as u64;
    for (_, s, e) in util::lines(text) {
        if let Some((id, _)) = placeholder_id(&text[s..e]) {
            if let Some(b) = STORE.get(id) {
                n += b.size();
            }
        }
    }
    n
}

/// Phase 1 of saving (slow, run in a background thread): write text + data blocks to a temp file
/// next to `target`, flush it to disk and check its size.
pub fn write_temp(text: &str, opts: &WriteOpts, target: &Path, prog: Option<&Progress>) -> io::Result<TempWrite> {
    let dir = target.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let fname = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{fname}.inplite-tmp"));
    let res = (|| -> io::Result<(u64, Vec<(u64, u64, u64)>)> {
        let f = File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(4 << 20, f);
        let r = write_text(text, &mut w, opts, prog)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        let len = f.metadata()?.len();
        if len != r.0 {
            return Err(io::Error::other(format!("size check failed: wrote {} of {} bytes", len, r.0)));
        }
        Ok(r)
    })();
    match res {
        Ok((bytes, placed)) => Ok(TempWrite { tmp, bytes, placed }),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Write a copy of `text` without node/element tables and data blocks.
pub fn export_nomesh(text: &str, opts: WriteOpts, title: &str, target: &Path, prog: Option<&Progress>) -> io::Result<u64> {
    let mut o = opts;
    o.mode = WriteMode::NoMesh;
    o.bom = false;
    let tmp = target.with_extension("inplite-tmp");
    let f = File::create(&tmp)?;
    let mut w = BufWriter::new(f);
    let nl = std::str::from_utf8(o.eol.bytes()).unwrap();
    let hdr = format!("** Exported by InpLite without mesh data from {title}{nl}** Node/element tables and large data blocks are replaced by comments. Not runnable.{nl}");
    w.write_all(hdr.as_bytes())?;
    let (n, _) = write_text(text, &mut w, &o, prog)?;
    w.flush()?;
    drop(w);
    fs::rename(&tmp, target)?;
    Ok(n + hdr.len() as u64)
}

pub struct SaveReport {
    pub bytes: u64,
    pub backup: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Full,
    NoMesh,
}

#[derive(Clone, Copy)]
pub struct WriteOpts {
    pub eol: Eol,
    pub latin1: bool,
    pub bom: bool,
    pub mode: WriteMode,
}

fn encode_into<W: Write>(w: &mut W, s: &str, latin1: bool) -> io::Result<u64> {
    if latin1 && !s.is_ascii() {
        let v: Vec<u8> = s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect();
        w.write_all(&v)?;
        Ok(v.len() as u64)
    } else {
        w.write_all(s.as_bytes())?;
        Ok(s.len() as u64)
    }
}

/// Write skeleton text, expanding bulk placeholders. Returns (bytes written, [(bulk id, start, end)]).
pub fn write_text<W: Write>(text: &str, w: &mut W, o: &WriteOpts, prog: Option<&Progress>) -> io::Result<(u64, Vec<(u64, u64, u64)>)> {
    let mut last_report: u64 = 0;
    let mut pos: u64 = 0;
    let mut placed = Vec::new();
    if o.bom && !o.latin1 {
        w.write_all(&[0xEF, 0xBB, 0xBF])?;
        pos += 3;
    }
    let eol = o.eol.bytes();
    let mut cur_kw = String::new();
    let mut omitted: u64 = 0;
    let mut kw_cont = false;
    let nomesh = o.mode == WriteMode::NoMesh;
    let bytes = text.as_bytes();
    let mut line_iter = util::lines(text).peekable();
    while let Some((_, s, e)) = line_iter.next() {
        let line = &text[s..e];
        let has_nl = e < bytes.len();
        if let Some((id, _)) = placeholder_id(line) {
            let b = STORE
                .get(id)
                .ok_or_else(|| io::Error::other(format!("unknown data block #{id} (placeholder line was edited?)")))?;
            if nomesh {
                if omitted > 0 {
                    let c = format!("** [InpLite] {} data lines omitted", util::fmt_int(omitted));
                    pos += encode_into(w, &c, o.latin1)?;
                    w.write_all(eol)?;
                    pos += eol.len() as u64;
                    omitted = 0;
                }
                let c = format!("** [InpLite] omitted {} data: {} lines, {}", b.kw_disp, util::fmt_int(b.lines), util::fmt_bytes(b.size()));
                pos += encode_into(w, &c, o.latin1)?;
            } else {
                let data = b.bytes();
                let start = pos;
                for chunk in data.chunks(8 << 20) {
                    w.write_all(chunk)?;
                    pos += chunk.len() as u64;
                    if let Some(p) = prog {
                        p.done.store(pos, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                placed.push((id, start, pos));
            }
        } else {
            let t = line.trim_start();
            let is_comment = t.starts_with("**");
            let is_kw = t.starts_with('*') && !is_comment;
            if nomesh {
                if is_kw {
                    if omitted > 0 {
                        let c = format!("** [InpLite] {} data lines omitted", util::fmt_int(omitted));
                        pos += encode_into(w, &c, o.latin1)?;
                        w.write_all(eol)?;
                        pos += eol.len() as u64;
                        omitted = 0;
                    }
                    cur_kw = util::norm(t[1..].split(',').next().unwrap_or(""));
                    kw_cont = t.trim_end().ends_with(',');
                } else if kw_cont && !is_comment && !t.is_empty() {
                    kw_cont = t.trim_end().ends_with(',');
                } else if !is_comment && !t.is_empty() && (cur_kw == "NODE" || cur_kw == "ELEMENT") {
                    omitted += 1;
                    continue;
                }
            }
            pos += encode_into(w, line, o.latin1)?;
        }
        if has_nl {
            w.write_all(eol)?;
            pos += eol.len() as u64;
        }
        if let Some(p) = prog {
            if pos - last_report > (1 << 20) {
                p.done.store(pos, std::sync::atomic::Ordering::Relaxed);
                last_report = pos;
            }
        }
    }
    if nomesh && omitted > 0 {
        let c = format!("** [InpLite] {} data lines omitted", util::fmt_int(omitted));
        w.write_all(eol)?;
        pos += eol.len() as u64;
        pos += encode_into(w, &c, o.latin1)?;
    }
    Ok((pos, placed))
}

/// Write a slice of skeleton text into a new include file (bulk placeholders expanded).
pub fn write_include_file(path: &Path, text: &str, o: &WriteOpts, prog: Option<&Progress>) -> io::Result<u64> {
    let tmp = path.with_extension("inplite-tmp");
    let f = File::create(&tmp)?;
    let mut w = BufWriter::with_capacity(4 << 20, f);
    let mut o2 = *o;
    o2.mode = WriteMode::Full;
    o2.bom = false;
    let (n, _) = write_text(text, &mut w, &o2, prog)?;
    let f = w.into_inner().map_err(|e| e.into_error())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{load, Progress};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("inplite_doc_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(p: &Path, crlf: bool) -> Vec<u8> {
        let nl = if crlf { "\r\n" } else { "\n" };
        let mut s = String::new();
        s += &format!("*Heading{nl}round trip{nl}*Part, name=P{nl}*Node{nl}");
        for i in 1..=50 {
            s += &format!("{i}, {}., 0., 0.{nl}", i);
        }
        s += &format!("*Element, type=T3D2, elset=E{nl}");
        for i in 1..=49 {
            s += &format!("{i}, {i}, {}{nl}", i + 1);
        }
        s += &format!("*End Part{nl}** last line without newline");
        let mut f = File::create(p).unwrap();
        f.write_all(s.as_bytes()).unwrap();
        s.into_bytes()
    }

    #[test]
    fn roundtrip_exact() {
        for crlf in [false, true] {
            let d = tmpdir(if crlf { "crlf" } else { "lf" });
            let p = d.join("m.inp");
            let orig = sample(&p, crlf);
            let pr = Progress::new();
            let l = load(&p, 10, &pr).unwrap();
            assert_eq!(l.bulks.len(), 2);
            let mut doc = Document::from_loaded(p.clone(), l);
            let rep = doc.save_to(&p, true).unwrap();
            let now = fs::read(&p).unwrap();
            assert_eq!(now, orig, "byte-exact round trip (crlf={crlf})");
            assert!(rep.backup.is_some());
            // second save after remap still exact
            doc.save_to(&p, false).unwrap();
            assert_eq!(fs::read(&p).unwrap(), orig);
            // edit + save
            let t = doc.text.s.replace("round trip", "edited");
            doc.text.set(t);
            doc.save_to(&p, false).unwrap();
            let s = String::from_utf8(fs::read(&p).unwrap()).unwrap();
            assert!(s.contains("edited"));
            assert!(s.contains("50, 50., 0., 0."));
        }
    }

    #[test]
    fn nomesh_export() {
        let d = tmpdir("nomesh");
        let p = d.join("m.inp");
        sample(&p, false);
        let pr = Progress::new();
        let l = load(&p, 10, &pr).unwrap();
        let doc = Document::from_loaded(p.clone(), l);
        let out = d.join("m_nomesh.inp");
        doc.export_without_mesh(&out).unwrap();
        let s = fs::read_to_string(&out).unwrap();
        assert!(s.contains("omitted *Node data: 50 lines"));
        assert!(!s.contains("50, 50., 0., 0."));
        assert!(s.contains("*End Part"));
    }
}
