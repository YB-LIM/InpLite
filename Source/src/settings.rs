//! Persistent user settings and native file dialogs.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// Data runs longer than this many lines stay on disk (shown as one placeholder line).
    pub bulk_threshold: u64,
    pub keep_backup: bool,
    pub font_size: f32,
    pub show_outline: bool,
    pub show_bottom: bool,
    pub recent: Vec<String>,
    /// default folder for new *INCLUDE files
    pub include_folder: IncludeFolder,
    /// sub-folder name (IncludeFolder::SubFolder)
    pub include_subdir: String,
    /// fixed folder (IncludeFolder::Fixed)
    pub include_fixed: String,
    /// dark colour theme
    pub dark: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum IncludeFolder {
    /// the folder of the main input file
    SameAsInput,
    /// a sub-folder of the input file's folder (relative *INCLUDE paths)
    SubFolder,
    /// always the same folder (full paths in *INCLUDE)
    Fixed,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            bulk_threshold: 2000,
            keep_backup: true,
            font_size: 13.0,
            show_outline: true,
            show_bottom: true,
            recent: Vec::new(),
            include_folder: IncludeFolder::SameAsInput,
            include_subdir: "include".into(),
            include_fixed: String::new(),
            dark: false,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("InpLite").join("settings.json"))
}

/// Settings written by version 0.2.0, which was briefly called "Abapad++".
fn old_config_path() -> Option<PathBuf> {
    let p = config_path()?;
    Some(p.parent()?.parent()?.join("Abapad++").join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        if std::env::var_os("INPLITE_NO_SETTINGS").is_some() {
            return Settings::default();
        }
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| old_config_path().and_then(|p| std::fs::read_to_string(p).ok()))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        if std::env::var_os("INPLITE_NO_SETTINGS").is_some() {
            return;
        }
        if let Some(p) = config_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(s) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, s);
            }
        }
    }
    /// Default folder for new *INCLUDE files of a model in `doc_dir`.
    pub fn include_folder(&self, doc_dir: &std::path::Path) -> PathBuf {
        match self.include_folder {
            IncludeFolder::SameAsInput => doc_dir.to_path_buf(),
            IncludeFolder::SubFolder => {
                let sub = self.include_subdir.trim().trim_matches(['/', '\\']);
                if sub.is_empty() { doc_dir.to_path_buf() } else { doc_dir.join(sub) }
            }
            IncludeFolder::Fixed => {
                let f = self.include_fixed.trim();
                if f.is_empty() { doc_dir.to_path_buf() } else { PathBuf::from(f) }
            }
        }
    }
    pub fn push_recent(&mut self, p: &std::path::Path) {
        let s = p.to_string_lossy().into_owned();
        self.recent.retain(|r| r != &s);
        self.recent.insert(0, s);
        self.recent.truncate(12);
    }
}

#[cfg(windows)]
pub fn pick_open(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter("Abaqus input", &["inp", "INP"])
        .add_filter("All files", &["*"])
        .pick_file()
}

#[cfg(windows)]
pub fn pick_save(title: &str, dir: Option<&std::path::Path>, name: &str) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title(title).set_file_name(name).add_filter("Abaqus input", &["inp"]);
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.save_file()
}

#[cfg(windows)]
pub fn pick_folder(title: &str, dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title(title);
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.pick_folder()
}

#[cfg(not(windows))]
pub fn pick_folder(title: &str, dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut args = vec!["--file-selection".to_string(), "--directory".into(), "--title".into(), title.into()];
    if let Some(d) = dir {
        args.push("--filename".into());
        args.push(format!("{}/", d.display()));
    }
    let out = std::process::Command::new("zenity").args(&args).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

/// Non-Windows builds are for development only: use zenity if available.
#[cfg(not(windows))]
pub fn pick_open(title: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("zenity").args(["--file-selection", "--title", title]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

#[cfg(not(windows))]
pub fn pick_save(title: &str, dir: Option<&std::path::Path>, name: &str) -> Option<PathBuf> {
    let f = match dir {
        Some(d) => d.join(name).to_string_lossy().into_owned(),
        None => name.to_string(),
    };
    let out = std::process::Command::new("zenity")
        .args(["--file-selection", "--save", "--confirm-overwrite", "--title", title, "--filename", &f])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}
