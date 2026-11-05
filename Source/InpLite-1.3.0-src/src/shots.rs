//! Scripted screenshot capture for documentation (feature "shots", Linux dev builds only).
//! Run: INPLITE_SHOTS=<outdir> INPLITE_SHOTS_DIFF=<other.inp> inplite <model.inp>

use crate::app::{App, Bottom, GotoDlg, GotoMode, SplitDlg, Tab};
use crate::tab::{BulkViewer, DocTab};
use std::io::Write;
use std::path::PathBuf;

pub struct Shots {
    dir: PathBuf,
    diff: Option<PathBuf>,
    scene: usize,
    wait: u32,
    requested: bool,
}

impl Shots {
    pub fn from_env() -> Option<Shots> {
        let dir = std::env::var_os("INPLITE_SHOTS")?;
        let diff = std::env::var_os("INPLITE_SHOTS_DIFF").map(PathBuf::from);
        Some(Shots { dir: PathBuf::from(dir), diff, scene: 0, wait: 0, requested: false })
    }
}

fn doc(app: &mut App) -> Option<&mut DocTab> {
    match app.tabs.get_mut(app.active) {
        Some(Tab::Doc(d)) => Some(d),
        _ => None,
    }
}

fn find_line(d: &DocTab, needle: &str) -> Option<usize> {
    d.doc.text.s.find(needle).map(|b| crate::util::line_of(&d.map.starts, b))
}

fn save_ppm(path: &std::path::Path, img: &egui::ColorImage) {
    let mut f = std::fs::File::create(path).unwrap();
    write!(f, "P6\n{} {}\n255\n", img.size[0], img.size[1]).unwrap();
    let mut buf = Vec::with_capacity(img.pixels.len() * 3);
    for p in &img.pixels {
        buf.extend_from_slice(&[p.r(), p.g(), p.b()]);
    }
    f.write_all(&buf).unwrap();
}

