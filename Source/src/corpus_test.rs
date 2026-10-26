//! Lints every .inp file under $INPLITE_CORPUS and prints the diagnostics (cargo test -- --ignored --nocapture).
use crate::{analysis, load, mesh, syntax};
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map(|x| x.eq_ignore_ascii_case("inp")).unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

#[test]
#[ignore]
fn lint_corpus() {
    let Some(dir) = std::env::var_os("INPLITE_CORPUS") else { return };
    let mut files = Vec::new();
    walk(Path::new(&dir), &mut files);
    files.sort();
    let mut total = 0;
    for f in files {
        let pr = load::Progress::new();
        let Ok(l) = load::load(&f, 2000, &pr) else { continue };
        let text = l.text;
        let hl = syntax::highlight(&text, &egui::FontId::monospace(12.0), &[]);
        let an = analysis::analyze(&text, 1, f.parent().unwrap());
        let m = mesh::build(&text, 1, &pr);
        let mut all: Vec<_> = hl.diags.iter().chain(an.diags.iter()).chain(m.diags.iter()).collect();
        all.sort_by_key(|d| d.line);
        println!("=== {} ({} blocks, {} nodes, {} elems): {} diags", f.display(), an.blocks.len(), m.total_nodes, m.total_elements, all.len());
        for d in all.iter().take(40) {
            let lt = text.lines().nth(d.line).unwrap_or("");
            println!("  L{:<6} {:?} {}   | {}", d.line + 1, d.sev, d.msg, &lt[..lt.len().min(90)]);
        }
        total += all.len();
    }
    println!("TOTAL diags {total}");
}

#[test]
#[ignore]
fn glyphs() {
    let ctx = egui::Context::default();
    let _ = ctx.run(Default::default(), |_| {});
    for c in "●•·…↔⇄⋯≡→×—–▲▼✕↑↓■□▸▾⚠✔✖⬤○◆".chars() {
        let p = ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::proportional(13.0), c));
        let m = ctx.fonts_mut(|f| f.has_glyph(&egui::FontId::monospace(13.0), c));
        println!("{c} U+{:04X} prop={p} mono={m}", c as u32);
    }
}

fn rss() -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().filter(|l| l.starts_with("RssAnon") || l.starts_with("VmHWM") || l.starts_with("RssFile")).collect::<Vec<_>>().join(" | ")
}

/// Performance check on a big file: INPLITE_BIG=/path/model.inp cargo test --release big_file -- --ignored --nocapture
#[test]
#[ignore]
fn big_file() {
    use std::time::Instant;
    let Some(p) = std::env::var_os("INPLITE_BIG") else { return };
    let p = std::path::PathBuf::from(p);
    let t = Instant::now();
    let pr = load::Progress::new();
    let l = load::load(&p, 2000, &pr).unwrap();
    println!("load: {:.2} s, file {} MB, skeleton {} KB, {} bulks, {} lines | {}", t.elapsed().as_secs_f32(), l.src.len >> 20, l.text.len() >> 10, l.bulks.len(), l.total_lines, rss());
    let text = l.text.clone();
    let t = Instant::now();
    let hl = syntax::highlight(&text, &egui::FontId::monospace(13.0), &[]);
    println!("highlight: {:.1} ms, {} sections", t.elapsed().as_secs_f64() * 1e3, hl.job.sections.len());
    let ctx = egui::Context::default();
    let _ = ctx.run(Default::default(), |_| {});
    let t = Instant::now();
    let g = ctx.fonts_mut(|f| f.layout_job(hl.job.clone()));
    println!("layout (cold): {:.1} ms, {} rows", t.elapsed().as_secs_f64() * 1e3, g.rows.len());
    let t = Instant::now();
    let an = analysis::analyze(&text, 1, p.parent().unwrap());
    println!("analysis: {:.1} ms, {} blocks, {} diags", t.elapsed().as_secs_f64() * 1e3, an.blocks.len(), an.diags.len());
    let t = Instant::now();
    let m = mesh::build(&text, 1, &pr);
    println!("mesh check: {:.2} s, {} nodes, {} elements, {} diags | {}", t.elapsed().as_secs_f32(), m.total_nodes, m.total_elements, m.diags.len(), rss());
    let t = Instant::now();
    let hits = m.find(mesh::IdKind::Node, 5_123_456, &text);
    println!("find node 5123456: {:.2} ms -> {:?}", t.elapsed().as_secs_f64() * 1e3, hits.iter().map(|h| h.0.clone()).collect::<Vec<_>>());
    let t = Instant::now();
    let hits = m.find(mesh::IdKind::Element, 8_000_000, &text);
    println!("find element 8000000: {:.2} ms -> {} hit(s)", t.elapsed().as_secs_f64() * 1e3, hits.len());
    // save round trip into a copy
    let copy = p.with_file_name("roundtrip_copy.inp");
    let mut doc = crate::doc::Document::from_loaded(p.clone(), l);
    let t = Instant::now();
    doc.save_to(&copy, false).unwrap();
    println!("save (save-as copy): {:.2} s", t.elapsed().as_secs_f32());
    let h = |f: &std::path::Path| {
        let m = crate::store::Source::open(f).unwrap();
        let mut x = crate::util::Fnv::default();
        for c in m.bytes().chunks(1 << 20) {
            x.0 ^= c.len() as u64;
            let mut y = crate::util::Fnv::default();
            y.bytes(&c[..64.min(c.len())]);
            y.bytes(&c[c.len().saturating_sub(64)..]);
            x.0 = x.0.wrapping_mul(31).wrapping_add(y.0);
        }
        (m.len, x.0)
    };
    let same_len = std::fs::metadata(&p).unwrap().len() == std::fs::metadata(&copy).unwrap().len();
    let a = std::fs::read(&p).map(|v| v.len()).unwrap_or(0);
    let _ = a;
    let ok = same_len && {
        // full byte comparison, streamed
        use std::io::Read;
        let mut fa = std::io::BufReader::with_capacity(8 << 20, std::fs::File::open(&p).unwrap());
        let mut fb = std::io::BufReader::with_capacity(8 << 20, std::fs::File::open(&copy).unwrap());
        let mut ba = vec![0u8; 8 << 20];
        let mut bb = vec![0u8; 8 << 20];
        let mut eq = true;
        loop {
            let na = fa.read(&mut ba).unwrap();
            let mut nb = 0;
            while nb < na {
                let k = fb.read(&mut bb[nb..na]).unwrap();
                if k == 0 { break; }
                nb += k;
            }
            if na != nb || ba[..na] != bb[..nb] { eq = false; break; }
            if na == 0 { break; }
        }
        eq
    };
    let _ = h;
    println!("round trip byte-identical: {ok}");
    std::fs::remove_file(&copy).ok();
    println!("{}", rss());
}

#[test]
#[ignore]
fn layout_scaling() {
    use std::time::Instant;
    for n in [5_000usize, 20_000, 60_000] {
        let mut s = String::new();
        for i in 0..n {
            if i % 50 == 0 {
                s.push_str(&format!("*Nset, nset=SET-{i}\n"));
            } else {
                s.push_str(&format!("{}, {}, {}, {}, {}, {}, {}, {}\n", i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7));
            }
        }
        let ctx = egui::Context::default();
        let font = egui::FontId::monospace(13.0);
        let mut times = Vec::new();
        for frame in 0..4 {
            let mut s2 = s.clone();
            if frame >= 2 {
                s2.insert(s.len() / 2, 'x');
            }
            let _ = ctx.run(Default::default(), |ctx| {
                let t = Instant::now();
                let hl = syntax::highlight(&s2, &font, &[]);
                let th = t.elapsed().as_secs_f64() * 1e3;
                let t = Instant::now();
                let _g = ctx.fonts_mut(|f| f.layout_job(hl.job));
                times.push((th, t.elapsed().as_secs_f64() * 1e3));
            });
        }
        println!("{n} lines: (highlight ms, layout ms) per frame: {:?}", times.iter().map(|(a, b)| (a.round(), b.round())).collect::<Vec<_>>());
    }
}

