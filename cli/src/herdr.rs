//! Herdr integration over its local socket (newline-delimited JSON requests).
//!
//! `sync` publishes each space's pending clip count as the `$clips` workspace metadata token,
//! which herdr can show in its sidebar. It runs after every CLI write, from the plugin's event
//! hooks, and from the menu bar app after its own writes. Without a running herdr it is a no-op.

use super::{detect_project, paths, Store};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SOURCE: &str = "agent-clip";

/// `$HERDR_SOCKET_PATH` inside herdr, else the default session's socket if one is listening.
fn socket() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HERDR_SOCKET_PATH") {
        return Some(PathBuf::from(p));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("herdr/herdr.sock")).filter(|p| p.exists())
}

fn request(sock: &Path, method: &str, params: Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("herdr socket: {e}"))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let req = json!({"id": "agent-clip", "method": method, "params": params});
    writeln!(s, "{req}").map_err(|e| format!("herdr socket: {e}"))?;
    let mut resp = String::new();
    BufReader::new(&s)
        .read_line(&mut resp)
        .map_err(|e| format!("herdr socket: {e}"))?;
    let mut v: Value = serde_json::from_str(&resp).map_err(|_| format!("herdr: bad reply {resp:?}"))?;
    match v.get("error") {
        Some(e) => Err(format!(
            "herdr: {}",
            e.get("message").and_then(Value::as_str).unwrap_or("request failed")
        )),
        None => Ok(v["result"].take()),
    }
}

/// Types `text` into a pane without pressing Enter. `pane.send_input` bracket-pastes when the
/// pane's program asked for it, so multi-line commands land whole instead of running line 1.
pub fn send(pane: &str, text: &str) -> Result<(), String> {
    let sock = socket().ok_or("herdr is not running")?;
    request(
        &sock,
        "pane.send_input",
        json!({"pane_id": pane, "text": text, "keys": []}),
    )
    .map(|_| ())
}

/// Target pane and project folder from the context herdr hands plugin commands.
pub fn context() -> (Option<String>, Option<PathBuf>) {
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

/// Pending (uncopied, unarchived) clips per space, counting every project open in its panes.
pub fn sync() -> Result<(), String> {
    let Some(sock) = socket() else { return Ok(()) };
    let panes = request(&sock, "pane.list", json!({}))?;
    let mut spaces: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for pane in panes["panes"].as_array().into_iter().flatten() {
        let Some(ws) = pane["workspace_id"].as_str() else {
            continue;
        };
        let projects = spaces.entry(ws.to_string()).or_default();
        if let Some(cwd) = pane["cwd"].as_str().or(pane["foreground_cwd"].as_str()) {
            projects.insert(detect_project(Path::new(cwd)));
        }
    }
    let store = Store::load(&paths());
    let mut pending: HashMap<&str, usize> = HashMap::new();
    for (_, c) in store.clips().filter(|(_, c)| !c.archived() && c.copies() == 0) {
        *pending.entry(c.project()).or_default() += 1;
    }
    // Wall-clock millis keep seq increasing across the separate processes that report.
    let seq = (super::now() * 1000.0) as u64;
    for (ws, projects) in spaces {
        let n: usize = projects
            .iter()
            .map(|p| pending.get(p.as_str()).copied().unwrap_or(0))
            .sum();
        let value = (n > 0).then(|| format!("📋 {n}"));
        request(
            &sock,
            "workspace.report_metadata",
            json!({"workspace_id": ws, "source": SOURCE, "tokens": {"clips": value}, "seq": seq}),
        )?;
    }
    Ok(())
}

/// Shows the session a clip was queued from: a Herdr app terminal, else a herdr pane (its space,
/// its tab, then the pane). Returns which one it focused.
pub fn focus(clip: &Value) -> Result<&'static str, String> {
    if let Some(term) = clip["hc_terminal"].as_str() {
        return focus_app(term).map(|_| "herdr-app");
    }
    let pane = clip["herdr_pane"].as_str().ok_or("clip was not queued from herdr")?;
    let sock = clip["herdr_socket"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(socket)
        .ok_or("herdr is not running")?;
    let info = request(&sock, "pane.get", json!({"pane_id": pane}))?;
    let info = info.get("pane").unwrap_or(&info);
    if let Some(ws) = info["workspace_id"].as_str() {
        request(&sock, "workspace.focus", json!({"workspace_id": ws}))?;
    }
    if let Some(tab) = info["tab_id"].as_str() {
        request(&sock, "tab.focus", json!({"tab_id": tab}))?;
    }
    request(&sock, "pane.focus", json!({"pane_id": pane})).map(|_| "herdr")
}

/// `herdr-app do agent.focus` switches space and tab too; plain terminals take `pane.focus`.
fn focus_app(term: &str) -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    // The menu bar app runs this with launchd's bare PATH.
    let bin = [
        home.join(".local/bin/herdr-app"),
        PathBuf::from("/Applications/Herdr.app/Contents/MacOS/herdr-app"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .unwrap_or_else(|| PathBuf::from("herdr-app"));
    let run = |cmd: &str| {
        Command::new(&bin)
            .args(["do", cmd, "--terminal", term])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("herdr-app: {e}"))
    };
    let out = run("agent.focus")?;
    if out.status.success() {
        return Ok(());
    }
    let out = run("pane.focus")?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("herdr-app: {}", String::from_utf8_lossy(&out.stdout).trim()))
}

/// `agent-clip focus ID`: used by the menu bar app when a toast is clicked.
pub fn cmd_focus(args: &[String]) {
    let a = super::parse(args, &[], false);
    a.max_pos(1);
    let Some(id) = a.pos.first() else {
        super::usage_error("the following arguments are required: id")
    };
    let s = Store::load(&paths());
    match focus(&s.value(s.find(id))) {
        Ok(which) => println!("{which}"),
        Err(e) => super::die(&e),
    }
}

/// `agent-clip herdr-sync`: errors go to stderr, but never fail the hook that called it.
pub fn cmd_sync(args: &[String]) {
    super::parse(args, &[], false).max_pos(0);
    if let Err(e) = sync() {
        eprintln!("agent-clip: {e}");
    }
}
