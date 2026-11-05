// Hide the console window on Windows release builds.
// Draft: a few fields are kept for features that are planned but not wired up yet.
#![allow(dead_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod analysis;
mod app;
mod diff;
mod codeview;
mod comment;
mod filedlg;
mod instance;
mod theme;
mod diffview;
mod doc;
mod editor;
mod lineview;
mod load;
mod logview;
mod mesh;
mod outline;
mod panels;
mod schema;
mod settings;
#[cfg(feature = "shots")]
mod shots;
mod store;
mod syntax;
mod tab;
mod textdiff;
mod util;
mod viewer;
#[cfg(test)]
mod corpus_test;

use std::sync::Arc;

fn icon() -> egui::IconData {
    let rgba = include_bytes!("../assets/icon64.rgba").to_vec();
    egui::IconData { rgba, width: 64, height: 64 }
}

fn show_error(title: &str, msg: &str) {
    eprintln!("{title}: {msg}");
    #[cfg(windows)]
    {
        let _ = rfd::MessageDialog::new().set_title(title).set_description(msg).set_level(rfd::MessageLevel::Error).show();
    }
}

/// Write a crash report (and show it on Windows, where there is no console).
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let path = std::env::temp_dir().join("InpLite-crash.txt");
        let msg = format!("InpLite {} stopped unexpectedly:\n{info}", env!("CARGO_PKG_VERSION"));
        let _ = std::fs::write(&path, format!("{msg}\n\n{}", std::backtrace::Backtrace::force_capture()));
        show_error("InpLite", &format!("{msg}\n\nDetails: {}", path.display()));
    }));
}

fn main() -> eframe::Result {
    install_panic_hook();
    let msgs = instance::parse_args(std::env::args_os().skip(1));
    #[cfg(feature = "shots")]
    if let Some(d) = std::env::var_os("INPLITE_SELFTEST") {
        let files: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(std::path::PathBuf::from).filter(|p| p.is_file()).collect();
        let ok = shots::selftest(std::path::Path::new(&d), &files[0]);
        std::process::exit(if ok { 0 } else { 1 });
    }
    // one window: hand the files to a running InpLite if there is one
    let new_window = settings::Settings::load().new_window || std::env::args_os().any(|a| a == "--new-window");
    let mut primary = None;
    if !new_window {
        let ask: Vec<instance::Msg> = if msgs.is_empty() { vec![instance::Msg::Show] } else { msgs.clone() };
        match instance::start(&ask) {
            instance::Start::HandedOver => return Ok(()),
            instance::Start::Primary(l, t) => primary = Some((l, t)),
            instance::Start::Alone => {}
        }
    }
    let size = if cfg!(feature = "shots") { [1600.0, 1000.0] } else { [1360.0, 860.0] };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("InpLite")
            .with_inner_size(size)
            .with_min_inner_size([640.0, 400.0])
            .with_icon(Arc::new(icon()))
            .with_drag_and_drop(true),
        vsync: true,
        ..Default::default()
    };
    // warm up the keyword schema off the UI thread
    std::thread::spawn(|| {
        let _ = schema::SCHEMA.kws.len();
    });
    let r = eframe::run_native("InpLite", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, msgs, primary)))));
    if let Err(e) = &r {
        show_error(
            "InpLite could not start",
            &format!("{e}\n\nInpLite needs OpenGL 2.0 or newer. Updating the graphics driver usually fixes this \
                      (on virtual machines / remote desktops, enable 3D acceleration)."),
        );
    }
    r
}