fn sample_doc(tag: &str) -> (std::path::PathBuf, crate::doc::Document) {
    let dir = std::env::temp_dir().join(format!("inplite_flow_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("model.inp");
    let mut s = String::from("*Heading\r\nflow test\r\n*Part, name=PLATE\r\n*Node\r\n");
    for i in 1..=300 {
        s += &format!("{i}, {}., 0., 0.\r\n", i);
    }
    s += "*Element, type=T3D2, elset=BAR\r\n";
    for i in 1..300 {
        s += &format!("{i}, {i}, {}\r\n", i + 1);
    }
    s += "*Nset, nset=FIX\r\n1,\r\n*Solid Section, elset=BAR, material=STEEL\r\n1.\r\n*End Part\r\n";
    s += "*Assembly, name=A\r\n*Instance, name=PLATE-1, part=PLATE\r\n*End Instance\r\n*End Assembly\r\n";
    s += "*Material, name=STEEL\r\n*Elastic\r\n210000., 0.3\r\n*Step, name=S1\r\n*Static\r\n*Boundary\r\nPLATE-1.FIX, ENCASTRE\r\n*End Step\r\n";
    std::fs::write(&p, s).unwrap();
    let pr = load::Progress::new();
    let l = load::load(&p, 100, &pr).unwrap();
    assert_eq!(l.bulks.len(), 2);
    (p.clone(), crate::doc::Document::from_loaded(p, l))
}

#[test]
fn split_include_save_reload() {
    let (p, mut doc) = sample_doc("split");
    let orig = std::fs::read_to_string(&p).unwrap();
    let an = analysis::analyze(&doc.text.s, doc.text.ver, &doc.dir());
    let part = an.outline.iter().find(|n| n.label == "Part  PLATE").unwrap();
    let r = crate::outline::mesh_ranges(&an, part.block);
    assert_eq!(r.len(), 1);
    let (s, e) = r[0];
    let slice = doc.text.s[s..e].to_string();
    let inc = doc.dir().join("model_PLATE_mesh.inp");
    crate::doc::write_include_file(&inc, &slice, &doc.opts(), None).unwrap();
    for (_, ls, le) in crate::util::lines(&slice) {
        if let Some((id, _)) = crate::store::placeholder_id(&slice[ls..le]) {
            doc.known_bulks.remove(&id);
        }
    }
    doc.text.replace_range(s..e, "*Include, input=model_PLATE_mesh.inp\n");
    assert!(doc.missing_bulks().is_empty());
    doc.save_to(&p, true).unwrap();
    let main = std::fs::read_to_string(&p).unwrap();
    let incs = std::fs::read_to_string(&inc).unwrap();
    // splice the include back in: must equal the original byte-for-byte
    let rebuilt = main.replace("*Include, input=model_PLATE_mesh.inp\r\n", &incs);
    assert_eq!(rebuilt, orig);
    assert!(incs.starts_with("*Node\r\n1, 1., 0., 0.\r\n"));
    assert!(incs.contains("*Nset, nset=FIX\r\n1,\r\n"));
    // the analysis of the new main file sees the definitions inside the include (FIX is used by *Boundary)
    let pr = load::Progress::new();
    let l = load::load(&p, 100, &pr).unwrap();
    let an2 = analysis::analyze(&l.text, 1, p.parent().unwrap());
    assert!(an2.diags.is_empty(), "{:?}", an2.diags);
    assert_eq!(an2.includes.len(), 1);
    assert!(an2.includes[0].exists);
    // inline it back
    let li = load::load(&inc, 100, &pr).unwrap();
    let pos = l.text.find("*Include").unwrap();
    let end = pos + l.text[pos..].find('\n').unwrap() + 1;
    let mut t = l.text.clone();
    t.replace_range(pos..end, &li.text);
    let mut d2 = crate::doc::Document::from_loaded(p.clone(), l);
    d2.text.set(t);
    d2.known_bulks.extend(li.bulks.iter().copied());
    let out = p.with_file_name("inlined.inp");
    d2.save_to(&out, false).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), orig);
}

#[test]
fn completion_and_navigation() {
    let (_p, doc) = sample_doc("nav");
    let mut tab = crate::tab::DocTab::new(doc);
    // wait for the background analysis
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(tab.analysis.is_some());
    // keyword completion
    let at = tab.doc.text.s.find("*Static").unwrap() + 3;
    tab.ensure_map();
    tab.cursor = (tab.map.line_of_byte(at), 3, at);
    let c = crate::editor::complete_at(&tab, at).unwrap();
    assert!(c.items.iter().any(|i| i.label == "*Static"), "{:?}", c.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>());
    // parameter value completion: material=
    let at = tab.doc.text.s.find("material=STEEL").unwrap() + "material=ST".len();
    tab.cursor = (tab.map.line_of_byte(at), 0, at);
    let c = crate::editor::complete_at(&tab, at).unwrap();
    assert_eq!(c.items[0].insert, "STEEL");
    // parameter name completion after ", "
    let line_end = tab.doc.text.s.find("*Solid Section, elset=BAR, material=STEEL").unwrap() + "*Solid Section, elset=BAR,".len();
    tab.cursor = (tab.map.line_of_byte(line_end), 0, line_end);
    let c = crate::editor::complete_at(&tab, line_end).unwrap();
    assert!(c.items.iter().any(|i| i.label == "orientation"), "{:?}", c.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>());
    // go to definition of PLATE-1.FIX
    let at = tab.doc.text.s.find("PLATE-1.FIX").unwrap() + 9;
    tab.cursor = (tab.map.line_of_byte(at), 0, at);
    let mut acts = Vec::new();
    crate::editor::go_to_definition(&mut tab, &mut acts);
    let j = tab.jump.expect("jump");
    let def_line = tab.line_text(j.line).to_string();
    assert!(def_line.starts_with("*Nset, nset=FIX"), "{def_line}");
    // references
    crate::editor::find_references(&mut tab, &mut acts);
    assert!(tab.refs.len() >= 2, "{}", tab.refs.len());
}

/// Drive the data viewer with simulated input on a big file and report frame times.
/// INPLITE_BIG=/path/model.inp cargo test --release viewer_interaction -- --ignored --nocapture
#[test]
#[ignore]
fn viewer_interaction() {
    use egui::{pos2, vec2, Event, Key, Modifiers, PointerButton, RawInput, Rect};
    use std::time::Instant;
    let Some(p) = std::env::var_os("INPLITE_BIG") else { return };
    let p = std::path::PathBuf::from(p);
    let pr = load::Progress::new();
    let l = load::load(&p, 2000, &pr).unwrap();
    let bulk = l.bulks[0];
    let doc = crate::doc::Document::from_loaded(p.clone(), l);
    let mut tab = crate::tab::DocTab::new(doc); // starts analysis + mesh threads like the app
    tab.viewer = Some(crate::tab::BulkViewer::new(bulk, None));
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 400.0));
    let frame = |name: &str, events: Vec<Event>, tab: &mut crate::tab::DocTab| {
        let raw = RawInput { screen_rect: Some(screen), events, ..Default::default() };
        let t = Instant::now();
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::viewer::viewer_ui(ui, tab, 13.0, &mut acts));
        });
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let v = tab.viewer.as_ref().map(|v| (v.top, v.sel, v.msg.clone()));
        println!("{name:<28} {ms:>9.1} ms   viewer={v:?}");
    };
    let mods = Modifiers::NONE;
    frame("first frame", vec![], &mut tab);
    frame("hover", vec![Event::PointerMoved(pos2(600.0, 250.0))], &mut tab);
    frame("wheel down", vec![Event::PointerMoved(pos2(600.0, 250.0)), Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, -5.0), modifiers: mods }], &mut tab);
    frame("wheel down 2", vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, -5.0), modifiers: mods }], &mut tab);
    frame("page down", vec![Event::Key { key: Key::PageDown, physical_key: None, pressed: true, repeat: false, modifiers: mods }], &mut tab);
    // drag the scrollbar from top to the middle and to the end
    let sx = screen.right() - 14.0;
    frame("scrollbar press", vec![Event::PointerMoved(pos2(sx, 90.0)), Event::PointerButton { pos: pos2(sx, 90.0), button: PointerButton::Primary, pressed: true, modifiers: mods }], &mut tab);
    for (i, y) in [150.0, 220.0, 300.0, 380.0].iter().enumerate() {
        frame(&format!("scrollbar drag {i}"), vec![Event::PointerMoved(pos2(sx, *y))], &mut tab);
    }
    frame("scrollbar release", vec![Event::PointerButton { pos: pos2(sx, 380.0), button: PointerButton::Primary, pressed: false, modifiers: mods }], &mut tab);
    frame("click row", vec![Event::PointerMoved(pos2(500.0, 200.0)), Event::PointerButton { pos: pos2(500.0, 200.0), button: PointerButton::Primary, pressed: true, modifiers: mods }], &mut tab);
    frame("click row release", vec![Event::PointerButton { pos: pos2(500.0, 200.0), button: PointerButton::Primary, pressed: false, modifiers: mods }], &mut tab);
    // go to an existing label and a missing one
    if let Some(v) = tab.viewer.as_mut() {
        v.goto = "5123456".into();
    }
    let go = crate::viewer::find_label;
    let b = crate::store::STORE.get(bulk).unwrap();
    let t = Instant::now();
    let r = go(&b, 5_123_456);
    println!("find_label existing          {:>9.1} ms -> {r:?}", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    let r = go(&b, 99_999_999);
    println!("find_label missing           {:>9.1} ms -> {r:?}", t.elapsed().as_secs_f64() * 1e3);
}

