//! The terminal UI's event loop: turns `Action`s into yt-dlp processes.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyEventKind},
    execute,
};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::{sync::mpsc, task::AbortHandle, time::MissedTickBehavior};

use crate::{
    app::{Action, App},
    config::{self, Config},
    download, ui,
    ytdlp::{self, Probe, Update},
};

/// How often the screen is redrawn even when nothing happened (so status
/// messages expire and the spinner moves).
const REDRAW: Duration = Duration::from_millis(500);

/// What `run` needs besides the app.
pub struct Settings {
    pub config: Config,
    pub config_path: PathBuf,
    /// Where the download list is saved.
    pub store_path: PathBuf,
    /// A URL to look up as soon as the window opens.
    pub url: Option<String>,
}

/// Results of background work, delivered to the event loop.
enum Msg {
    Probed {
        seq: u64,
        url: String,
        result: Result<Probe>,
    },
    /// A background clean-up finished.
    Status(String, bool),
}

/// Enter the alternate screen / raw mode. `ratatui::init` also installs a panic
/// hook that restores the terminal; we extend it to undo bracketed paste.
pub fn init_terminal() -> Result<DefaultTerminal> {
    let terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        prev(info);
    }));
    Ok(terminal)
}

pub fn restore_terminal() {
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

/// Save the list if it changed. A failure is shown, not fatal: downloads go on.
fn save(app: &mut App, path: &std::path::Path) {
    if !app.dirty {
        return;
    }
    app.dirty = false;
    if let Err(e) = download::save(path, &app.downloads) {
        app.set_status(format!("Cannot save the download list: {e:#}"), true);
    }
}

pub async fn run(mut terminal: DefaultTerminal, mut app: App, settings: Settings) -> Result<()> {
    let Settings {
        config,
        config_path,
        store_path,
        url,
    } = settings;
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<(u64, Update)>();
    let mut term_events = EventStream::new();
    let mut tick = tokio::time::interval(REDRAW);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut running: HashMap<u64, AbortHandle> = HashMap::new();
    let mut probe: Option<AbortHandle> = None;

    // Downloads that were running when a previous window closed come back
    // paused; nothing starts until the user asks.
    let mut pending = Vec::new();
    if let Some(url) = url {
        app.open_add(&url);
        pending.extend(app.begin_probe());
    }

    let mut actions = pending;
    loop {
        for action in actions.drain(..) {
            match action {
                Action::Quit => {
                    app.pause_all();
                    for handle in running.values() {
                        handle.abort();
                    }
                    if let Some(handle) = &probe {
                        handle.abort();
                    }
                    save(&mut app, &store_path);
                    // Give the aborted tasks a moment to drop their processes.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    return Ok(());
                }
                Action::Probe { seq, url } => {
                    if let Some(old) = probe.take() {
                        old.abort();
                    }
                    let (config, tx) = (config.clone(), tx.clone());
                    probe = Some(
                        tokio::spawn(async move {
                            let result = ytdlp::probe(&config, &url).await;
                            let _ = tx.send(Msg::Probed { seq, url, result });
                        })
                        .abort_handle(),
                    );
                }
                Action::CancelProbe => {
                    if let Some(old) = probe.take() {
                        old.abort();
                    }
                }
                Action::Start(id) => {
                    if let Some(d) = app.downloads.iter().find(|d| d.id == id) {
                        let (config, job, tx) = (config.clone(), d.job(), update_tx.clone());
                        let task = tokio::spawn(ytdlp::download(config, id, job, tx));
                        if let Some(old) = running.insert(id, task.abort_handle()) {
                            old.abort();
                        }
                    }
                }
                Action::Stop(id) => {
                    if let Some(handle) = running.remove(&id) {
                        handle.abort();
                    }
                }
                Action::SaveTheme { name, transparent } => {
                    let mut note = format!("Theme: {name}");
                    let mut failed = None;
                    if let Err(e) =
                        config::set_value(&config_path, "theme", &config::yaml_scalar(&name))
                    {
                        failed = Some(e);
                    } else if let Some(on) = transparent {
                        note.push_str(if on {
                            ", transparent background"
                        } else {
                            ", theme background"
                        });
                        if let Err(e) = config::set_value(
                            &config_path,
                            "transparent_background",
                            &on.to_string(),
                        ) {
                            failed = Some(e);
                        }
                    }
                    match failed {
                        None => app.set_status(format!("{note} (saved)"), false),
                        Some(e) => {
                            app.set_status(format!("{note} applied but not saved: {e:#}"), true)
                        }
                    }
                }
                Action::Remove {
                    download,
                    delete_files,
                } => {
                    if let Some(handle) = running.remove(&download.id) {
                        handle.abort();
                    }
                    // The killed process may still hold files for a moment, so
                    // clean up off the UI thread and report only a failure.
                    let tx = tx.clone();
                    tokio::task::spawn_blocking(move || {
                        std::thread::sleep(Duration::from_millis(200));
                        if let Some(e) = download.delete_files(delete_files).first() {
                            let _ = tx.send(Msg::Status(format!("Cannot delete {e}"), true));
                        }
                    });
                    app.set_status("Removed", false);
                }
            }
        }
        save(&mut app, &store_path);

        terminal.draw(|f| ui::draw(f, &app))?;

        tokio::select! {
            _ = tick.tick() => app.tick(),

            maybe_event = term_events.next() => {
                // Stream end means stdin closed; nothing more can be done.
                let Some(event) = maybe_event else {
                    actions.push(Action::Quit);
                    continue;
                };
                match event? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        actions.extend(app.handle_key(key));
                    }
                    Event::Paste(text) => app.handle_paste(&text),
                    // Resize and everything else just triggers the redraw at the loop top.
                    _ => {}
                }
            }

            Some(msg) = rx.recv() => match msg {
                Msg::Probed { seq, url, result } => {
                    app.probed(seq, url, result.map_err(|e| format!("{e:#}")));
                }
                Msg::Status(text, is_error) => app.set_status(text, is_error),
            },

            Some((id, update)) = update_rx.recv() => {
                if matches!(update, Update::Exited(_)) {
                    running.remove(&id);
                }
                app.apply_update(id, update);
            }
        }
    }
}
