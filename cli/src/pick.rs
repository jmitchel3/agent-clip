//! `agent-clip pick`: terminal picker, the TUI twin of the menu bar app's ⌘⇧C panel.
//!
//! Draws on /dev/tty so stdout stays free: by default the chosen command is printed there
//! (fzf style, `cmd=$(agent-clip pick)`). With `--to-pane` it is typed into a Herdr pane instead,
//! and `--keep` leaves the picker open afterwards so it can live in a docked split.

use super::{detect_project, die, lock, parse, paths, pbcopy, py_strip, visible, Store, PROJECT};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const RESULTS: usize = 200;

// MARK: - Terminal

/// Raw mode plus the alternate screen on /dev/tty; both undone on drop.
struct Term {
    tty: File,
    orig: libc::termios,
}

impl Term {
    fn open() -> Term {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .unwrap_or_else(|e| die(&format!("pick needs a terminal: {e}")));
        let fd = tty.as_raw_fd();
        let mut orig: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
            die("pick needs a terminal");
        }
        let mut raw = orig;
        unsafe {
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(fd, libc::TCSANOW, &raw);
        }
        let mut t = Term { tty, orig };
        t.write("\x1b[?1049h\x1b[?25l");
        t
    }

    fn write(&mut self, s: &str) {
        let _ = self.tty.write_all(s.as_bytes());
        let _ = self.tty.flush();
    }

    fn size(&self) -> (usize, usize) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        unsafe { libc::ioctl(self.tty.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
        (
            if ws.ws_col == 0 { 80 } else { ws.ws_col as usize },
            if ws.ws_row == 0 { 24 } else { ws.ws_row as usize },
        )
    }

    /// Bytes typed within `timeout`, or none.
    fn read(&mut self, timeout: Duration) -> Vec<u8> {
        let mut pfd = libc::pollfd {
            fd: self.tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int) } <= 0 {
            return Vec::new();
        }
        let mut buf = [0u8; 512];
        match self.tty.read(&mut buf) {
            Ok(n) => buf[..n].to_vec(),
            Err(_) => Vec::new(),
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.write("\x1b[?25h\x1b[?1049l");
        unsafe { libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.orig) };
    }
}

#[derive(Debug, PartialEq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Enter,
    AltEnter,
    Esc,
    Backspace,
    Ctrl(u8),
    Char(char),
}

fn parse_keys(b: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            0x1b => match b.get(i) {
                Some(b'\r' | b'\n') => {
                    out.push(Key::AltEnter);
                    i += 1;
                }
                Some(b'[' | b'O') => {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && !(0x40..=0x7e).contains(&b[j]) {
                        j += 1;
                    }
                    if j >= b.len() {
                        break;
                    }
                    let key = match (&b[start..j], b[j]) {
                        (_, b'A') => Some(Key::Up),
                        (_, b'B') => Some(Key::Down),
                        (_, b'C') => Some(Key::Right),
                        (_, b'D') => Some(Key::Left),
                        (_, b'Z') => Some(Key::BackTab),
                        (b"5", b'~') => Some(Key::PageUp),
                        (b"6", b'~') => Some(Key::PageDown),
                        (b"13;3", b'u') => Some(Key::AltEnter),
                        (b"13", b'u') => Some(Key::Enter),
                        (b"27", b'u') => Some(Key::Esc),
                        _ => None,
                    };
                    out.extend(key);
                    i = j + 1;
                }
                _ => out.push(Key::Esc),
            },
            b'\r' | b'\n' => out.push(Key::Enter),
            b'\t' => out.push(Key::Tab),
            0x7f | 0x08 => out.push(Key::Backspace),
            c if c < 0x20 => out.push(Key::Ctrl(c + b'a' - 1)),
            _ => {
                let len = match c {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                let end = (i - 1 + len).min(b.len());
                if let Some(ch) = std::str::from_utf8(&b[i - 1..end]).ok().and_then(|s| s.chars().next()) {
                    out.push(Key::Char(ch));
                }
                i = end;
            }
        }
    }
    out
}

// MARK: - Model

struct Row {
    id: i64,
    ts: f64,
    project: String,
    note: String,
    cmd: String,
    copied: bool,
}