#[test]
fn background_save_and_split() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let (p, doc) = sample_doc("bgops");
    let orig = std::fs::read_to_string(&p).unwrap();
    let mut app = crate::app::App::for_test();
    let mut tab = crate::tab::DocTab::new(doc);
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // block that owns the *Node data block -> "Save as *Include file"
    let an = tab.analysis.clone().unwrap();
    let bi = an.blocks.iter().position(|b| b.norm == "NODE").unwrap();
    let crate::actions::Action::SplitBlock { ranges, suggested, .. } = crate::outline::block_split_action(&an, bi, "model") else { panic!() };
    assert_eq!(suggested, "model_PLATE_Node.inp");
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    app.start_split(vec![crate::app::SplitEdit { ranges, inc: suggested.clone() }]);
    assert!(app.file_op.is_some());
    app.wait_file_op();
    let crate::app::Tab::Doc(d) = &app.tabs[0] else { panic!() };
    assert!(d.doc.text.s.contains("*Include, input=model_PLATE_Node.inp\n*Element"), "{}", &d.doc.text.s[..200]);
    assert!(d.doc.missing_bulks().is_empty());
    let inc = std::fs::read_to_string(p.with_file_name("model_PLATE_Node.inp")).unwrap();
    assert!(inc.starts_with("*Node\r\n1, 1., 0., 0.\r\n") && inc.ends_with("300, 300., 0., 0.\r\n"), "{}", &inc[inc.len() - 40..]);
    // background save, then splice check
    app.save(0, false, false);
    assert!(app.file_op.is_some());
    app.wait_file_op();
    let main = std::fs::read_to_string(&p).unwrap();
    assert_eq!(main.replace("*Include, input=model_PLATE_Node.inp\r\n", &inc), orig);
    let crate::app::Tab::Doc(d) = &app.tabs[0] else { panic!() };
    assert!(!d.doc.modified());
    assert!(app.toasts.iter().any(|t| t.0.starts_with("Saved model.inp")), "{:?}", app.toasts);
}

