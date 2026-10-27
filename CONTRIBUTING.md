# Contributing to ClaudeSyncTray

Thanks for helping. Small fixes, bug reports and ideas are all welcome. By taking part you agree
to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go through
[SECURITY.md](SECURITY.md), not a public issue.

## Setup

You need Windows 10 or 11, Rust 1.89 or newer, and the Visual Studio Build Tools with the
**Desktop development with C++** workload and a Windows SDK.

```powershell
git clone https://github.com/YarooqH/claudeswitcher.git
cd claudeswitcher
cargo build --release     # -> target\release\claude-sync-tray.exe
```

If the build fails with a linker error, run `cargo` from an *x64 Native Tools Command Prompt*.
Git for Windows ships its own `link.exe`, which can shadow the MSVC one.

## Dev loop

`powershell -File tools\dev.ps1` rebuilds and restarts the tray. Windows will not let cargo
overwrite a running exe, so the script renames it aside first and puts it back if the build
fails. Use `-NoRun` to only build.

The app reads and writes your real Claude Desktop data folder. While working on switching or
copying code, take a copy of `%APPDATA%\ClaudeSyncTray` first, or run the exe against a
throwaway tree the way `tools\screenshots.ps1` does: point `APPDATA`, `LOCALAPPDATA` and
`USERPROFILE` at an empty folder before starting it.

## Tests and lint

```powershell
cargo test
cargo clippy
```

- Add a test for behaviour that touches user data (`sessions.rs`, `transfer.rs`, `profiles.rs`).
  Tests build their own temporary folders; they must never touch the real `%APPDATA%`.
- Don't introduce new clippy warnings.
- The code is not fully `rustfmt`-clean. Match the style of the file you are editing and don't
  reformat unrelated code in the same change.
- If you change what the UI looks like, regenerate the screenshots with
  `powershell -File tools\screenshots.ps1` (see the README) and commit the new images. Look at
  them before committing: they must show only the demo accounts.

## Commit style

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(ui): one switch for shared Code history
fix(sessions): never let an empty account remove sessions
docs: explain the sandbox sharing undo
chore: add tools/dev.ps1
```

Keep each commit to one change, and explain the why in the body when it isn't obvious.

## Pull requests

1. Branch from `main`.
2. Make sure `cargo build --release` and `cargo test` pass.
3. Fill in the pull request template: what changed, why, and how you tested it. For UI changes,
   include a screenshot.

## Things that will bite you

- **Never delete a real folder you didn't create.** Junctions and symlinks are removed, never
  followed. See `remove_entry` in `src/profiles.rs`.
- **Sign-in data is never overwritten** when copying between accounts (`transfer.rs`), so a copy
  can't change which account someone is logged into.
- **Session sync only adds.** An empty account must never be able to empty a full one
  (`sessions.rs` has tests for this; keep them passing).
- **Claude Desktop and the Claude Code CLI are both `claude.exe`.** Only Desktop is ever closed;
  `src/process.rs` tells them apart by Desktop's `resources` folder.
- **Windows can rename a running exe but not overwrite it,** and files in Claude's folder stay
  open while it runs. File operations retry with backoff for that reason.
- **Packaged and classic installs keep their data in different places.** Test path logic against
  both (`src/claude.rs`).
- Paths come from `%APPDATA%`, `%LOCALAPPDATA%` and `%USERPROFILE%` only (`src/paths.rs`). Keep it
  that way, since the screenshot script and tests depend on it.
- GPUI element ids that carry an index must be `(&str, usize)` tuples.