/// Scenes: (name, setup). Setup returns true once the scene is ready.
pub fn step(app: &mut App, ctx: &egui::Context) {
    let Some(mut s) = app.shots.take() else { return };
    ctx.request_repaint();
    // screenshot arrived?
    let shot = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
    });
    if let Some(img) = shot {
        let names = ["1_overview", "2_errors", "3_autocomplete", "4_data_viewer", "5_summary", "6_diff", "7_include_split"];
        let p = s.dir.join(format!("{}.ppm", names[s.scene]));
        save_ppm(&p, &img);
        eprintln!("saved {}", p.display());
        s.scene += 1;
        s.wait = 0;
        s.requested = false;
        if let Some(Tab::Doc(d)) = app.tabs.get_mut(0) {
            d.force_hover = None;
        }
    }
    if s.requested {
        app.shots = Some(s);
        return;
    }
    // wait for everything to be loaded/analysed
    let ready = match app.tabs.first() {
        Some(Tab::Doc(d)) => d.analysis.as_ref().map(|a| a.ver) == Some(d.doc.text.ver) && d.mesh.is_some() && d.mesh_job.is_none(),
        _ => false,
    };
    if !ready && s.scene < 7 {
        app.shots = Some(s);
        return;
    }
    s.wait += 1;
    let settle = 25;
    match s.scene {
        0 => {
            if s.wait == 1 {
                app.active = 0;
                app.bottom = Bottom::Problems;
                if let Some(d) = doc(app) {
                    for (k, _) in d.outline_open.clone() {
                        d.outline_open.remove(&k);
                    }
                    let open_keys: Vec<String> = d
                        .analysis
                        .as_ref()
                        .unwrap()
                        .outline
                        .iter()
                        .filter(|n| n.label.starts_with("Part  PLATE") || n.label.starts_with("Step  Forming"))
                        .map(|n| format!("/{}#0", n.label))
                        .collect();
                    for k in open_keys {
                        d.outline_open.insert(k, true);
                    }
                    if let Some(l) = find_line(d, "*Part, name=PLATE") {
                        d.jump_to_line(l + 1, None);
                    }
                }
            }
            if s.wait == 5 {
                if let Some(d) = doc(app) {
                    if let Some(l) = find_line(d, "*Part, name=PLATE") {
                        d.jump_to_line(l.saturating_sub(6), None);
                    }
                }
            }
        }
        1 => {
            if s.wait == 1 {
                if let Some(d) = doc(app) {
                    if let Some(l) = find_line(d, "*Material, name=STEEL-DP780") {
                        d.jump_to_line(l + 14, None);
                    }
                }
            }
            if s.wait == 8 {
                if let Some(d) = doc(app) {
                    d.force_hover = d.doc.text.s.find("*Elastik").map(|b| b + 3);
                }
            }
        }
        2 => {
            if s.wait == 1 {
                if let Some(d) = doc(app) {
                    if let Some(b) = d.doc.text.s.find("*Density\n7.85e-09,\n") {
                        let at = b + "*Density\n7.85e-09,\n".len();
                        d.doc.text.replace_range(at..at, "*pla\n");
                        let cur = at + 4;
                        let line = crate::util::line_of(&crate::util::line_starts(&d.doc.text.s), cur);
                        d.jump_to_line(line, Some((cur, cur)));
                    }
                }
            }
            if s.wait == 6 {
                if let Some(d) = doc(app) {
                    let b = d.doc.text.s.find("*pla\n").map(|b| b + 4).unwrap_or(0);
                    d.ensure_map();
                    d.cursor = (d.map.line_of_byte(b), 4, b);
                    d.completion = crate::editor::complete_at(d, b);
                }
            }
        }
        3 => {
            if s.wait == 1 {
                if let Some(d) = doc(app) {
                    // undo the autocomplete demo edit
                    if let Some(b) = d.doc.text.s.find("*pla\n") {
                        d.doc.text.replace_range(b..b + 5, "");
                    }
                    d.completion = None;
                    let hit = d.mesh.as_ref().unwrap().find(crate::mesh::IdKind::Node, 512345, &d.doc.text.s);
                    if let Some((_, crate::mesh::Hit::Bulk { id, row })) = hit.first().cloned() {
                        d.ensure_map();
                        if let Some(&l) = d.map.bulk_line.get(&id) {
                            d.jump_to_line(l, None);
                        }
                        let mut v = BulkViewer::new(id, Some(row));
                        v.goto = "512345".into();
                        d.viewer = Some(v);
                    }
                }
                app.bottom = Bottom::Data;
                app.dlg.goto = Some(GotoDlg { mode: GotoMode::Node, text: "512345".into(), results: vec![], msg: String::new(), focus: false, pending: None, searched: None, idx: 0 });
            }
        }
        4 => {
            if s.wait == 1 {
                app.dlg.goto = None;
                app.bottom = Bottom::Problems;
                if let Some(d) = doc(app) {
                    d.viewer = None;
                }
                app.dlg.summary = true;
            }
        }
        5 => {
            if s.wait == 1 {
                app.dlg.summary = false;
                if let Some(p) = s.diff.clone() {
                    app.compare_with(p);
                }
            }
            if !matches!(app.tabs.get(app.active), Some(Tab::Diff(_))) {
                s.wait = 2;
            }
        }
        6 => {
            if s.wait == 1 {
                app.active = 0;
                app.bottom = Bottom::Problems;
                if let Some(d) = doc(app) {
                    d.viewer = None;
                    let an = d.analysis.clone().unwrap();
                    if let Some(n) = an.outline.iter().find(|n| n.label.starts_with("Part  PLATE")) {
                        let ranges = crate::outline::mesh_ranges(&an, n.block);
                        if !ranges.is_empty() {
                            let ver = d.doc.text.ver;
                            app.dlg.split = Some(SplitDlg { ranges, folder: d.doc.dir().to_string_lossy().into_owned(), name: "forming_PLATE_mesh.inp".into(), label: "mesh of part PLATE".into(), ver, err: String::new() });
                        }
                    }
                }
            }
        }
        _ => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            app.exit_ok = true;
            app.shots = Some(s);
            return;
        }
    }
    if s.wait >= settle && !s.requested {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        s.requested = true;
    }
    app.shots = Some(s);
}

