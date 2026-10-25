<p align="center"><img src="logo-dithered.jpg" alt="ClaudeSyncTray logo" width="280"></p>

# ClaudeSyncTray

A GPU-accelerated Windows desktop dashboard (built with [GPUI](https://gpui.rs)) and
system-tray utility that keeps **multiple Claude Desktop accounts fully isolated**
on one machine. Each account gets its own data directory; switching accounts swaps
an NTFS junction and relaunches Claude.

- **GPUI Desktop Dashboard + Native System Tray** — manage profiles, settings, and logs visually, or switch directly from the tray.
- **Hide-to-tray workflow** — closing the dashboard window hides it to the system tray; left-clicking the tray icon toggles the window.
- **A logo that moves** — the dashboard title bar draws the logo's asterisk as a live ordered-dither animation, which speeds up while a switch is in progress.
- **Instant non-blocking switches** — profile changes re-point an NTFS junction on a background worker without freezing the UI.

> Windows only (NTFS junctions + DirectX/DirectWrite via GPUI). Built and tested with Rust 1.89+.

---

## How it works

Claude Desktop stores everything (session tokens, cookies, local storage,
`claude_desktop_config.json`) under one data directory. The app manages three
profiles by default:

```
%APPDATA%\ClaudeSyncTray\
├── config.json                  # settings (profiles, launch override, autostart)
├── claude-sync-tray.log         # everything is logged here
├── instance.lock                # single-instance guard
└── profiles\
    ├── account_1\               # a full, real Claude data directory
    ├── account_2\
    └── account_3\
```

The **live** Claude data directory is turned into an NTFS **junction** pointing
at the active profile:

```
%APPDATA%\Claude  ──junction──▶  %APPDATA%\ClaudeSyncTray\profiles\account_1
```

Switching accounts:

1. Gracefully closes every Claude **Desktop** process (`WM_CLOSE` to its top-level
   windows), then force-terminates anything still alive. The Claude Code CLI is also
   called `claude.exe`; it is told apart by Desktop's Electron `resources` folder and
   is never touched.
2. Re-points the junction at the chosen profile (instant — no data moves).
3. Relaunches Claude Desktop.

Because Claude writes *through* the junction, every session is automatically
persisted into the active profile. Nothing is copied on switch.

**First run against an existing install:** if `%APPDATA%\Claude` is a normal
directory, it is adopted into `profiles\account_1` and replaced with a junction.
This only happens while Claude is not running (otherwise it is deferred to the
next switch). If the profile already holds an account (for example Claude was
started while the link was missing and created a fresh folder), the stray
directory is never merged over it; it is moved to
`%APPDATA%\ClaudeSyncTray\backups\stray-claude-data-<timestamp>` instead.

**Packaged (Microsoft Store) installs** are detected too: the app looks for
`%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude` and manages that
path instead, launching the app through its `shell:AppsFolder` id.

### Sharing Code sessions between accounts (opt-in)

Desktop's **Code** tab lists sessions from
`claude-code-sessions\<accountId>\<orgId>\local_<id>.json` inside the data
directory, so each account normally sees only its own list. The conversations
themselves live in `~/.claude/projects`, which every account already shares; only
these small index files are per account.

On the **Accounts** page, turn on **Code history ▸ Same Code history in every account**.
If the accounts differ, the app first shows what each one will receive (for example
`painted secret  0 → 48`) and asks before anything is copied; it closes Claude Desktop,
backs up, syncs and reopens it for you. The app then mirrors the index files so every
login shows the same history:

- Sync only adds. An account with no sessions can never empty one that has many; two
  accounts with different sessions both end up with all of them.

- The newest copy of each session wins; a session deleted in one account (Desktop
  leaves a `deleted_<id>` marker) is removed from all of them and never comes back.
- Account folders that exist in any profile are created in all of them, so signing
  in to an account that has not opened Code yet already shows the shared list.
- It runs on every switch while the toggle is on. When new sessions have appeared in
  one account, **Sync now** appears next to the status and does the same close, sync and
  reopen on demand.
- Before anything is written, every profile's session list is copied to
  `%APPDATA%\ClaudeSyncTray\backups\auto-sessions-<timestamp>`. The newest 10 of
  these are kept; backups you made by hand are never pruned. A switch also takes this
  snapshot even when sharing is off. If the snapshot fails, nothing is synced.
- Not shared: Claude.ai chat history (server-side, per account), Cowork sessions
  (`local-agent-mode-sessions`) and scheduled tasks.

### Copying data between accounts (Storage tab)

An account folder holds a lot more than settings: the Linux sandbox (`vm_bundles`,
`claude-code-vm`) alone is around 11 GB, plus the Claude Code CLI builds and several
browser caches. The **Storage** tab lists every top-level folder of the selected account,
what it is for (sign-in, settings, history, sandbox, cache), and its size, so you can see
where the space goes.

**Copy data in** brings data from another account or from a backup folder:

- **Copy minimal** is one click. It copies settings and Code history only: the few MB
  that are yours. It leaves out the sandbox, caches and sign-in data.
- **Customize** shows every folder with what would happen to it before you press
  anything, and a checkbox to include or leave it out. *Minimal* and *Everything*
  (all but caches) set the starting selection.

  | Tag | Meaning |
  |---|---|
  | `COPY` | The account has nothing by that name; it is copied over. |
  | `MERGE` | Both have it. Files the account lacks are added; for Code history, the newer copy of a session wins. |
  | `KEEP` | Already there, left exactly as it is. Sign-in data is never overwritten, so copying cannot change which account someone is logged into. |
  | `SKIP` | Not selected, or a link. |

- **Last copy** lists what each folder received: files added, updated, already there, bytes
  and any file that could not be written. Nothing in the target is deleted.
- Close Claude Desktop first when the active account is involved.

**Linux sandbox ▸ Share** moves each account's sandbox and CLI builds into
`%APPDATA%\ClaudeSyncTray\shared` once and links every account to it with a junction, so
the 11 GB exists a single time. **Undo** removes the links and moves the folder back.
A second real copy is never deleted, only reported. Don't run two accounts side by side
while the sandbox is shared.

---

## Build

```powershell
cargo build --release
# -> target\release\claude-sync-tray.exe
```

Optionally copy the exe somewhere permanent (e.g. `%LOCALAPPDATA%\Programs\ClaudeSyncTray\`)
before enabling *Start with Windows*, since the autostart entry stores the exe path.

### Logo and icons

`logo-dithered.jpg` is the brand artwork. Three things are derived from it:

| Where | How |
|---|---|
| Dashboard title bar | Drawn procedurally from the logo's geometry (`src/logo.rs`) and dithered with an 8x8 Bayer matrix (`src/dither.rs`); no image is shipped for it. |
| Tray icon | The asterisk on a dark tile, rasterised at the shell's small-icon size, with a pip in the active account's colour (`src/icon.rs`). |
| `.exe` / taskbar / window icon | `assets/icon.ico`, embedded by `build.rs`. Regenerate it from the JPG with `python tools/make_icon.py` (needs Pillow). If no Windows resource compiler is available the build still succeeds, just without the embedded icon. |

## Usage

Run it — the **GPUI Dashboard** opens alongside a color-coded **System Tray icon**:

- **Dashboard tabs:**
  - **Accounts:** Switch accounts, add, rename or remove them, see each account's email and Code session count, and keep the Code history the same in every account with one switch.
  - **Settings:** *Start with Windows* and what to keep the same in every account. Under **Advanced**: manual sync buttons, `claude.exe` and launch overrides, `live_override`, and folder shortcuts.
  - **Storage:** See what each account folder contains and how big it is, copy data between accounts (one-click minimal or a detailed per-folder view), and share one Linux sandbox between accounts.
  - **Activity Log:** Inspect, refresh, or clear `claude-sync-tray.log` in real time.
- **System Tray:**
  - **Left-click** the tray icon to toggle the GPUI dashboard window.
  - **Right-click** the tray icon for the quick context menu:

| Menu item | Action |
| --- | --- |
| **Open Dashboard** | Show and focus the GPUI window |
| **Active: Account N** | Current account (status only) |
| **Switch Account ▸** | Switch to any configured profile |
| **Restart Claude Desktop** | Relaunch on the current account |
| **Repair Profile Link** | Re-create the junction if it was tampered with |
| **Start with Windows** | Toggle the `HKCU\...\CurrentVersion\Run` entry (starts hidden with `--tray`) |
| **Open Claude Code Projects** | Opens `%USERPROFILE%\.claude\projects` |
| **Open Profiles Folder** | Opens the profile store |
| **Open Log File** | Opens the log |
| **Quit** | Exit the dashboard and tray |

### Command line

```
claude-sync-tray                 # open dashboard & tray (or focus existing instance)
claude-sync-tray --tray          # start hidden in the system tray
claude-sync-tray --switch 2      # switch to the 2nd profile (numbering starts at 1) and exit
claude-sync-tray --help
```

If the tray is already running, `--switch N` is handed to it via a command file
(`%APPDATA%\ClaudeSyncTray\command.txt`) and executed within a second. This makes
it easy to wire keyboard shortcuts or shortcuts that pin an account.

---

## Configuration

`%APPDATA%\ClaudeSyncTray\config.json`:

```json
{
  "version": 1,
  "active": 0,
  "profiles": [
    { "name": "Account 1", "dir": "profiles/account_1" },
    { "name": "Account 2", "dir": "profiles/account_2" },
    { "name": "Account 3", "dir": "profiles/account_3" }
  ],
  "live_override": null,
  "launch": null,
  "start_with_windows": false,
  "share_mcp_config": true,
  "sync_claude_code": true,
  "share_sessions": false
}
```

| Field | Purpose |
| --- | --- |
| `profiles` | Rename accounts or add more. `dir` may be absolute or relative to `%APPDATA%\ClaudeSyncTray`. |
| `live_override` | Force the Claude data directory (e.g. a portable install). |
| `launch.exe` | Explicit `claude.exe` path; skips auto-detection. |
| `launch.command` | Arbitrary launch command (e.g. a `shell:AppsFolder` id). |
| `active` | Fallback only — the junction target wins on startup. |
| `share_mcp_config` | Keep `claude_desktop_config.json` (MCP servers, app settings) identical across profiles. |
| `sync_claude_code` | Swap the Claude Code CLI credentials together with the Desktop account. |
| `share_sessions` | Mirror the Code session list into every account (see above). Off by default. |

`claude.exe` is auto-detected from: the running process, `%LOCALAPPDATA%\AnthropicClaude\app-*`,
`%LOCALAPPDATA%\Programs\Claude`, `%ProgramFiles%\Claude`, and the
`App Paths\claude.exe` registry key.

---

## Reliability notes

- **File locks:** Anthropic's Electron app keeps session `.json` files open while
  writing. Every filesystem operation retries with backoff on
  `ERROR_ACCESS_DENIED` / `SHARING_VIOLATION` / `LOCK_VIOLATION`, so a switch
  during a write normally still succeeds.
- **Safety:** the app never recursively deletes a *real* directory it did not
  create; an unmanaged directory is adopted (moved) first. The junction is only
  removed, never followed.
- **Single instance:** enforced with an exclusive lock on `instance.lock`.

### Caveats

- The live path is a junction owned by this tool. **Don't delete
  `%APPDATA%\ClaudeSyncTray` while Claude is installed** — that would leave a
  dangling junction. To uninstall cleanly: *Quit* the tray, then treat one
  profile directory as authoritative, copy it back to `%APPDATA%\Claude`, and
  remove the junction (`rmdir "%APPDATA%\Claude"` then move the real folder in).
- Some aggressive "cleaner"/antivirus tools flag junctions. If Claude misbehaves
  after a cleanup, use **Repair Profile Link**.
- A switch blocks the event loop for a few seconds while Claude shuts down.

## Troubleshooting

Check `%APPDATA%\ClaudeSyncTray\claude-sync-tray.log` first. Common cases:

- *"Claude Desktop still has files open"* — quit Claude from its own UI and retry.
- Claude won't launch — set `launch.exe` in `config.json` to the real path.
- Package install — set `launch.command` to
  `explorer.exe shell:AppsFolder\<PackageFamilyName>!<AppId>` if auto-detection fails.