/// Drive the whole application (outline jumps, typing, scrolling) on a model and print frame times.
/// INPLITE_PERF=/path/model.inp cargo test --release app_perf -- --ignored --nocapture
#[test]
#[ignore]
fn app_perf() {
    use egui::{pos2, vec2, Event, Key, Modifiers, RawInput, Rect};
    use std::time::Instant;
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let Some(p) = std::env::var_os("INPLITE_PERF") else { return };
    let ctx = egui::Context::default();
    crate::app::setup_style(&ctx);
    let mut app = crate::app::App::for_test();
    app.settings.show_outline = std::env::var_os("PERF_NO_OUTLINE").is_none();
    app.settings.show_bottom = std::env::var_os("PERF_NO_BOTTOM").is_none();
    app.open(std::path::PathBuf::from(p), None);
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1360.0, 860.0));
    let mut time = 0.0f64;
    let mut worst: Vec<(f64, String)> = Vec::new();
    let mut run = |app: &mut crate::app::App, name: &str, events: Vec<Event>, quiet: bool| -> f64 {
        time += 1.0 / 30.0;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(time), focused: true, ..Default::default() };
        let t = Instant::now();
        let out = ctx.run(raw, |ctx| app.frame(ctx));
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if !quiet || ms > 50.0 {
            println!("{name:<34} {ms:>8.1} ms  ({} meshes)", prims.len());
        }
        worst.push((ms, name.to_string()));
        ms
    };
    // wait for load + analysis + mesh
    let t0 = Instant::now();
    loop {
        run(&mut app, "loading", vec![], true);
        let ready = match app.tabs.first() {
            Some(crate::app::Tab::Doc(d)) => d.analysis.as_ref().map(|a| a.ver) == Some(d.doc.text.ver) && d.mesh_job.is_none(),
            _ => false,
        };
        if ready || t0.elapsed().as_secs() > 120 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    let lines = match app.tabs.first() {
        Some(crate::app::Tab::Doc(d)) => d.map.starts.len(),
        _ => 0,
    };
    println!("ready after {:.1}s, skeleton lines {lines}, text {} bytes", t0.elapsed().as_secs_f64(), match app.tabs.first() { Some(crate::app::Tab::Doc(d)) => d.doc.text.s.len(), _ => 0 });
    for i in 0..3 {
        run(&mut app, &format!("idle {i}"), vec![], false);
    }
    let key = |k: Key| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    // click into the editor to focus it
    let ep = pos2(700.0, 200.0);
    run(&mut app, "click editor", vec![Event::PointerMoved(ep), Event::PointerButton { pos: ep, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], false);
    run(&mut app, "release", vec![Event::PointerButton { pos: ep, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], false);
    for frac in [0.1, 0.5, 0.9, 0.3, 0.7] {
        let line = (lines as f64 * frac) as usize;
        if let Some(crate::app::Tab::Doc(d)) = app.tabs.get_mut(0) {
            d.jump_to_line(line, None);
        }
        run(&mut app, &format!("jump to {line}"), vec![], false);
        run(&mut app, "after jump", vec![], false);
        for (j, c) in "x, y".chars().enumerate() {
            run(&mut app, &format!("type {j}"), vec![Event::Text(c.to_string())], false);
        }
        run(&mut app, "enter", vec![key(Key::Enter)], false);
        run(&mut app, "backspace", vec![key(Key::Backspace)], false);
        for j in 0..4 {
            run(&mut app, &format!("arrow down {j}"), vec![key(Key::ArrowDown)], false);
        }
        run(&mut app, "page down", vec![key(Key::PageDown)], false);
        run(&mut app, "wheel", vec![Event::PointerMoved(ep), Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, -10.0), modifiers: Modifiers::NONE }], false);
        // let the debounced analysis run and come back
        let t1 = Instant::now();
        while t1.elapsed().as_millis() < 1500 {
            run(&mut app, "idle (analysis)", vec![], true);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("worst frames: {:?}", &worst[..worst.len().min(8)]);
    let n = worst.len() as f64;
    println!("mean frame {:.1} ms over {} frames", worst.iter().map(|w| w.0).sum::<f64>() / n, worst.len());
}

#[test]
#[ignore]
fn analysis_costs() {
    use std::time::Instant;
    let Some(p) = std::env::var_os("INPLITE_PERF") else { return };
    let p = std::path::PathBuf::from(p);
    let pr = load::Progress::new();
    let l = load::load(&p, 2000, &pr).unwrap();
    let text = l.text.clone();
    let t = Instant::now();
    let mut a = analysis::analyze(&text, 1, p.parent().unwrap());
    println!("analyze {:.1} ms, {} blocks, {} diags, {} outline roots", t.elapsed().as_secs_f64() * 1e3, a.blocks.len(), a.diags.len(), a.outline.len());
    let t = Instant::now();
    a.live = syntax::check(&text);
    println!("check {:.1} ms, {} live diags", t.elapsed().as_secs_f64() * 1e3, a.live.len());
    let doc = crate::doc::Document::from_loaded(p.clone(), l);
    let mut tab = crate::tab::DocTab::new(doc);
    tab.analysis = Some(std::sync::Arc::new(a));
    let t = Instant::now();
    let d = tab.all_diags();
    println!("all_diags {:.1} ms ({})", t.elapsed().as_secs_f64() * 1e3, d.len());
    let t = Instant::now();
    let old = tab.analysis.take();
    drop(old);
    println!("drop analysis {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
}

/// Drive the editor widget with simulated input: typing, navigation, selection, undo/redo,
/// clipboard and completion (Enter and double-click).
#[test]
fn editor_widget() {
    use egui::{pos2, vec2, Event, Key, Modifiers, PointerButton, RawInput, Rect};
    let (_p, doc) = sample_doc("widget");
    let mut tab = crate::tab::DocTab::new(doc);
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let ctx = egui::Context::default();
    crate::app::setup_style(&ctx);
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
    let mut time = 0.0;
    let mut copied = String::new();
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>| {
        time += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(time), focused: true, ..Default::default() };
        let mut acts = Vec::new();
        let out = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                copied = t.clone();
            }
        }
        tab.poll(time);
        copied.clone()
    };
    let key = |k: Key, m: Modifiers| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
    let none = Modifiers::NONE;
    frame(&mut tab, vec![]);
    // click on the first line to focus
    let g = tab.geom.clone();
    let p = pos2(g.origin.x + 2.0, g.origin.y + g.row_h * 0.5);
    frame(&mut tab, vec![Event::PointerMoved(p), Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: none }]);
    frame(&mut tab, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: none }]);
    assert_eq!(tab.sel, (0, 0));
    // type at the start of the file
    frame(&mut tab, vec![Event::Text("** hello".into()), key(Key::Enter, none)]);
    assert!(tab.doc.text.s.starts_with("** hello\n*Heading"), "{}", &tab.doc.text.s[..30]);
    // one undo removes the whole typed line (grouped)
    frame(&mut tab, vec![key(Key::Z, Modifiers::COMMAND)]);
    assert!(tab.doc.text.s.starts_with("** hello*Heading") || tab.doc.text.s.starts_with("*Heading"), "{}", &tab.doc.text.s[..30]);
    frame(&mut tab, vec![key(Key::Z, Modifiers::COMMAND)]);
    assert!(tab.doc.text.s.starts_with("*Heading"), "{}", &tab.doc.text.s[..30]);
    frame(&mut tab, vec![key(Key::Y, Modifiers::COMMAND)]);
    assert!(tab.doc.text.s.starts_with("** hello"), "{}", &tab.doc.text.s[..30]);
    frame(&mut tab, vec![key(Key::Z, Modifiers::COMMAND), key(Key::Z, Modifiers::COMMAND)]);
    assert!(tab.doc.text.s.starts_with("*Heading"));
    // select the keyword with shift+ctrl+right and copy it
    frame(&mut tab, vec![key(Key::Home, Modifiers::COMMAND), key(Key::ArrowRight, none), key(Key::ArrowRight, Modifiers::COMMAND | Modifiers::SHIFT)]);
    let c = frame(&mut tab, vec![Event::Copy]);
    assert_eq!(c, "Heading");
    // down / end / backspace
    frame(&mut tab, vec![key(Key::ArrowDown, none), key(Key::End, none), key(Key::Backspace, none), Event::Text("T".into())]);
    assert!(tab.doc.text.s.starts_with("*Heading\nflow tesT\n"), "{}", &tab.doc.text.s[..30]);
    // paste with CRLF is normalised
    frame(&mut tab, vec![Event::Paste("A\r\nB".into())]);
    assert!(tab.doc.text.s.starts_with("*Heading\nflow tesTA\nB\n"), "{}", &tab.doc.text.s[..30]);
    // keyword completion: type "*Stat" on a new line, accept with Enter
    frame(&mut tab, vec![key(Key::Enter, none), Event::Text("*Stati".into())]);
    assert!(tab.completion.is_some(), "completion popup");
    frame(&mut tab, vec![key(Key::Enter, none)]);
    assert!(tab.doc.text.s.contains("\n*Static\n") || tab.doc.text.s.contains("\n*Static,"), "{}", &tab.doc.text.s[..60]);
    // completion by double-click
    frame(&mut tab, vec![key(Key::End, none), key(Key::Enter, none), Event::Text("*Densi".into())]);
    assert!(tab.completion.is_some());
    frame(&mut tab, vec![]);
    let r = tab.geom.pos_of(&tab, tab.completion.as_ref().unwrap().replace.0);
    let item = pos2(r.x + 40.0, r.y + tab.geom.row_h + 2.0 + 4.0 + 10.0);
    let down = |t: f64| Event::PointerButton { pos: item, button: PointerButton::Primary, pressed: t > 0.0, modifiers: none };
    frame(&mut tab, vec![Event::PointerMoved(item), down(1.0)]);
    frame(&mut tab, vec![down(0.0)]);
    frame(&mut tab, vec![down(1.0)]);
    frame(&mut tab, vec![down(0.0)]);
    frame(&mut tab, vec![]);
    assert!(tab.completion.is_none(), "accepted by double click");
    assert!(tab.doc.text.s.contains("\n*Density"), "{}", &tab.doc.text.s[..80]);
    // keyboard still goes to the editor after clicking the popup
    frame(&mut tab, vec![Event::Text("X".into())]);
    assert!(tab.doc.text.s.contains("\n*DensityX"), "{}", &tab.doc.text.s[..80]);
    // select all + delete + undo restores everything
    let before = tab.doc.text.s.clone();
    frame(&mut tab, vec![key(Key::A, Modifiers::COMMAND), key(Key::Delete, none)]);
    assert!(tab.doc.text.s.is_empty());
    frame(&mut tab, vec![key(Key::Z, Modifiers::COMMAND)]);
    assert_eq!(tab.doc.text.s, before);
}

#[test]
fn split_to_other_folder() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let (p, doc) = sample_doc("otherdir");
    let dir = p.parent().unwrap().to_path_buf();
    let sub = dir.join("meshes");
    let other = std::env::temp_dir().join(format!("inplite other {}", std::process::id()));
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(crate::app::include_ref(&dir, &dir.join("a.inp")), "a.inp");
    assert_eq!(crate::app::include_ref(&dir, &sub.join("a.inp")), "meshes/a.inp");
    let r = crate::app::include_ref(&dir, &other.join("a.inp"));
    assert!(r.starts_with('"') && r.ends_with("a.inp\""), "{r}");
    let mut app = crate::app::App::for_test();
    let mut tab = crate::tab::DocTab::new(doc);
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let an = tab.analysis.clone().unwrap();
    let bi = an.blocks.iter().position(|b| b.norm == "NODE").unwrap();
    let crate::actions::Action::SplitBlock { ranges, .. } = crate::outline::block_split_action(&an, bi, "model") else { panic!() };
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    app.start_split(vec![crate::app::SplitEdit { ranges, inc: r.clone() }]);
    app.wait_file_op();
    let crate::app::Tab::Doc(d) = &app.tabs[0] else { panic!() };
    assert!(d.doc.text.s.contains(&format!("*Include, input={r}\n")));
    assert!(other.join("a.inp").exists());
    // the analysis resolves the quoted absolute path and finds the definitions inside
    let an2 = crate::analysis::analyze(&d.doc.text.s, 99, &dir);
    assert!(an2.includes.iter().any(|i| i.exists), "{:?}", an2.includes.iter().map(|i| i.path.clone()).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&other);
}