struct Picker {
    rows: Vec<Row>,
    home: Option<String>,
    scope: Option<String>,
    scopes: Vec<Option<String>>,
    query: String,
    items: Vec<usize>,
    sel: usize,
    top: usize,
}

impl Picker {
    fn load(&mut self, s: &Store) {
        let keep = self.items.get(self.sel).map(|&i| self.rows[i].id);
        self.rows = visible(s, false, None)
            .into_iter()
            .rev()
            .map(|(_, c)| Row {
                id: c.id(),
                ts: c.ts(),
                project: c.project().to_string(),
                note: c.note().to_string(),
                cmd: c.cmd().to_string(),
                copied: c.copies() > 0,
            })
            .collect();
        let mut names: Vec<String> = self.home.iter().cloned().collect();
        for r in &self.rows {
            if !names.contains(&r.project) {
                names.push(r.project.clone());
            }
        }
        self.scopes = names.into_iter().map(Some).chain([None]).collect();
        if !self.scopes.contains(&self.scope) {
            self.scope = None;
        }
        self.refilter();
        if let Some(pos) = keep.and_then(|id| self.items.iter().position(|&i| self.rows[i].id == id)) {
            self.sel = pos;
        }
    }

    fn refilter(&mut self) {
        self.sel = 0;
        self.top = 0;
        let q = self.query.trim();
        // Typing 34 or #34 jumps straight to that clip, in any project.
        if let Ok(id) = q.strip_prefix('#').unwrap_or(q).parse::<i64>() {
            if let Some(i) = self.rows.iter().position(|r| r.id == id) {
                self.items = vec![i];
                return;
            }
        }
        let q = q.to_lowercase();
        self.items = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| self.scope.as_ref().is_none_or(|s| &r.project == s))
            .filter(|(_, r)| {
                q.is_empty()
                    || [&r.cmd, &r.note, &r.project]
                        .iter()
                        .any(|f| f.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .take(RESULTS)
            .collect();
    }

    fn step_scope(&mut self, delta: isize) {
        let n = self.scopes.len() as isize;
        let i = self.scopes.iter().position(|s| *s == self.scope).unwrap_or(0) as isize;
        self.scope = self.scopes[((i + delta) % n + n) as usize % n as usize].clone();
        self.refilter();
    }

    fn pending(&self, scope: &Option<String>) -> usize {
        self.rows
            .iter()
            .filter(|r| !r.copied && scope.as_ref().is_none_or(|s| &r.project == s))
            .count()
    }

    fn selected(&self) -> Option<&Row> {
        self.items.get(self.sel).map(|&i| &self.rows[i])
    }
}

// MARK: - Drawing

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const ACCENT: &str = "\x1b[36m";
const SEL_BG: &str = "\x1b[48;5;237m";
const RESET: &str = "\x1b[0m";

/// One display line: control characters flattened, cut to `width` columns.
fn fit(s: &str, width: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if flat.chars().count() <= width {
        flat
    } else if width == 0 {
        String::new()
    } else {
        flat.chars().take(width - 1).collect::<String>() + "…"
    }
}

fn age(ts: f64) -> String {
    let s = (super::now() - ts).max(0.0) as u64;
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn draw(t: &mut Term, p: &mut Picker, hints: &str, status: Option<&str>) {
    let (w, h) = t.size();
    let mut out = String::from("\x1b[H");
    let line = |s: &str, out: &mut String| {
        out.push_str(s);
        out.push_str("\x1b[0m\x1b[K\r\n");
    };

    let search = if p.query.is_empty() {
        format!("{DIM}search, or #id{RESET}")
    } else {
        fit(&p.query, w.saturating_sub(4))
    };
    line(&format!(" {ACCENT}❯{RESET} {search}\x1b[7m \x1b[27m"), &mut out);

    // Tab strip; if it overflows, drop tabs from the left until the current one fits.
    let tabs: Vec<(String, bool)> = p
        .scopes
        .iter()
        .map(|s| {
            let name = s.as_deref().unwrap_or("All");
            let n = p.pending(s);
            let label = if n > 0 {
                format!(" {name} {n} ")
            } else {
                format!(" {name} ")
            };
            (label, *s == p.scope)
        })
        .collect();
    let cur = tabs.iter().position(|t| t.1).unwrap_or(0);
    let mut first = 0;
    let width_of = |a: usize, b: usize| tabs[a..=b].iter().map(|t| t.0.chars().count() + 1).sum::<usize>();
    while first < cur && width_of(first, cur) > w {
        first += 1;
    }
    // ‹ and › mark tabs scrolled out of view on either side.
    let mut strip = String::from(if first > 0 { "‹" } else { " " });
    let mut used = 1;
    for (k, (label, on)) in tabs.iter().enumerate().skip(first) {
        let n = label.chars().count();
        if used + n + 2 > w && k + 1 < tabs.len() || used + n > w {
            strip += &format!("{DIM}›{RESET}");
            break;
        }
        if *on {
            strip += &format!("{BOLD}\x1b[7m{ACCENT}{label}{RESET}");
        } else {
            strip += &format!("{DIM}{label}{RESET}");
        }
        strip.push(' ');
        used += n + 1;
    }
    line(&strip, &mut out);
    line(&format!("{DIM}{}", "─".repeat(w)), &mut out);

    let body = h.saturating_sub(4);
    let per_page = (body / 2).max(1);
    if p.sel < p.top {
        p.top = p.sel;
    } else if p.sel >= p.top + per_page {
        p.top = p.sel + 1 - per_page;
    }
    let mut drawn = 0;
    if p.items.is_empty() {
        let what = match &p.scope {
            _ if !p.query.trim().is_empty() => "no matches".to_string(),
            Some(s) => format!("nothing queued for {s}"),
            None => "nothing queued".to_string(),
        };
        line(&format!("  {DIM}{what}"), &mut out);
        drawn = 1;
    }
    for (k, &ri) in p.items.iter().enumerate().skip(p.top).take(per_page) {
        let r = &p.rows[ri];
        let on = k == p.sel;
        let (bg, bar) = if on {
            (SEL_BG, format!("{ACCENT}▌{RESET}{SEL_BG}"))
        } else {
            ("", " ".to_string())
        };
        let id = format!("#{}", r.id);
        let lines: Vec<&str> = py_strip(&r.cmd).lines().collect();
        let mut cmd = lines.first().copied().unwrap_or("").to_string();
        if lines.len() > 1 {
            cmd += " …";
        }
        let cmd = fit(&cmd, w.saturating_sub(8));
        let pad = |s: &str, used: usize| format!("{s}{}", " ".repeat(w.saturating_sub(used)));
        let a_used = 1 + 6 + cmd.chars().count();
        line(&format!("{bar}{DIM}{id:>5}{RESET}{bg} {}", pad(&cmd, a_used)), &mut out);

        let mut meta = Vec::new();
        if p.scope.is_none() {
            meta.push(r.project.clone());
        }
        if !r.note.is_empty() {
            meta.push(r.note.clone());
        }
        meta.push(age(r.ts));
        let dot = if r.copied { "✓" } else { "●" };
        let meta = fit(&meta.join(" · "), w.saturating_sub(10));
        let b_used = 1 + 6 + 2 + meta.chars().count();
        let dot_color = if r.copied { DIM } else { ACCENT };
        line(
            &format!("{bar}      {dot_color}{dot}{RESET}{bg} {DIM}{}", pad(&meta, b_used)),
            &mut out,
        );
        drawn += 2;
    }
    for _ in drawn..body {
        line("", &mut out);
    }
    let footer = match status {
        Some(s) => format!("{ACCENT}{}", fit(s, w)),
        None => format!("{DIM}{}", fit(hints, w)),
    };
    out += &format!("{footer}\x1b[0m\x1b[K");
    t.write(&out);
}

// MARK: - Actions

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Records a use of clip `id` and returns its command, re-reading the store under the lock.
fn take(id: i64) -> Option<String> {
    let p = paths();
    let _lock = lock(&p);
    let mut s = Store::load(&p);
    let i = s.clips().find(|(_, c)| c.id() == id).map(|(i, _)| i)?;
    s.record_copy(i);
    s.save(&p);
    Some(s.clip(i).cmd().to_string())
}

fn archive(id: i64) {
    let p = paths();
    let _lock = lock(&p);
    let mut s = Store::load(&p);
    let found = s.clips().find(|(_, c)| c.id() == id).map(|(i, _)| i);
    if let Some(i) = found {
        s.modify(i, |o| {
            o.insert("archived".into(), Value::Bool(true));
        });
        s.save(&p);
    }
}

/// Types `text` into a Herdr pane without pressing Enter. `pane.send_input` bracket-pastes when
/// the pane's program asked for it, so multi-line commands land whole instead of running line 1.
fn herdr_send(pane: &str, text: &str) -> Result<(), String> {
    let sock = std::env::var_os("HERDR_SOCKET_PATH").ok_or("HERDR_SOCKET_PATH is not set")?;
    let mut s = UnixStream::connect(&sock).map_err(|e| format!("herdr socket: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let req = json!({
        "id": "agent-clip",
        "method": "pane.send_input",
        "params": {"pane_id": pane, "text": text, "keys": []},
    });
    writeln!(s, "{req}").map_err(|e| format!("herdr socket: {e}"))?;
    let mut resp = String::new();
    BufReader::new(&s)
        .read_line(&mut resp)
        .map_err(|e| format!("herdr socket: {e}"))?;
    let v: Value = serde_json::from_str(&resp).map_err(|_| format!("herdr: bad reply {resp:?}"))?;
    match v.get("error") {
        Some(e) => Err(format!(
            "herdr: {}",
            e.get("message").and_then(Value::as_str).unwrap_or("send failed")
        )),
        None => Ok(()),
    }
}

/// Target pane and project folder from the context Herdr hands plugin commands.
fn herdr_context() -> (Option<String>, Option<PathBuf>) {
    let ctx: Value = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let str_of = |k: &str| {
        ctx.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let own = std::env::var("HERDR_PANE_ID").ok();
    let pane = str_of("focused_pane_id").filter(|p| Some(p) != own.as_ref());
    let cwd = str_of("focused_pane_cwd")
        .or_else(|| str_of("workspace_cwd"))
        .map(PathBuf::from);
    (pane, cwd)
}

// MARK: - Command

pub fn cmd_pick(args: &[String]) {
    let a = parse(
        args,
        &[
            PROJECT,
            ("", "cwd", true),
            ("", "to-pane", true),
            ("", "keep", false),
            ("", "herdr", false),
        ],
        false,
    );
    a.max_pos(0);
    let (ctx_pane, ctx_cwd) = if a.flag("herdr") { herdr_context() } else { (None, None) };
    let to_pane = a.val("to-pane").map(String::from).or(ctx_pane);
    let keep = a.flag("keep") && to_pane.is_some();
    let home = a.val("project").map(String::from).or_else(|| {
        let cwd = a
            .val("cwd")
            .map(PathBuf::from)
            .or(ctx_cwd)
            .or_else(|| std::env::current_dir().ok())?;
        Some(detect_project(&cwd))
    });

    let p = paths();
    let s = Store::load(&p);
    let mut pk = Picker {
        rows: Vec::new(),
        scope: None,
        scopes: Vec::new(),
        query: String::new(),
        items: Vec::new(),
        sel: 0,
        top: 0,
        home: home.clone(),
    };
    pk.load(&s);
    // Open on this folder's project when it has anything queued, otherwise on All.
    if home.as_ref().is_some_and(|h| pk.rows.iter().any(|r| &r.project == h)) {
        pk.scope = home;
        pk.refilter();
    }

    let enter = if to_pane.is_some() { "↩ insert" } else { "↩ select" };
    let close = if keep { "^c close" } else { "esc close" };
    let hints = format!("{enter}  ⌥↩/^x archive  ^y copy  tab project  {close}");

    let mut term = Term::open();
    let mut seen = mtime(&p.store);
    let mut size = term.size();
    let mut status: Option<(String, Instant)> = None;
    let mut chosen: Option<String> = None;
    let mut dirty = true;

    'run: loop {
        if status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(3))
        {
            status = None;
            dirty = true;
        }
        if dirty {
            draw(&mut term, &mut pk, &hints, status.as_ref().map(|(s, _)| s.as_str()));
            dirty = false;
        }
        let bytes = term.read(Duration::from_millis(400));
        if bytes.is_empty() {
            let m = mtime(&p.store);
            if m != seen {
                seen = m;
                pk.load(&Store::load(&p));
                dirty = true;
            }
            if term.size() != size {
                size = term.size();
                dirty = true;
            }
            continue;
        }
        dirty = true;
        for key in parse_keys(&bytes) {
            let sel = pk.selected().map(|r| r.id);
            match key {
                Key::Ctrl(b'c') => break 'run,
                Key::Esc if !keep => break 'run,
                Key::Esc => {
                    pk.query.clear();
                    pk.refilter();
                }
                Key::Up | Key::Ctrl(b'p') => pk.sel = pk.sel.saturating_sub(1),
                Key::Down | Key::Ctrl(b'n') => pk.sel = (pk.sel + 1).min(pk.items.len().saturating_sub(1)),
                Key::PageUp => pk.sel = pk.sel.saturating_sub(10),
                Key::PageDown => pk.sel = (pk.sel + 10).min(pk.items.len().saturating_sub(1)),
                Key::Tab => pk.step_scope(1),
                Key::BackTab => pk.step_scope(-1),
                Key::Right if pk.query.is_empty() => pk.step_scope(1),
                Key::Left if pk.query.is_empty() => pk.step_scope(-1),
                Key::Backspace => {
                    pk.query.pop();
                    pk.refilter();
                }
                Key::Ctrl(b'u') => {
                    pk.query.clear();
                    pk.refilter();
                }
                Key::Ctrl(b'w') => {
                    let t = pk.query.trim_end().len();
                    let cut = pk.query[..t].rfind(' ').map_or(0, |i| i + 1);
                    pk.query.truncate(cut);
                    pk.refilter();
                }
                Key::Char(c) => {
                    pk.query.push(c);
                    pk.refilter();
                }
                Key::AltEnter | Key::Ctrl(b'x') => {
                    if let Some(id) = sel {
                        archive(id);
                        status = Some((format!("archived #{id}"), Instant::now()));
                    }
                }
                Key::Ctrl(b'y') => {
                    if let Some(cmd) = sel.and_then(take) {
                        pbcopy(&cmd);
                        if !keep {
                            break 'run;
                        }
                        status = Some((format!("copied #{}", sel.unwrap()), Instant::now()));
                    }
                }
                Key::Enter => {
                    let Some(id) = sel else { continue };
                    let Some(cmd) = take(id) else { continue };
                    let text = cmd.trim_end_matches('\n').to_string();
                    match &to_pane {
                        None => {
                            chosen = Some(text);
                            break 'run;
                        }
                        Some(pane) => match herdr_send(pane, &text) {
                            Ok(()) if keep => status = Some((format!("inserted #{id} into {pane}"), Instant::now())),
                            Ok(()) => break 'run,
                            Err(e) => status = Some((e, Instant::now())),
                        },
                    }
                }
                _ => {}
            }
        }
        // Our own writes (copy, archive) change the store; pick them up now, not on the next tick.
        let m = mtime(&p.store);
        if m != seen {
            seen = m;
            pk.load(&Store::load(&p));
        }
    }
    drop(term);
    match chosen {
        Some(cmd) => super::emit(&format!("{cmd}\n")),
        None if to_pane.is_none() => std::process::exit(1),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            parse_keys(b"\x1b[A\x1b[B\x1b\r\x1b[Z\t\r\x7fa\xc3\xa9\x15\x1b"),
            vec![
                Key::Up,
                Key::Down,
                Key::AltEnter,
                Key::BackTab,
                Key::Tab,
                Key::Enter,
                Key::Backspace,
                Key::Char('a'),
                Key::Char('é'),
                Key::Ctrl(b'u'),
                Key::Esc,
            ]
        );
        assert_eq!(parse_keys(b"\x1b[13;3u\x1bOD"), vec![Key::AltEnter, Key::Left]);
    }

    #[test]
    fn fits() {
        assert_eq!(fit("a\tb", 10), "a b");
        assert_eq!(fit("abcdef", 4), "abc…");
    }
}
