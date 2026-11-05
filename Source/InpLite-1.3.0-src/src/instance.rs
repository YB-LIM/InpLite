//! One InpLite window: a file opened while InpLite is running (double-click in Explorer, "Open
//! with", a terminal) goes to the running window as a new tab, like Notepad++.
//!
//! The first process listens on a local TCP port (127.0.0.1 only) derived from the user name;
//! later processes hand their files over and exit. A random token, stored in the user's
//! settings folder, makes sure only this user's InpLite talks to it.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Open(PathBuf),
    /// compare this file with the next one that arrives (Explorer: two files → "Compare with InpLite")
    Compare(PathBuf),
    /// just show the window
    Show,
}

pub enum Start {
    /// another InpLite took the files: exit
    HandedOver,
    /// this is the first InpLite: serve the others
    Primary(TcpListener, String),
    /// no single-window mode (port taken by another program, settings say so, …)
    Alone,
}

fn user() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default()
}

fn port() -> u16 {
    let mut h = crate::util::Fnv::default();
    h.bytes(format!("InpLite-{}", user()).as_bytes());
    40_000 + (h.0 % 20_000) as u16
}

fn token_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("instance.token"))
}

fn read_token() -> Option<String> {
    std::fs::read_to_string(token_path()?).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn write_token(t: &str) {
    let Some(p) = token_path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, t);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

fn new_token() -> String {
    let mut h = crate::util::Fnv::default();
    h.bytes(format!("{:?}{}{}", Instant::now(), std::process::id(), crate::util::now_secs()).as_bytes());
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:016x}{:016x}", h.0, t as u64 ^ h.0.rotate_left(17))
}

/// Send messages to a running InpLite. True if it answered.
fn send(msgs: &[Msg]) -> bool {
    let Some(token) = read_token() else { return false };
    let addr = SocketAddr::from(([127, 0, 0, 1], port()));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    let mut out = format!("INPLITE 1 {token}\n");
    for m in msgs {
        match m {
            Msg::Open(p) => out.push_str(&format!("OPEN {}\n", p.display())),
            Msg::Compare(p) => out.push_str(&format!("COMPARE {}\n", p.display())),
            Msg::Show => out.push_str("SHOW\n"),
        }
    }
    out.push_str("END\n");
    if s.write_all(out.as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).is_ok() && reply.trim() == "OK"
}

/// Decide at start-up whether to run or to hand the files to a running InpLite.
pub fn start(msgs: &[Msg]) -> Start {
    if std::env::var_os("INPLITE_NEW_WINDOW").is_some() {
        return Start::Alone;
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port()));
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match TcpListener::bind(addr) {
            Ok(l) => {
                let token = new_token();
                write_token(&token);
                return Start::Primary(l, token);
            }
            Err(_) => {
                // taken: probably a running InpLite (which may still be starting)
                if send(msgs) {
                    return Start::HandedOver;
                }
                if Instant::now() > deadline {
                    return Start::Alone;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

/// Serve other InpLite processes; messages arrive on the returned channel (and wake the UI).
pub fn serve(l: TcpListener, token: String, ctx: egui::Context) -> Receiver<Msg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(s) = conn else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut w = match s.try_clone() {
                Ok(w) => w,
                Err(_) => continue,
            };
            let mut lines = BufReader::new(s).lines();
            let hello = lines.next().and_then(|l| l.ok()).unwrap_or_default();
            if hello != format!("INPLITE 1 {token}") {
                continue;
            }
            let mut got = Vec::new();
            for l in lines.map_while(Result::ok) {
                if l == "END" {
                    break;
                }
                if let Some(p) = l.strip_prefix("OPEN ") {
                    got.push(Msg::Open(PathBuf::from(p)));
                } else if let Some(p) = l.strip_prefix("COMPARE ") {
                    got.push(Msg::Compare(PathBuf::from(p)));
                } else if l == "SHOW" {
                    got.push(Msg::Show);
                }
            }
            let _ = w.write_all(b"OK\n");
            for m in got {
                let _ = tx.send(m);
            }
            ctx.request_repaint();
        }
    });
    rx
}

/// Command line: files to open, or `--compare a [b]`.
pub fn parse_args(args: impl Iterator<Item = std::ffi::OsString>) -> Vec<Msg> {
    let mut compare = false;
    let mut out = Vec::new();
    for a in args {
        if a == "--compare" {
            compare = true;
            continue;
        }
        if a == "--new-window" {
            continue;
        }
        let p = PathBuf::from(&a);
        if !p.is_file() {
            continue;
        }
        let p = std::fs::canonicalize(&p).map(strip_unc).unwrap_or(p);
        out.push(if compare { Msg::Compare(p) } else { Msg::Open(p) });
    }
    out
}

/// `\\?\C:\x` (canonicalize on Windows) -> `C:\x`
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(r) if !r.starts_with("UNC") => PathBuf::from(r),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hand_over() {
        std::env::set_var("INPLITE_NO_SETTINGS", "1");
        // a private port / token for the test
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let ctx = egui::Context::default();
        let rx = serve(l, "tok".into(), ctx);
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"INPLITE 1 tok\nOPEN /a/b.inp\nCOMPARE /c.inp\nEND\n").unwrap();
        let mut r = String::new();
        BufReader::new(s).read_line(&mut r).unwrap();
        assert_eq!(r.trim(), "OK");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Msg::Open("/a/b.inp".into()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Msg::Compare("/c.inp".into()));
        // a wrong token is ignored
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"INPLITE 1 bad\nOPEN /x.inp\nEND\n").unwrap();
        drop(s);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
