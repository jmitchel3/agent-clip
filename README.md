# 📋 agent-clip

**When agents need you to run a command, it goes here instead of into some long message that's impossible to copy.**

Claude Code, Codex, and Grok all hit the same wall: they need *you* to run something (a login, a `sudo`, an interactive prompt), so they print it in a wall of terminal text. Then you try to select it, the line wraps, a quote goes missing, and the command breaks.

agent-clip gives every agent session one shared queue. Agents drop commands in. You grab them from your menu bar when you're ready.

```
 Claude ─┐                                    ┌─ ⌘⇧C  picker
 Codex  ─┼─▶ agent-clip add ─▶ clips.jsonl ─▶─┼─ toast on new command
 Grok   ─┘                                    ├─ ⌃⌥C  copy newest
                                              └─ ac    load onto your zsh prompt
```

## What you get

- **One clipboard for every agent.** All sessions write to `~/.agent-clip/clips.jsonl`.
- **Sorted by project automatically.** Each command is tagged with the git repo it came from (worktrees count as their main repo).
- **It knows where you are.** The picker opens on the project in your front Terminal tab.
- **Nothing gets mangled.** Agents pass commands through a quoted heredoc, so quotes, `$vars` and multi-line scripts arrive byte for byte.
- **A log of everything you've run.** Nothing is deleted. `agent-clip stats` surfaces the commands you keep running so you can turn them into aliases or proper tools.

## Getting a command

| How | What happens |
| --- | --- |
| **Toast** | A card pops up top-right when an agent queues something and stays until you use it. Click it to copy: it turns green and slowly fades. Hover to hold it, click again to re-copy, or hit ✕ to shatter it. |
| **⌘⇧C** or click the menu bar icon | A Spotlight-style picker. The newest command is preselected, so **⌘⇧C ↩** copies it. |
| **⌃⌥C** | Copies the newest command you haven't copied yet for the current project. No UI. |
| **`ac`** in your terminal | Puts the newest command for this folder onto your zsh prompt, ready to edit or run. No clipboard involved. `ac 12` loads clip #12. |

Inside the picker (each row shows its clip id, the `#12` agents tell you about):

| Key | Action |
| --- | --- |
| `↩` / click | copy |
| `⌘1` to `⌘9` | copy that row |
| `⌥↩` | archive (hide from the list; it stays in the log) |
| `←` `→` / `tab` | switch project tab (arrows work while the search box is empty) |
| type | search commands, notes and projects; `34` or `#34` jumps to clip #34 in any project |
| `esc` | close |

Right-click the menu bar icon for settings: project filter, toast on/off, bulk archive, open the log.

### In herdr