/// File-operation self test (save in place while memory-mapped, backups, include split).
/// Used to verify Windows semantics of the cross-compiled build: INPLITE_SELFTEST=<work dir> inplite <model.inp>
pub fn selftest(dir: &std::path::Path, src: &std::path::Path) -> bool {
    use crate::{analysis, doc::Document, load};
    let mut ok = true;
    let mut check = |name: &str, c: bool| {
        println!("{} {}", if c { "PASS" } else { "FAIL" }, name);
        ok &= c;
    };
    let _ = std::fs::create_dir_all(dir);
    let p = dir.join("selftest.inp");
    std::fs::copy(src, &p).unwrap();
    let orig = std::fs::read(&p).unwrap();
    let pr = load::Progress::new();
    let l = load::load(&p, 100, &pr).unwrap();
    check("load: bulk blocks found", !l.bulks.is_empty());
    let mut doc = Document::from_loaded(p.clone(), l);
    let r = doc.save_to(&p, true);
    check(&format!("save in place while mapped: {:?}", r.as_ref().err()), r.is_ok());
    check("byte-identical after save", std::fs::read(&p).unwrap() == orig);
    check(".bak created", dir.join("selftest.inp.bak").exists());
    let r = doc.save_to(&p, true);
    check(&format!("second save (replace old .bak): {:?}", r.as_ref().err()), r.is_ok());
    check("byte-identical after second save", std::fs::read(&p).unwrap() == orig);
    let r = doc.save_to(&p, false);
    check(&format!("save without keeping backup: {:?}", r.as_ref().map(|x| x.backup.clone()).err()), r.is_ok());
    let t = doc.text.s.replacen("DP780-cup-draw", "SELFTEST", 1);
    doc.text.set(t);
    let r = doc.save_to(&p, true);
    check("save after edit", r.is_ok());
    let edited = std::fs::read(&p).unwrap();
    check("edit persisted", String::from_utf8_lossy(&edited).contains("SELFTEST"));
    check("size consistent", edited.len() + "DP780-cup-draw".len() == orig.len() + "SELFTEST".len());
    // split mesh into include
    let an = analysis::analyze(&doc.text.s, doc.text.ver, &doc.dir());
    let part = an.outline.iter().find(|n| n.label.starts_with("Part  PLATE"));
    if let Some((s, e)) = part.and_then(|n| crate::outline::mesh_ranges(&an, n.block).first().copied()) {
        let slice = doc.text.s[s..e].to_string();
        let inc = dir.join("selftest_PLATE_mesh.inp");
        let _ = std::fs::remove_file(&inc);
        let w = crate::doc::write_include_file(&inc, &slice, &doc.opts(), None);
        check("write include file", w.is_ok());
        for (_, ls, le) in crate::util::lines(&slice) {
            if let Some((id, _)) = crate::store::placeholder_id(&slice[ls..le]) {
                doc.known_bulks.remove(&id);
            }
        }
        doc.text.replace_range(s..e, "*Include, input=selftest_PLATE_mesh.inp\n");
        let r = doc.save_to(&p, true);
        check("save with *Include", r.is_ok());
        let main = std::fs::read(&p).unwrap();
        let incb = std::fs::read(&inc).unwrap();
        let line = b"*Include, input=selftest_PLATE_mesh.inp\r\n";
        let pos = main.windows(line.len()).position(|w| w == line);
        check("include line present", pos.is_some());
        if let Some(pos) = pos {
            let mut rebuilt = main[..pos].to_vec();
            rebuilt.extend_from_slice(&incb);
            rebuilt.extend_from_slice(&main[pos + line.len()..]);
            check("main + include == edited original", rebuilt == edited);
        }
    } else {
        check("found PLATE mesh range", false);
    }
    let r = doc.export_without_mesh(&dir.join("selftest_nomesh.inp"));
    check("export without mesh", r.is_ok());
    println!("SELFTEST {}", if ok { "OK" } else { "FAILED" });
    ok
}