#[test]
#[ignore]
fn jump_centering() {
    use egui::{pos2, vec2, RawInput, Rect};
    let Some(p) = std::env::var_os("INPLITE_PERF") else { return };
    let pr = load::Progress::new();
    let l = load::load(std::path::Path::new(&p), 2000, &pr).unwrap();
    let doc = crate::doc::Document::from_loaded(std::path::PathBuf::from(&p), l);
    let mut tab = crate::tab::DocTab::new(doc);
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
    let mut t = 0.0;
    let mut frame = |tab: &mut crate::tab::DocTab| {
        t += 0.05;
        let raw = RawInput { screen_rect: Some(screen), time: Some(t), ..Default::default() };
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
    };
    frame(&mut tab);
    for line in [100_000usize, 100_004, 50, 120_000, 120_010, 5] {
        tab.jump_to_line(line, None);
        for _ in 0..3 {
            frame(&mut tab);
        }
        let g = &tab.geom;
        let y = g.origin.y + line as f32 * g.row_h;
        println!("line {line}: y {:.0} view {:.0}..{:.0} (center {:.0})", y, g.view.top(), g.view.bottom(), g.view.center().y);
    }
}

#[test]
fn editor_non_ascii_and_tabs() {
    use egui::{pos2, vec2, Event, Key, Modifiers, RawInput, Rect};
    let doc_text = "*Heading\n** température °C\tfin\n*Node\n1,\t0., 0.\n";
    let dir = std::env::temp_dir().join(format!("inplite_na_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("na.inp");
    std::fs::write(&p, doc_text).unwrap();
    let pr = load::Progress::new();
    let l = load::load(&p, 2000, &pr).unwrap();
    let mut tab = crate::tab::DocTab::new(crate::doc::Document::from_loaded(p.clone(), l));
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0));
    let mut t = 0.0;
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>| {
        t += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(t), focused: true, ..Default::default() };
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
    };
    frame(&mut tab, vec![]);
    // every char boundary of line 1 maps to a screen x and back
    let ls = tab.map.starts[1];
    let le = tab.map.starts[2] - 1;
    let s = tab.doc.text.s.clone();
    for (i, _) in s[ls..le].char_indices().chain(std::iter::once((le - ls, ' '))) {
        let b = ls + i;
        let pos = tab.geom.pos_of(&tab, b);
        let back = tab.geom.byte_at(&tab, pos + vec2(0.5, tab.geom.row_h * 0.5));
        assert_eq!(back, b, "round trip at byte {b}");
    }
    // arrows over multi-byte chars keep char boundaries; typing after them works
    tab.jump_to_line(1, Some((ls + 6, ls + 6)));
    frame(&mut tab, vec![]);
    let k = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    frame(&mut tab, vec![k(Key::ArrowRight), k(Key::ArrowRight), k(Key::ArrowRight), Event::Text("X".into())]);
    assert!(tab.doc.text.s.contains("** tempérXature"), "{}", tab.doc.text.s);
    frame(&mut tab, vec![k(Key::ArrowDown), k(Key::ArrowDown), k(Key::Backspace), k(Key::Backspace)]);
    assert!(tab.doc.text.s.is_char_boundary(tab.sel.1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn placeholder_lines_read_only() {
    use egui::{pos2, vec2, Event, Key, Modifiers, RawInput, Rect};
    let (_p, doc) = sample_doc("ro");
    let mut tab = crate::tab::DocTab::new(doc);
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 500.0));
    let mut t = 0.0;
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>| {
        t += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(t), focused: true, ..Default::default() };
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
        acts.iter().any(|a| matches!(a, crate::actions::Action::Toast(_)))
    };
    let k = |key, m| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m };
    let none = Modifiers::NONE;
    tab.ensure_map();
    let ph = (0..tab.map.starts.len()).find(|&l| tab.line_text(l).starts_with(crate::store::PH_PREFIX)).unwrap();
    let ls = tab.map.starts[ph];
    let before = tab.doc.text.s.clone();
    // typing inside / at the end, backspace at the start, delete at the end: refused
    tab.jump_to_line(ph, Some((ls + 5, ls + 5)));
    frame(&mut tab, vec![]);
    assert!(frame(&mut tab, vec![Event::Text("x".into())]));
    frame(&mut tab, vec![k(Key::End, none), Event::Text("x".into()), k(Key::Delete, none), k(Key::Home, none), k(Key::Home, none), k(Key::Backspace, none)]);
    assert_eq!(tab.doc.text.s, before);
    // Enter at the start / end of the line is fine (adds a line)
    frame(&mut tab, vec![k(Key::Enter, none)]);
    assert_eq!(tab.doc.text.s.len(), before.len() + 1);
    frame(&mut tab, vec![k(Key::Z, Modifiers::COMMAND)]);
    assert_eq!(tab.doc.text.s, before);
    // deleting the whole line (triple-click selection = line + newline) is allowed
    let le = tab.map.starts[ph + 1];
    tab.jump_to_line(ph, Some((ls, le)));
    frame(&mut tab, vec![]);
    frame(&mut tab, vec![k(Key::Delete, none)]);
    assert_eq!(tab.doc.text.s.len(), before.len() - (le - ls));
}

#[test]
#[ignore]
fn venza_probe() {
    let Some(p) = std::env::var_os("INPLITE_PERF") else { return };
    let p = std::path::PathBuf::from(p);
    let pr = load::Progress::new();
    let t = std::time::Instant::now();
    let l = load::load(&p, 2000, &pr).unwrap();
    println!("load {:.2}s skeleton {} lines, {} bulks, threshold {}", t.elapsed().as_secs_f64(), l.text.lines().count(), l.bulks.len(), l.threshold);
    let an = analysis::analyze(&l.text, 1, p.parent().unwrap());
    println!("assembly_based {} blocks {} diags {}", an.summary.assembly_based, an.blocks.len(), an.diags.len());
    for d in an.diags.iter().take(10) { println!("  {:?} L{} {}", d.sev, d.line, d.msg); }
    for n in &an.outline {
        let r = if n.label.starts_with("Part") { crate::outline::mesh_ranges(&an, n.block) } else { vec![] };
        println!("{:<30} kids {:>5} mesh_ranges {:?}", n.label, n.children.len(), r.iter().map(|&(s, e)| (s, e, l.text[s..e].lines().count())).collect::<Vec<_>>());
        if n.label.starts_with("Part") { for c in n.children.iter().take(6) { println!("    {} {:?} kids {}", c.label, c.block, c.children.len()); } }
    }
}