If you use [herdr](https://herdr.dev), `install.sh` links a plugin that brings the picker into the terminal. It opens on the project of the focused pane, and ↩ types the command into that pane (without pressing Enter) instead of copying it. Multi-line commands arrive as a single paste, so nothing runs until you hit Enter.

Bind it in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+y"
type = "plugin_action"
command = "agent-clipboard.pick"     # popup; closes after ↩

[[keys.command]]
key = "prefix+shift+y"
type = "plugin_action"
command = "agent-clipboard.panel"    # docked split that stays open
```

The docked panel types into the pane that was focused when you opened it, and updates live as agents queue commands. Keys: `↩` insert, `⌥↩` or `^x` archive, `^y` copy instead, `tab` / `←` `→` switch project, `esc` close the popup (clear the search in the panel), `^c` close. Without the keybindings, run `herdr plugin action invoke agent-clipboard.pick`.

To show how many commands are waiting in each space, add the `$clips` token to the sidebar rows:

```toml
[ui.sidebar.spaces]
rows = [["state_icon", "workspace", "$clips"], ["branch", "git_status"]]
```

A space counts the uncopied commands for every project open in its panes. The count refreshes whenever a command is queued, copied or archived (from the CLI, the picker or the menu bar app) and when panes, tabs or spaces open or change focus. `agent-clip herdr-sync` refreshes it by hand.

`agent-clip pick` also works on its own in any terminal: it prints the chosen command, so `cmd=$(agent-clip pick)` works.

## Install

Requires macOS 14+, the Xcode command line tools (`xcode-select --install`) and Rust (`brew install rust`).

```bash
git clone https://github.com/jmitchel3/agent-clip.git
cd agent-clip
./install.sh
echo "source $PWD/shell/agent-clip.zsh" >> ~/.zshrc   # optional: the `ac` command
```

`install.sh` builds `~/Applications/Agent Clip.app`, builds and installs `agent-clip` into `~/.local/bin`, and registers a LaunchAgent so the app starts at login. Re-run it after pulling changes.

The first time the picker reads your Terminal tab, macOS asks to let Agent Clip control Terminal. Allow it, or project detection falls back to showing all projects.

## Teach your agents

Paste this into `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, and `~/.grok/AGENTS.md`:

````markdown
## Commands for the user to run
- When the user needs to run a shell command themselves (logins, sudo, interactive tools, anything you can't run), queue it on the agent clipboard instead of printing it for them to copy.
- Run from the project directory, one command per call, using a quoted heredoc so nothing gets mangled:
  ```
  agent-clip add -n "what it does" <<'EOF'
  gh auth login --scopes repo
  EOF
  ```
- Project is auto-detected from the git repo (override with `-p name`). Plain bash, no `!` prefix. In your reply, name the clip in one line (e.g. "queued #12: gh auth login").
- `agent-clip stats` shows commands repeated across sessions; suggest an alias or script when something keeps coming up.
````

**Codex users:** the workspace sandbox blocks writes outside the repo. Allow the clipboard store in `~/.codex/config.toml`:

```toml
[sandbox_workspace_write]
writable_roots = ["/Users/you/.agent-clip"]
```

## CLI

```bash
agent-clip add -n "note" <<'EOF'      # queue a command (stdin), or: agent-clip add -- cmd args
                                      # an identical command still pending in the project is not queued twice
agent-clip ls [-p project] [-a]       # list (-a includes archived)
agent-clip show 12                    # print the full command
agent-clip copy [12]                  # copy to the macOS clipboard (default: newest)
agent-clip pull [12]                  # print and mark used (what `ac` calls)
agent-clip pick [-p project]          # terminal picker; prints the chosen command
agent-clip archive [ids] [--copied]   # hide from the menu, keep in the log
agent-clip projects                   # projects with clip counts
agent-clip stats [-p project] [-d 7]  # what you run most
```

## Find your next alias

Every add and every copy is logged, so after a week you have real data on what agents keep asking you to run:

```
$ agent-clip stats
Top subcommands:
     6  docker compose
     3  gh auth

Repeated command shapes (alias / script candidates):
     6  docker compose exec web python manage.py <str>
        projects: my-app, my-api
```

"Shapes" group commands that differ only in quoted strings, numbers or hashes. If one keeps showing up, it's a script waiting to be written.

## How it works

- **Store:** `~/.agent-clip/clips.jsonl`, one JSON object per clip (`id`, `ts`, `project`, `cwd`, `agent`, `note`, `cmd`, `copies`, `archived`). Writers share an `flock`; `add` appends, other writes rewrite atomically, and lines that fail to parse are kept verbatim. Set `AGENT_CLIP_DIR` to move it.
- **CLI:** `cli/`, a small Rust binary (about 3 ms per command). Project detection reads `.git` directly instead of spawning git.
- **App:** `app/AgentClip.swift`, a single-file AppKit + SwiftUI menu bar app. Hotkeys use Carbon `RegisterEventHotKey`, so no Accessibility permission is needed. Front-tab detection asks Terminal for the tab's tty with one raw Apple Event (no AppleScript interpreter loaded), then reads the shell's working directory with `libproc`. The app watches the store with kqueue rather than polling, and only reparses it when it changes.

## Uninstall

```bash
launchctl bootout gui/$(id -u)/com.agent-clip
rm -rf ~/Applications/"Agent Clip.app" ~/Library/LaunchAgents/com.agent-clip.plist ~/.local/bin/agent-clip
herdr plugin uninstall agent-clipboard   # if you use herdr
rm -rf ~/.agent-clip   # your clip history
```
