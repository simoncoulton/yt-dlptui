# AGENTS.md

Guidance for AI agents (and people) changing yt-dlptui. It follows the
conventions of torrentui; the user-facing doc is the README.

## What this is

`ratatui` + `crossterm` UI that runs `yt-dlp` child processes.

| Module | Role |
| --- | --- |
| `app.rs` | UI state and **all key handling**. Pure: no I/O, no rendering. |
| `ui.rs` | Rendering only: `draw(frame, &app)` never mutates state. |
| `tui.rs` | The event loop; turns `Action`s into yt-dlp processes. |
| `ytdlp.rs` | Probing formats, building the download command, parsing output. |
| `download.rs` | The `Download` model, progress maths, the saved list. |
| `config.rs`, `theme.rs`, `fmt.rs` | YAML config, palettes, formatting. |

Data flow: key -> `App::handle_key` -> `Option<Action>` -> `tui.rs` spawns or
aborts a task -> task sends `Update`s -> `App::apply_update` -> `ui::draw`.
Never do I/O or block in `app.rs` or `ui.rs`.

## Rules carried over from torrentui

- No colour literals outside `theme.rs`; use the semantic palette on
  `app.theme`. `accent` is for titles, key hints and modal borders.
  Status colours carry meaning (`success` downloading, `info` done, `warning`
  paused and confirmations, `error` failures, `special` starting).
- Themes: `theme::load_themes` layers built-ins, then `themes/*.yml` next to
  the config (built-ins are seeded as files), then `themes:` in `config.yml`.
  Match names with `theme::normalize`. Never paint a background yourself; go
  through `theme.base()` / `popup()` so `transparent_background` works. Use
  `App::set_transparent` so the previewed theme is re-derived.
- The config is edited in place (`config::set_value`), never rewritten, and a
  broken file is refused rather than overwritten.
- Modals: `centered` + `popup` + `modal()` (rounded, accent border, hint line).
  Errors go on a line inside the dialog in `t.error`.
- Text fields eat letters: plain characters are text in the Add dialog.
- Destructive actions ask first; deleting data needs an explicit `y`. Quitting
  asks too while a download is running (`Ctrl-C` twice also quits).
- Nothing may panic at any terminal size (`saturating_sub`, `.min`/`.max`).
- Keep the footer short; less common keys live in the help popup and README.
- When you add a key, update `key_*` in `app.rs`, `draw_help`, the README table
  and the tests.

## yt-dlp specifics

- The URL is always passed after `--` so it can never become an option.
- Progress and the final path are read from lines tagged `[ytdlptui]` /
  `[ytdlptui-file]` (`--progress-template`, `--print after_move:`). Video and
  audio download as separate streams; `Download::apply_progress` sums them.
- Partial files go to `<dir>/.yt-dlptui/<id>/` (`-P temp:`), so removal deletes
  exactly what that download made. Pausing aborts the task (the process is
  killed on drop); resuming re-runs the same command and yt-dlp continues.
- Probe answers carry a sequence number; a cancelled probe's answer is dropped.

## Checks

`cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`. Tests use temp dirs, never the real config or data dirs.
When running the app for real, use an isolated `HOME` and `--dir`.