/// Replace every "*Include, input=..." line by the file content (test helper).
fn inline_includes(text: &str, dir: &std::path::Path) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for line in text.split_inclusive('\n') {
        let l = line.trim_end();
        if l.to_ascii_lowercase().starts_with("*include, input=") {
            let raw = &l["*include, input=".len()..];
            let p = crate::analysis::resolve_include(dir, raw);
            out.push_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Save all part meshes / all data blocks on a big Part/Assembly model and check that inlining
/// the include files gives back the original file byte for byte.
/// INPLITE_PERF=/path/venza_main.inp cargo test --release batch_split_big -- --ignored --nocapture
#[test]
#[ignore]
fn batch_split_big() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let Some(src) = std::env::var_os("INPLITE_PERF") else { return };
    let src = std::path::PathBuf::from(src);
    use crate::app::MeshSplit;
    for (mode, ms, min) in [("meshes", MeshSplit::Combined, None), ("meshes", MeshSplit::NodesElements, None), ("meshes", MeshSplit::AllFour, None), ("bulks", MeshSplit::Combined, Some(2000u64)), ("bulks", MeshSplit::Combined, Some(50u64)), ("bulks", MeshSplit::Combined, Some(5u64))] {
        let dir = std::env::temp_dir().join(format!("inplite_batch_{mode}_{ms:?}_{min:?}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inc")).unwrap();
        let p = dir.join("model.inp");
        std::fs::copy(&src, &p).unwrap();
        let orig = std::fs::read_to_string(&p).unwrap();
        let pr = load::Progress::new();
        let l = load::load(&p, 2000, &pr).unwrap();
        let mut tab = crate::tab::DocTab::new(crate::doc::Document::from_loaded(p.clone(), l));
        let t0 = std::time::Instant::now();
        while tab.analysis.is_none() && t0.elapsed().as_secs() < 60 {
            tab.poll(0.0);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut app = crate::app::App::for_test();
        app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
        let items = app.batch_items_for_test(mode == "meshes", ms, min);
        println!("{mode} {ms:?} {min:?}: {} items: {:?}", items.len(), items.iter().take(5).map(|i| (i.name.clone(), i.ranges.len())).collect::<Vec<_>>());
        let edits: Vec<crate::app::SplitEdit> = items.iter().map(|it| crate::app::SplitEdit { ranges: it.ranges.clone(), inc: crate::app::include_ref(&dir, &dir.join("inc").join(&it.name)) }).collect();
        let t = std::time::Instant::now();
        app.start_split(edits);
        app.wait_file_op();
        println!("  split {:.1}s, toasts {:?}", t.elapsed().as_secs_f64(), app.toasts.iter().map(|t| t.0.chars().take(120).collect::<String>()).collect::<Vec<_>>());
        app.save(0, false, true);
        app.wait_file_op();
        let main = std::fs::read_to_string(&p).unwrap();
        println!("  main file now {} bytes (was {})", main.len(), orig.len());
        let rebuilt = inline_includes(&main, &dir);
        let multi = items.iter().any(|i| i.ranges.len() > 1);
        if !multi {
            assert!(rebuilt == orig, "inlined includes differ from the original");
            println!("  inlined == original: OK");
        } else {
            assert_eq!(rebuilt.len(), orig.len());
            println!("  multi-range items: same total size OK");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn middle_button_autoscroll() {
    use egui::{pos2, vec2, Event, Modifiers, PointerButton, RawInput, Rect};
    let dir = std::env::temp_dir().join(format!("inplite_as_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("long.inp");
    let mut t = String::from("*Heading\n");
    for i in 0..3000 {
        t += &format!("** comment line {i}\n");
    }
    std::fs::write(&p, t).unwrap();
    let pr = load::Progress::new();
    let l = load::load(&p, 100000, &pr).unwrap();
    let mut tab = crate::tab::DocTab::new(crate::doc::Document::from_loaded(p.clone(), l));
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 500.0));
    let mut time = 0.0;
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>| {
        time += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(time), predicted_dt: 0.05, focused: true, ..Default::default() };
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
    };
    let btn = |b, pressed, p| Event::PointerButton { pos: p, button: b, pressed, modifiers: Modifiers::NONE };
    frame(&mut tab, vec![]);
    let a = pos2(400.0, 200.0);
    // hold and drag down: scrolls while held
    frame(&mut tab, vec![Event::PointerMoved(a), btn(PointerButton::Middle, true, a)]);
    assert!(tab.autoscroll.is_some());
    for _ in 0..10 {
        frame(&mut tab, vec![Event::PointerMoved(a + vec2(0.0, 150.0))]);
    }
    let first = tab.geom.first;
    assert!(first > 20, "scrolled down: first line {first}");
    frame(&mut tab, vec![btn(PointerButton::Middle, false, a + vec2(0.0, 150.0))]);
    assert!(tab.autoscroll.is_none(), "drag mode ends on release");
    // click (no move) = sticky mode; moving up scrolls back; a left click ends it without moving the caret
    let sel = tab.sel;
    frame(&mut tab, vec![Event::PointerMoved(a), btn(PointerButton::Middle, true, a)]);
    frame(&mut tab, vec![btn(PointerButton::Middle, false, a)]);
    assert!(tab.autoscroll.is_some_and(|s| s.sticky));
    for _ in 0..10 {
        frame(&mut tab, vec![Event::PointerMoved(a - vec2(0.0, 120.0))]);
    }
    assert!(tab.geom.first < first, "scrolled up: {} < {first}", tab.geom.first);
    let q = a - vec2(0.0, 120.0);
    frame(&mut tab, vec![btn(PointerButton::Primary, true, q)]);
    frame(&mut tab, vec![btn(PointerButton::Primary, false, q)]);
    assert!(tab.autoscroll.is_none());
    assert_eq!(tab.sel, sel, "the click that ends autoscroll does not move the caret");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shift_f12_finds_references() {
    use egui::{pos2, vec2, Event, Key, Modifiers, RawInput, Rect};
    let (_p, doc) = sample_doc("sf12");
    let mut tab = crate::tab::DocTab::new(doc);
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 500.0));
    let mut t = 0.0;
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>| {
        t += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(t), focused: true, ..Default::default() };
        let mut acts = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
        acts
    };
    let at = tab.doc.text.s.find("PLATE-1.FIX").unwrap() + 9;
    tab.ensure_map();
    tab.jump_to_line(tab.map.line_of_byte(at), Some((at, at)));
    frame(&mut tab, vec![]);
    let jump_before = tab.cursor.0;
    let acts = frame(&mut tab, vec![Event::Key { key: Key::F12, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::SHIFT }]);
    assert!(acts.iter().any(|a| matches!(a, crate::actions::Action::ShowRefs)), "Shift+F12 shows references");
    assert!(tab.refs.len() >= 2, "{}", tab.refs.len());
    assert!(tab.jump.is_none() && tab.cursor.0 == jump_before, "Shift+F12 must not jump to the definition");
}

/// Parts with *NGEN / *SYSTEM / several *Node and *Element blocks: the mesh is moved as one
/// contiguous span so that inlining the include gives back the original text exactly, and the
/// separate-file modes fall back to one file when the order matters.
#[test]
fn mesh_export_keeps_order() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let dir = std::env::temp_dir().join(format!("inplite_ngen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("m.inp");
    let mut t = String::from("*Heading\n*Part, name=P\n*Node\n1, 0., 0., 0.\n11, 10., 0., 0.\n*Ngen, nset=EDGE\n1, 11, 1\n*System\n0., 0., 5.\n*Node\n");
    for i in 100..400 {
        t += &format!("{i}, {i}., 1., 0.\n");
    }
    t += "*Element, type=T3D2, elset=A\n1, 1, 2\n2, 2, 3\n*Shell Section, elset=A, material=M\n1.\n*Element, type=T3D2, elset=B\n";
    for i in 3..300 {
        t += &format!("{i}, {}, {}\n", i + 97, i + 98);
    }
    t += "*Nset, nset=ALL, generate\n1, 11, 1\n*Solid Section, elset=B, material=M\n1.\n*End Part\n*Assembly, name=A\n*Instance, name=P-1, part=P\n*End Instance\n*End Assembly\n*Material, name=M\n*Elastic\n1., 0.3\n";
    std::fs::write(&p, &t).unwrap();
    let pr = load::Progress::new();
    let l = load::load(&p, 100, &pr).unwrap();
    let mut tab = crate::tab::DocTab::new(crate::doc::Document::from_loaded(p.clone(), l));
    let t0 = std::time::Instant::now();
    while tab.analysis.is_none() && t0.elapsed().as_secs() < 10 {
        tab.poll(0.0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut app = crate::app::App::for_test();
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    use crate::app::MeshSplit;
    for mode in [MeshSplit::NodesElements, MeshSplit::AllFour] {
        let items = app.batch_items_for_test(true, mode, None);
        assert_eq!(items.len(), 1, "{mode:?}: falls back to one file");
        assert!(items[0].label.contains("one file"), "{}", items[0].label);
    }
    let items = app.batch_items_for_test(true, MeshSplit::Combined, None);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].ranges.len(), 1, "one contiguous range");
    let edits = vec![crate::app::SplitEdit { ranges: items[0].ranges.clone(), inc: "m_P_mesh.inp".into() }];
    app.start_split(edits);
    app.wait_file_op();
    app.save(0, false, true);
    app.wait_file_op();
    let main = std::fs::read_to_string(&p).unwrap();
    let inc = std::fs::read_to_string(dir.join("m_P_mesh.inp")).unwrap();
    assert!(inc.starts_with("*Node\n1, 0., 0., 0.\n") && inc.contains("*Ngen") && inc.contains("*System") && inc.contains("*Shell Section"));
    assert!(inc.ends_with("*Nset, nset=ALL, generate\n1, 11, 1\n"), "sets right after the last mesh block: {}", &inc[inc.len() - 60..]);
    assert_eq!(main.replace("*Include, input=m_P_mesh.inp\n", &inc), t, "inlining gives back the original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selection_to_include_and_new_file() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let (p, doc) = sample_doc("selinc");
    let orig = std::fs::read_to_string(&p).unwrap();
    let mut tab = crate::tab::DocTab::new(doc);
    tab.ensure_map();
    // select from the middle of the "*Node" line to the middle of "*Nset" (spans several blocks + data blocks)
    let a = tab.doc.text.s.find("*Node").unwrap() + 2;
    let b = tab.doc.text.s.find("*Nset").unwrap() + 3;
    tab.sel = (a, b);
    let crate::actions::Action::SplitBlock { ranges, label, .. } = crate::editor::selection_split_action(&mut tab).unwrap() else { panic!() };
    let (s, e) = ranges[0];
    assert!(tab.doc.text.s[s..].starts_with("*Node\n"));
    assert!(tab.doc.text.s[..e].ends_with("*Nset, nset=FIX\n"), "{}", &tab.doc.text.s[e - 20..e]);
    assert!(label.contains("data blocks included"), "{label}");
    let mut app = crate::app::App::for_test();
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    app.start_split(vec![crate::app::SplitEdit { ranges, inc: "sel.inp".into() }]);
    app.wait_file_op();
    app.save(0, false, true);
    app.wait_file_op();
    let main = std::fs::read_to_string(&p).unwrap();
    let inc = std::fs::read_to_string(p.with_file_name("sel.inp")).unwrap();
    assert_eq!(main.replace("*Include, input=sel.inp\r\n", &inc), orig);
    // File → New: untitled document, the first save goes through "Save As"
    app.new_file();
    let n = app.tabs.len() - 1;
    let crate::app::Tab::Doc(d) = &app.tabs[n] else { panic!() };
    assert!(d.doc.path.is_none() && d.doc.text.s.starts_with("*Heading\n") && !d.doc.modified());
    let target = p.with_file_name("new_model.inp");
    app.save_to_for_test(n, target.clone());
    app.wait_file_op();
    assert!(std::fs::read_to_string(&target).unwrap().starts_with("*Heading"));
    let crate::app::Tab::Doc(d) = &app.tabs[n] else { panic!() };
    assert_eq!(d.doc.path.as_deref(), Some(target.as_path()));
}

fn wait_analysis(tab: &mut crate::tab::DocTab) {
    let t0 = std::time::Instant::now();
    while tab.current_analysis().is_none() {
        assert!(t0.elapsed().as_secs() < 20, "analysis");
        tab.poll(t0.elapsed().as_secs_f64() + 1.0);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn app_doc(app: &mut crate::app::App) -> &mut crate::tab::DocTab {
    let a = app.active;
    match &mut app.tabs[a] {
        crate::app::Tab::Doc(d) => d,
        _ => panic!("no doc"),
    }
}

#[test]
fn balance_keeps_end_pairs() {
    let (_p, doc) = sample_doc("balance");
    let an = analysis::analyze(&doc.text.s, 1, &doc.dir());
    let t = &doc.text.s;
    let blk = |kw: &str| an.blocks.iter().position(|b| b.disp.eq_ignore_ascii_case(kw)).unwrap();
    // *Part … *Nset (no *End Part): extended to *End Part
    let (s, e, n) = crate::outline::balance_span(&an, an.blocks[blk("*Part")].start, an.blocks[blk("*Nset")].end);
    assert!(t[s..e].starts_with("*Part") && t[..e].ends_with("*End Part\n"), "{n:?}");
    assert!(n.iter().any(|x| x.contains("*End Part")), "{n:?}");
    // *End Part … *End Assembly: *End Part stays in the main file
    let (s, e, n) = crate::outline::balance_span(&an, an.blocks[blk("*End Part")].start, an.blocks[blk("*End Assembly")].end);
    assert!(t[s..e].starts_with("*Assembly") && t[..e].ends_with("*End Assembly\n"), "{:?} {n:?}", &t[s..e]);
    // *Static … *End Step: the *End Step stays
    let (s, e, _) = crate::outline::balance_span(&an, an.blocks[blk("*Static")].start, an.blocks[blk("*End Step")].end);
    assert!(t[s..e].starts_with("*Static") && t[..e].ends_with("ENCASTRE\n"), "{:?}", &t[s..e]);
    // *Elastic … *Static: the *Step pulls in its *End Step
    let (s, e, _) = crate::outline::balance_span(&an, an.blocks[blk("*Elastic")].start, an.blocks[blk("*Static")].end);
    assert!(t[s..e].starts_with("*Elastic") && t[..e].ends_with("*End Step\n"));
    // a range ending inside a part after *End Part of another: extends back
    let (s, e, _) = crate::outline::balance_span(&an, an.blocks[blk("*Solid Section")].start, an.blocks[blk("*Instance")].end);
    assert!(t[s..e].starts_with("*Part") && t[..e].ends_with("*End Assembly\n"), "{:?}", &t[s..e]);
}

#[test]
fn comment_part_roundtrip() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let (p, doc) = sample_doc("comment");
    let orig = std::fs::read_to_string(&p).unwrap();
    let mut tab = crate::tab::DocTab::new(doc);
    wait_analysis(&mut tab);
    let an = tab.analysis.clone().unwrap();
    let part = an.outline.iter().find(|n| n.label == "Part  PLATE").unwrap();
    let (s, e) = crate::outline::row_range(&an, part).unwrap();
    let mut app = crate::app::App::for_test();
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    app.comment_ranges(vec![(s, e)], crate::comment::Mode::Comment);
    app.wait_file_op();
    let d = app_doc(&mut app);
    assert!(d.doc.text.s.contains("\n***Part, name=PLATE\n***Node\n"), "{}", &d.doc.text.s[..200]);
    assert!(d.doc.text.s.contains("\n***End Part\n*Assembly"));
    assert!(d.doc.missing_bulks().is_empty());
    wait_analysis(d);
    let an = d.analysis.clone().unwrap();
    assert!(!an.blocks.iter().any(|b| b.norm == "PART"));
    assert_eq!(an.commented.len(), 1, "{:?}", an.commented);
    assert!(an.commented[0].label.starts_with("*Part, name=PLATE"));
    assert!(an.outline.iter().any(|n| n.kind == crate::analysis::NodeKind::Commented));
    // save: every line of the part has ** in front
    app.save(0, false, true);
    app.wait_file_op();
    let saved = std::fs::read_to_string(&p).unwrap();
    let expect: String = orig
        .split_inclusive("\r\n")
        .map(|l| l.to_string())
        .scan(false, |inpart, l| {
            if l.starts_with("*Part") {
                *inpart = true;
            }
            let out = if *inpart { format!("**{l}") } else { l.clone() };
            if l.starts_with("*End Part") {
                *inpart = false;
            }
            Some(out)
        })
        .collect();
    assert_eq!(saved, expect);
    // read it again: the commented part is one block of comment lines on disk
    let pr = load::Progress::new();
    let l = load::load(&p, 100, &pr).unwrap();
    assert_eq!(l.bulks.len(), 1);
    assert!(crate::store::STORE.get(l.bulks[0]).unwrap().comment);
    let doc2 = crate::doc::Document::from_loaded(p.clone(), l);
    let mut tab2 = crate::tab::DocTab::new(doc2);
    wait_analysis(&mut tab2);
    let an2 = tab2.analysis.clone().unwrap();
    assert_eq!(an2.commented.len(), 1);
    let (cs, ce) = (an2.commented[0].start, an2.commented[0].end);
    let mut app2 = crate::app::App::for_test();
    app2.tabs.push(crate::app::Tab::Doc(Box::new(tab2)));
    app2.comment_ranges(vec![(cs, ce)], crate::comment::Mode::Uncomment);
    app2.wait_file_op();
    let d2 = app_doc(&mut app2);
    assert!(d2.doc.text.s.contains("\n*Part, name=PLATE\n*Node\n**@@BULK"), "{}", &d2.doc.text.s[..300]);
    app2.save(0, false, true);
    app2.wait_file_op();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), orig, "uncommenting gives back the original");
    // undo of a comment command is one step
    let d2 = app_doc(&mut app2);
    let before = d2.doc.text.s.clone();
    let (a, b) = (d2.doc.text.s.find("*Material").unwrap(), d2.doc.text.s.find("*Step").unwrap());
    app2.comment_ranges(vec![(a, b)], crate::comment::Mode::Toggle);
    let d2 = app_doc(&mut app2);
    assert!(d2.doc.text.s.contains("***Material, name=STEEL\n***Elastic\n**210000., 0.3\n*Step"));
    d2.doc.text.undo();
    assert_eq!(d2.doc.text.s, before);
}

#[test]
fn rechunk_after_threshold_change() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let (_p, doc) = sample_doc("rechunk");
    let mut app = crate::app::App::for_test();
    app.tabs.push(crate::app::Tab::Doc(Box::new(crate::tab::DocTab::new(doc))));
    // an unsaved change survives
    {
        let d = app_doc(&mut app);
        let at = d.doc.text.s.find("flow test").unwrap();
        d.doc.text.replace_range(at..at + 4, "FLOW");
    }
    app.settings.bulk_threshold = 1000;
    app.rechunk_for_test();
    let d = app_doc(&mut app);
    assert!(d.doc.text.s.contains("FLOW test"));
    assert!(d.doc.modified());
    assert!(!d.doc.text.s.contains("**@@BULK"), "300 lines < 1000: as text now");
    assert!(d.doc.text.s.contains("\n299, 299, 300\n"));
    assert!(d.doc.path.as_ref().unwrap().ends_with("model.inp"));
    app.settings.bulk_threshold = 50;
    app.rechunk_for_test();
    let d = app_doc(&mut app);
    assert_eq!(d.doc.text.s.matches("**@@BULK").count(), 2);
    assert!(d.doc.text.s.contains("FLOW test"));
}

#[test]
fn rectangular_selection() {
    use egui::{pos2, vec2, Event, Key, Modifiers, PointerButton, RawInput, Rect};
    let (_p, doc) = sample_doc("rect");
    let mut tab = crate::tab::DocTab::new(doc);
    wait_analysis(&mut tab);
    let ctx = egui::Context::default();
    crate::app::setup_style(&ctx);
    let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
    let mut time = 0.0;
    let mut copied = String::new();
    let mut frame = |tab: &mut crate::tab::DocTab, events: Vec<Event>, m: Modifiers| {
        time += 0.05;
        let raw = RawInput { screen_rect: Some(screen), events, time: Some(time), focused: true, modifiers: m, ..Default::default() };
        let mut acts = Vec::new();
        let out = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::editor::editor_ui(ui, tab, 13.0, &mut acts));
        });
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                copied = t.clone();
            }
        }
        copied.clone()
    };
    let alt = Modifiers { alt: true, ..Default::default() };
    let none = Modifiers::NONE;
    frame(&mut tab, vec![], none);
    // the editor text: line 4.. are "1, 1., 0., 0." etc? (sample: *Heading, flow test, *Part, *Node, placeholder)
    // use lines 0..=2: "*Heading", "flow test", "*Part, name=PLATE"
    let g = tab.geom.clone();
    let at = |line: usize, col: usize| pos2(g.origin.x + col as f32 * g.char_w + 1.0, g.origin.y + g.row_h * (line as f32 + 0.5));
    let (p0, p1) = (at(0, 1), at(2, 4));
    frame(&mut tab, vec![Event::PointerMoved(p0), Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: alt }], alt);
    frame(&mut tab, vec![Event::PointerMoved(at(1, 3))], alt);
    frame(&mut tab, vec![Event::PointerMoved(p1)], alt);
    frame(&mut tab, vec![Event::PointerButton { pos: p1, button: PointerButton::Primary, pressed: false, modifiers: alt }], alt);
    let r = tab.rect.expect("rectangle");
    assert_eq!(r.bounds(), (0, 2, 1, 4), "{r:?}");
    let c = frame(&mut tab, vec![Event::Copy], none);
    assert_eq!(c, "Hea\nlow\nPar");
    let key = |k: Key, m: Modifiers| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
    frame(&mut tab, vec![key(Key::Num3, Modifiers::SHIFT), Event::Text("#".into())], none);
    assert!(tab.doc.text.s.starts_with("*#ding\nf# test\n*#t, name=PLATE\n"), "{}", &tab.doc.text.s[..40]);
    // a column of carets now: typing inserts on each line
    frame(&mut tab, vec![Event::Text("ab".into())], none);
    assert!(tab.doc.text.s.starts_with("*#abding\nf#ab test\n*#abt, name"), "{}", &tab.doc.text.s[..40]);
    frame(&mut tab, vec![key(Key::Backspace, none), key(Key::Backspace, none), key(Key::Backspace, none)], none);
    assert!(tab.doc.text.s.starts_with("*ding\nf test\n*t, name"), "{}", &tab.doc.text.s[..40]);
    // undo restores in steps
    tab.doc.text.undo();
    assert!(tab.doc.text.s.starts_with("*#ding\n"), "{}", &tab.doc.text.s[..40]);
    // Alt+Shift+Down extends from the caret; Escape ends it
    frame(&mut tab, vec![key(Key::Escape, none)], none);
    assert!(tab.rect.is_none());
    let sh = Modifiers { alt: true, shift: true, ..Default::default() };
    frame(&mut tab, vec![key(Key::ArrowDown, sh), key(Key::ArrowRight, sh)], sh);
    let r = tab.rect.expect("keyboard rectangle");
    assert_eq!(r.head.0, r.anchor.0 + 1);
}

/// INPLITE_PROBE=/path/model.inp cargo test probe_comment -- --ignored --nocapture
#[test]
#[ignore]
fn probe_comment() {
    std::env::set_var("INPLITE_NO_SETTINGS", "1");
    let Some(p) = std::env::var_os("INPLITE_PROBE") else { return };
    let p = std::path::PathBuf::from(p);
    let pr = load::Progress::new();
    let l = load::load(&p, 2000, &pr).unwrap();
    let doc = crate::doc::Document::from_loaded(p.clone(), l);
    let mut tab = crate::tab::DocTab::new(doc);
    wait_analysis(&mut tab);
    let an = tab.analysis.clone().unwrap();
    let part = std::env::var("INPLITE_PROBE_PART").unwrap_or("PUNCH".into());
    let s = an.blocks.iter().find(|b| b.norm == "PART" && b.param("NAME") == Some(part.as_str())).unwrap().start;
    let e = if part == "PUNCH" { an.blocks.iter().find(|b| b.norm == "AMPLITUDE").unwrap().end } else { s + 1 };
    let t0 = std::time::Instant::now();
    let (s, e, notes) = crate::outline::balance_span(&an, s, e);
    println!("notes {notes:?}");
    let mut app = crate::app::App::for_test();
    app.tabs.push(crate::app::Tab::Doc(Box::new(tab)));
    app.comment_ranges(vec![(s, e)], crate::comment::Mode::Comment);
    app.wait_file_op();
    println!("commented in {:.1} s", t0.elapsed().as_secs_f32());
    let d = app_doc(&mut app);
    let t = d.doc.text.s.clone();
    let regs = crate::analysis::comment_regions(&t);
    for r in &regs {
        let next: String = t[r.end..].chars().take(300).collect();
        println!("REGION {:?} lines {} end-> {:?}", r.label, r.lines, next.lines().take(3).collect::<Vec<_>>());
    }
}
