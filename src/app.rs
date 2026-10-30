//! UI state and all key handling. Pure: no I/O, no rendering. Key handlers
//! return an `Action` and the event loop in `tui.rs` performs the side effect.

use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    download::{Download, Status},
    theme::{self, Theme},
    ytdlp::{Choice, Probe, Update},
};

/// How long a status message stays in the footer.
const STATUS_TTL: Duration = Duration::from_secs(5);

/// Throughput samples kept for the header sparkline.
const HISTORY: usize = 300;

/// A single-line text field. The cursor is a char index.
#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.chars().count();
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    fn byte(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Insert pasted text as one line: line breaks and tabs are dropped.
    pub fn paste(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.chars().count() {
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    /// Asking for a URL.
    Add,
    /// Waiting for yt-dlp to list the formats.
    Probing,
    /// Choosing a format.
    Formats,
    ConfirmRemove {
        id: u64,
        delete_files: bool,
    },
    Help,
    /// Quitting would stop running downloads.
    ConfirmQuit,
    /// Theme picker with live preview; state in [`App::theme_picker`].
    Theme,
}

pub struct ThemePicker {
    pub cursor: usize,
    /// Index of the theme in effect when the picker opened, restored on cancel.
    original: usize,
    /// Whether the background was transparent when it opened, likewise.
    original_transparent: bool,
}

/// The format picker's state while it is open.
#[derive(Debug, Clone)]
pub struct FormatPicker {
    pub url: String,
    pub title: String,
    pub duration: Option<u64>,
    pub choices: Vec<Choice>,
    pub selected: usize,
}

/// Side effects the event loop performs for a key press.
#[derive(Debug)]
pub enum Action {
    Quit,
    /// List the formats of a URL; `seq` ties the answer to this request.
    Probe {
        seq: u64,
        url: String,
    },
    CancelProbe,
    /// Run (or resume) yt-dlp for this download.
    Start(u64),
    /// Stop its yt-dlp; partial files stay for a later resume.
    Stop(u64),
    Remove {
        download: Box<Download>,
        delete_files: bool,
    },
    /// Persist the chosen theme name. `transparent` is set only when the
    /// picker changed the background.
    SaveTheme {
        name: String,
        transparent: Option<bool>,
    },
}

pub struct App {
    pub theme: Theme,
    /// Whether the theme's background is dropped so the terminal's shows.
    pub transparent: bool,
    pub themes: Vec<Theme>,
    theme_index: usize,
    pub theme_picker: Option<ThemePicker>,
    pub download_dir: PathBuf,
    pub downloads: Vec<Download>,
    pub selected: usize,
    pub mode: Mode,
    pub input: Input,
    /// Why the last probe failed, shown inside the Add dialog.
    pub add_error: Option<String>,
    pub picker: Option<FormatPicker>,
    pub down_history: VecDeque<u64>,
    /// Redraw ticks so far; drives the spinner.
    pub ticks: usize,
    /// The saved list is out of date.
    pub dirty: bool,
    probe_seq: u64,
    status: Option<(String, bool, Instant)>,
}

impl App {
    pub fn new(theme: Theme, download_dir: PathBuf, downloads: Vec<Download>) -> Self {
        Self {
            theme,
            transparent: true,
            themes: theme::builtin_themes(),
            theme_index: 0,
            theme_picker: None,
            download_dir,
            downloads,
            selected: 0,
            mode: Mode::Normal,
            input: Input::default(),
            add_error: None,
            picker: None,
            down_history: VecDeque::new(),
            ticks: 0,
            dirty: false,
            probe_seq: 0,
            status: None,
        }
    }

    /// Switch between the theme's background and the terminal's.
    pub fn set_transparent(&mut self, on: bool) {
        self.transparent = on;
        // The theme being previewed, if the picker is open.
        let shown = self
            .theme_picker
            .as_ref()
            .map_or(self.theme_index, |p| p.cursor);
        self.theme = self.themes[shown].clone().transparent(on);
    }

    /// Install the available themes and make `current` (by name) active.
    pub fn set_themes(&mut self, themes: Vec<Theme>, current: &str) {
        self.theme_index = themes
            .iter()
            .position(|t| theme::normalize(&t.name) == theme::normalize(current))
            .unwrap_or(0);
        self.theme = themes[self.theme_index]
            .clone()
            .transparent(self.transparent);
        self.themes = themes;
    }

    pub fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some((text.into(), is_error, Instant::now()));
    }

    /// The message to show, unless it has expired.
    pub fn status(&self) -> Option<(&str, bool)> {
        self.status
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL)
            .map(|(text, is_error, _)| (text.as_str(), *is_error))
    }

    pub fn current(&self) -> Option<&Download> {
        self.downloads.get(self.selected)
    }

    pub fn total_speed(&self) -> u64 {
        self.downloads.iter().map(|d| d.speed).sum()
    }

    pub fn active_count(&self) -> usize {
        self.downloads
            .iter()
            .filter(|d| d.status.is_active())
            .count()
    }

    /// Called on every redraw tick.
    pub fn tick(&mut self) {
        self.ticks += 1;
        // One sample per second, not per tick.
        if self.ticks.is_multiple_of(2) {
            self.down_history.push_back(self.total_speed());
            while self.down_history.len() > HISTORY {
                self.down_history.pop_front();
            }
        }
    }

    /// Ids are creation times, so they stay unique across restarts and the
    /// temp folder of a removed download is never reused.
    fn next_id(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let newest = self.downloads.iter().map(|d| d.id).max().unwrap_or(0);
        now.max(newest + 1)
    }

    /// Open the Add dialog with `url` filled in, as when one is pasted.
    pub fn open_add(&mut self, url: &str) {
        self.input.clear();
        self.input.paste(url);
        self.add_error = None;
        self.mode = Mode::Add;
    }

    /// Start listing the formats of the URL in the input.
    pub fn begin_probe(&mut self) -> Option<Action> {
        let url = self.input.text().trim().to_string();
        if url.is_empty() {
            self.add_error = Some("Enter the URL of a video".into());
            return None;
        }
        self.probe_seq += 1;
        self.add_error = None;
        self.mode = Mode::Probing;
        Some(Action::Probe {
            seq: self.probe_seq,
            url,
        })
    }

    /// yt-dlp answered a probe. Answers to a probe the user has since
    /// cancelled are dropped.
    pub fn probed(&mut self, seq: u64, url: String, result: Result<Probe, String>) {
        if self.mode != Mode::Probing || seq != self.probe_seq {
            return;
        }
        match result {
            Ok(probe) => {
                self.picker = Some(FormatPicker {
                    url,
                    title: probe.title,
                    duration: probe.duration,
                    choices: probe.choices,
                    selected: 0,
                });
                self.mode = Mode::Formats;
            }
            Err(e) => {
                self.add_error = Some(e);
                self.mode = Mode::Add;
            }
        }
    }

    /// A running yt-dlp reported back. Reports from a download the user has
    /// paused or removed in the meantime are stale and ignored.
    pub fn apply_update(&mut self, id: u64, update: Update) {
        let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) else {
            return;
        };
        if !d.status.is_active() {
            return;
        }
        match update {
            Update::Event(event) => d.apply(event),
            Update::Exited(result) => {
                let failed = result.clone().err();
                d.exited(result);
                let title = d.title.clone();
                self.dirty = true;
                match failed {
                    None => self.set_status(format!("Finished: {title}"), false),
                    Some(e) => self.set_status(format!("Failed: {title}: {e}"), true),
                }
            }
        }
    }

    /// The window is closing: running downloads stop and come back paused.
    pub fn pause_all(&mut self) {
        for d in self.downloads.iter_mut().filter(|d| d.status.is_active()) {
            d.pause();
            self.dirty = true;
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        match self.mode {
            Mode::Add => {
                self.input.paste(text);
                self.add_error = None;
            }
            Mode::Normal => self.open_add(text.trim()),
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            // A second Ctrl-C at the confirmation quits for sure.
            return if self.mode == Mode::ConfirmQuit {
                Some(Action::Quit)
            } else {
                self.request_quit()
            };
        }
        match self.mode {
            Mode::Normal => self.key_normal(key),
            Mode::Add => self.key_add(key),
            Mode::Probing => self.key_probing(key),
            Mode::Formats => self.key_formats(key),
            Mode::ConfirmQuit => self.key_confirm_quit(key),
            Mode::Theme => self.key_theme(key),
            Mode::ConfirmRemove { id, delete_files } => self.key_confirm(key, id, delete_files),
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?')
                ) {
                    self.mode = Mode::Normal;
                }
                None
            }
        }
    }

    fn select(&mut self, to: usize) {
        self.selected = to.min(self.downloads.len().saturating_sub(1));
    }

    fn key_normal(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => return self.request_quit(),
            KeyCode::Char('T') => {
                self.theme_picker = Some(ThemePicker {
                    cursor: self.theme_index,
                    original: self.theme_index,
                    original_transparent: self.transparent,
                });
                self.mode = Mode::Theme;
            }
            KeyCode::Char('a') => self.open_add(""),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('p' | ' ') => return self.toggle_pause(),
            KeyCode::Char('d') | KeyCode::Delete => self.ask_remove(false),
            KeyCode::Char('D') => self.ask_remove(true),
            _ => {}
        }
        None
    }

    /// Quit now, or ask first when that would stop running downloads.
    fn request_quit(&mut self) -> Option<Action> {
        if self.active_count() == 0 {
            return Some(Action::Quit);
        }
        self.mode = Mode::ConfirmQuit;
        None
    }

    fn key_confirm_quit(&mut self, key: KeyEvent) -> Option<Action> {
        self.mode = Mode::Normal;
        matches!(key.code, KeyCode::Char('y') | KeyCode::Enter).then_some(Action::Quit)
    }

    fn key_theme(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(picker) = self.theme_picker.as_mut() else {
            self.mode = Mode::Normal;
            return None;
        };
        let last = self.themes.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => picker.cursor = (picker.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => picker.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => picker.cursor = last,
            KeyCode::Char('b') => {
                let on = !self.transparent;
                self.set_transparent(on);
                return None;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let (original, was) = (picker.original, picker.original_transparent);
                self.theme_picker = None;
                self.transparent = was;
                self.theme = self.themes[original].clone().transparent(was);
                self.mode = Mode::Normal;
                return None;
            }
            KeyCode::Enter => {
                let (cursor, was) = (picker.cursor, picker.original_transparent);
                self.theme_picker = None;
                self.theme_index = cursor;
                self.theme = self.themes[cursor].clone().transparent(self.transparent);
                self.mode = Mode::Normal;
                return Some(Action::SaveTheme {
                    name: self.theme.name.clone(),
                    transparent: (self.transparent != was).then_some(self.transparent),
                });
            }
            _ => return None,
        }
        // Live preview of the highlighted theme.
        self.theme = self.themes[picker.cursor]
            .clone()
            .transparent(self.transparent);
        None
    }

    fn toggle_pause(&mut self) -> Option<Action> {
        let d = self.downloads.get_mut(self.selected)?;
        let id = d.id;
        match d.status {
            Status::Starting | Status::Downloading => {
                d.pause();
                self.dirty = true;
                Some(Action::Stop(id))
            }
            Status::Paused | Status::Error => {
                d.restart();
                self.dirty = true;
                Some(Action::Start(id))
            }
            Status::Done => {
                self.set_status("Already finished", false);
                None
            }
        }
    }

    fn ask_remove(&mut self, delete_files: bool) {
        if let Some(d) = self.current() {
            self.mode = Mode::ConfirmRemove {
                id: d.id,
                delete_files,
            };
        }
    }

    fn key_add(&mut self, key: KeyEvent) -> Option<Action> {
        // Plain characters are text here, never commands.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.add_error = None;
            }
            KeyCode::Enter => return self.begin_probe(),
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.insert(c);
                self.add_error = None;
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            _ => {}
        }
        None
    }

    fn key_probing(&mut self, key: KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Esc {
            // Back to the dialog with the URL intact, to fix a typo.
            self.mode = Mode::Add;
            return Some(Action::CancelProbe);
        }
        None
    }

    fn key_formats(&mut self, key: KeyEvent) -> Option<Action> {
        let picker = self.picker.as_mut()?;
        let last = picker.choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::PageDown => picker.selected = (picker.selected + 10).min(last),
            KeyCode::PageUp => picker.selected = picker.selected.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => picker.selected = 0,
            KeyCode::Char('G') | KeyCode::End => picker.selected = last,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.picker = None;
                self.mode = Mode::Add;
            }
            KeyCode::Enter => return self.start_selected(),
            _ => {}
        }
        None
    }

    fn start_selected(&mut self) -> Option<Action> {
        let picker = self.picker.take()?;
        let choice = picker.choices.get(picker.selected)?;
        let id = self.next_id();
        let download = Download::new(
            id,
            picker.url,
            picker.title,
            choice,
            self.download_dir.clone(),
        );
        self.downloads.push(download);
        self.selected = self.downloads.len() - 1;
        self.input.clear();
        self.mode = Mode::Normal;
        self.dirty = true;
        Some(Action::Start(id))
    }

    fn key_confirm(&mut self, key: KeyEvent, id: u64, delete_files: bool) -> Option<Action> {
        let confirmed = match key.code {
            KeyCode::Char('y') => true,
            // Deleting data needs an explicit `y`; Enter only removes the entry.
            KeyCode::Enter => !delete_files,
            KeyCode::Char('D') if !delete_files => {
                self.mode = Mode::ConfirmRemove {
                    id,
                    delete_files: true,
                };
                return None;
            }
            _ => false,
        };
        self.mode = Mode::Normal;
        if !confirmed {
            return None;
        }
        let at = self.downloads.iter().position(|d| d.id == id)?;
        let download = self.downloads.remove(at);
        self.select(self.selected);
        self.dirty = true;
        Some(Action::Remove {
            download: Box::new(download),
            delete_files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        theme::default_theme,
        ytdlp::{Event, Progress},
    };

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn choice(label: &str, selector: &str) -> Choice {
        Choice {
            label: label.into(),
            selector: selector.into(),
            size: Some(100),
            format: None,
            note: String::new(),
        }
    }

    fn probe() -> Probe {
        Probe {
            title: "A video".into(),
            duration: Some(60),
            choices: vec![
                choice("best", "bv*+ba/b"),
                choice("144p mp4 [160]", "160+ba/160"),
            ],
        }
    }

    fn app() -> App {
        App::new(default_theme(), "/tmp/dl".into(), Vec::new())
    }

    /// An app that has gone through Add -> Probing -> Formats.
    fn at_formats() -> App {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "https://example.com/v");
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        app.probed(seq, url, Ok(probe()));
        assert_eq!(app.mode, Mode::Formats);
        app
    }

    fn with_download() -> App {
        let mut app = at_formats();
        press(&mut app, KeyCode::Down);
        let Some(Action::Start(_)) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a start");
        };
        app
    }

    #[test]
    fn input_edits_by_char_not_byte() {
        let mut i = Input::default();
        i.paste("héllo\nwörld");
        assert_eq!(i.text(), "héllowörld");
        i.home();
        i.right();
        i.right();
        i.delete();
        i.backspace();
        assert_eq!(i.text(), "hlowörld");
        i.end();
        i.insert('!');
        assert_eq!(i.text(), "hlowörld!");
        assert_eq!(i.cursor(), 9);
    }

    #[test]
    fn adding_probes_then_lists_formats() {
        let mut app = app();
        assert!(press(&mut app, KeyCode::Char('a')).is_none());
        assert_eq!(app.mode, Mode::Add);
        // Letters that are commands elsewhere are plain text here.
        type_text(&mut app, "q a d p ?");
        assert_eq!(app.mode, Mode::Add);
        assert_eq!(app.input.text(), "q a d p ?");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut app, " https://example.com/v ");
        let Some(Action::Probe { url, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        assert_eq!(url, "https://example.com/v");
        assert_eq!(app.mode, Mode::Probing);
    }

    #[test]
    fn empty_url_is_refused_in_the_dialog() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.mode, Mode::Add);
        assert!(app.add_error.is_some());
        type_text(&mut app, "x");
        assert!(app.add_error.is_none());
    }

    #[test]
    fn failed_probe_returns_to_the_dialog_with_the_url() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "bad");
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        app.probed(seq, url, Err("Unsupported URL".into()));
        assert_eq!(app.mode, Mode::Add);
        assert_eq!(app.input.text(), "bad");
        assert_eq!(app.add_error.as_deref(), Some("Unsupported URL"));
    }

    #[test]
    fn cancelled_probe_answers_are_ignored() {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "u");
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        assert!(matches!(
            press(&mut app, KeyCode::Esc),
            Some(Action::CancelProbe)
        ));
        assert_eq!(app.mode, Mode::Add);
        app.probed(seq, url.clone(), Ok(probe()));
        assert_eq!(app.mode, Mode::Add);
        assert!(app.picker.is_none());

        // The answer to a newer probe still counts; the old one stays stale.
        let Some(Action::Probe { seq: newer, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        app.probed(seq, url.clone(), Ok(probe()));
        assert_eq!(app.mode, Mode::Probing);
        app.probed(newer, url, Ok(probe()));
        assert_eq!(app.mode, Mode::Formats);
    }

    #[test]
    fn picking_a_format_starts_a_download() {
        let app = with_download();
        assert_eq!(app.mode, Mode::Normal);
        let d = app.current().unwrap();
        assert_eq!(d.title, "A video");
        assert_eq!(d.selector, "160+ba/160");
        assert_eq!(d.status, Status::Starting);
        assert!(app.dirty);
    }

    #[test]
    fn format_navigation_is_bounded_and_esc_goes_back() {
        let mut app = at_formats();
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.picker.as_ref().unwrap().selected, 1);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Add);
        assert_eq!(app.input.text(), "https://example.com/v");
    }

    #[test]
    fn pause_and_resume_toggle_the_process() {
        let mut app = with_download();
        let id = app.current().unwrap().id;
        assert!(matches!(press(&mut app, KeyCode::Char('p')), Some(Action::Stop(i)) if i == id));
        assert_eq!(app.current().unwrap().status, Status::Paused);
        assert!(matches!(press(&mut app, KeyCode::Char(' ')), Some(Action::Start(i)) if i == id));
        assert_eq!(app.current().unwrap().status, Status::Starting);

        app.apply_update(id, Update::Exited(Ok(())));
        assert_eq!(app.current().unwrap().status, Status::Done);
        assert!(press(&mut app, KeyCode::Char('p')).is_none());
    }

    #[test]
    fn a_failed_download_can_be_retried() {
        let mut app = with_download();
        let id = app.current().unwrap().id;
        app.apply_update(id, Update::Exited(Err("HTTP 403".into())));
        assert_eq!(app.current().unwrap().status, Status::Error);
        assert!(
            app.status()
                .is_some_and(|(t, e)| e && t.contains("HTTP 403"))
        );
        assert!(matches!(
            press(&mut app, KeyCode::Char('p')),
            Some(Action::Start(_))
        ));
    }

    #[test]
    fn stale_updates_are_ignored() {
        let mut app = with_download();
        let id = app.current().unwrap().id;
        press(&mut app, KeyCode::Char('p'));
        let event = Event::Progress(Progress {
            finished: false,
            downloaded: 50,
            total: Some(100),
            speed: Some(5),
        });
        app.apply_update(id, Update::Event(event));
        app.apply_update(id, Update::Exited(Err("killed".into())));
        let d = app.current().unwrap();
        assert_eq!((d.status, d.downloaded), (Status::Paused, 0));
        // Unknown ids are fine too.
        app.apply_update(id + 1, Update::Exited(Ok(())));
    }

    #[test]
    fn removing_asks_first() {
        let mut app = with_download();
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(
            app.mode,
            Mode::ConfirmRemove {
                delete_files: false,
                ..
            }
        ));
        assert!(press(&mut app, KeyCode::Char('n')).is_none());
        assert_eq!(app.downloads.len(), 1);

        press(&mut app, KeyCode::Char('d'));
        let Some(Action::Remove { delete_files, .. }) = press(&mut app, KeyCode::Char('y')) else {
            panic!("expected a removal");
        };
        assert!(!delete_files);
        assert!(app.downloads.is_empty());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn deleting_data_needs_an_explicit_y() {
        let mut app = with_download();
        press(&mut app, KeyCode::Char('D'));
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert_eq!(app.downloads.len(), 1);

        // `D` inside the plain dialog upgrades it.
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('D'));
        assert!(matches!(
            app.mode,
            Mode::ConfirmRemove {
                delete_files: true,
                ..
            }
        ));
        assert!(matches!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Remove {
                delete_files: true,
                ..
            })
        ));
    }

    #[test]
    fn keys_on_an_empty_list_do_nothing() {
        let mut app = app();
        for c in ['p', 'd', 'D', 'j', 'G', ' '] {
            assert!(press(&mut app, KeyCode::Char(c)).is_none());
        }
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn quitting_pauses_running_downloads() {
        let mut app = with_download();
        assert_eq!(app.active_count(), 1);
        assert!(press(&mut app, KeyCode::Char('q')).is_none());
        assert_eq!(app.mode, Mode::ConfirmQuit);
        assert!(matches!(
            press(&mut app, KeyCode::Char('y')),
            Some(Action::Quit)
        ));
        app.pause_all();
        assert_eq!(app.active_count(), 0);
        assert_eq!(app.current().unwrap().status, Status::Paused);
    }

    #[test]
    fn quitting_with_nothing_running_is_immediate() {
        let mut idle = app();
        assert!(matches!(
            press(&mut idle, KeyCode::Char('q')),
            Some(Action::Quit)
        ));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(idle.handle_key(ctrl_c), Some(Action::Quit)));

        // Paused or finished downloads are not at risk either.
        let mut app = with_download();
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(
            press(&mut app, KeyCode::Char('q')),
            Some(Action::Quit)
        ));
    }

    #[test]
    fn quit_confirmation_can_be_declined() {
        for decline in [
            KeyCode::Char('n'),
            KeyCode::Esc,
            KeyCode::Char('q'),
            KeyCode::Char('a'),
        ] {
            let mut app = with_download();
            press(&mut app, KeyCode::Char('q'));
            assert!(press(&mut app, decline).is_none());
            assert_eq!(app.mode, Mode::Normal);
            assert_eq!(app.active_count(), 1);
        }
        let mut app = with_download();
        press(&mut app, KeyCode::Char('q'));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Quit)
        ));
    }

    #[test]
    fn ctrl_c_asks_first_then_quits() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut app = with_download();
        assert!(app.handle_key(ctrl_c).is_none());
        assert_eq!(app.mode, Mode::ConfirmQuit);
        assert!(matches!(app.handle_key(ctrl_c), Some(Action::Quit)));
    }

    fn themed_app() -> App {
        let mut app = app();
        app.set_themes(theme::builtin_themes(), "default");
        app
    }

    #[test]
    fn theme_picker_previews_saves_and_cancels() {
        let mut app = themed_app();
        assert_eq!(app.theme.name, "default");
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.mode, Mode::Theme);
        press(&mut app, KeyCode::Down);
        let previewed = app.theme.name.clone();
        assert_ne!(previewed, "default", "moving previews the theme live");
        press(&mut app, KeyCode::Esc);
        assert_eq!(
            (app.mode.clone(), app.theme.name.as_str()),
            (Mode::Normal, "default")
        );

        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        let chosen = app.theme.name.clone();
        let Some(Action::SaveTheme { name, transparent }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a save");
        };
        assert_eq!((name, transparent), (chosen.clone(), None));
        assert_eq!(app.theme.name, chosen);

        // Reopening starts on the current theme; cancel keeps it.
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.theme_picker.as_ref().unwrap().cursor, 2);
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.theme.name, chosen);
    }

    #[test]
    fn theme_picker_toggles_the_background_and_reports_the_change() {
        let mut app = themed_app();
        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.theme.bg, ratatui::style::Color::Reset);
        press(&mut app, KeyCode::Char('b'));
        assert!(!app.transparent);
        assert_ne!(app.theme.bg, ratatui::style::Color::Reset);
        // Cancelling restores the background too.
        press(&mut app, KeyCode::Esc);
        assert!(app.transparent);
        assert_eq!(app.theme.name, "default");

        press(&mut app, KeyCode::Char('T'));
        press(&mut app, KeyCode::Char('b'));
        let Some(Action::SaveTheme { transparent, .. }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a save");
        };
        assert_eq!(transparent, Some(false));
    }

    #[test]
    fn theme_picker_cursor_is_bounded() {
        let mut app = themed_app();
        press(&mut app, KeyCode::Char('T'));
        for _ in 0..100 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(
            app.theme_picker.as_ref().unwrap().cursor,
            app.themes.len() - 1
        );
        for _ in 0..100 {
            press(&mut app, KeyCode::Up);
        }
        assert_eq!(app.theme_picker.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn pasting_outside_a_dialog_opens_add() {
        let mut app = app();
        app.handle_paste("  https://example.com/v\n");
        assert_eq!(app.mode, Mode::Add);
        assert_eq!(app.input.text(), "https://example.com/v");
        app.handle_paste("?t=1");
        assert_eq!(app.input.text(), "https://example.com/v?t=1");
    }

    #[test]
    fn ids_are_unique_and_increasing() {
        let mut app = with_download();
        let first = app.current().unwrap().id;
        press(&mut app, KeyCode::Char('a'));
        type_text(&mut app, "u");
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        app.probed(seq, url, Ok(probe()));
        press(&mut app, KeyCode::Enter);
        assert!(app.current().unwrap().id > first);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn status_messages_expire() {
        let mut app = app();
        app.set_status("hi", false);
        assert_eq!(app.status(), Some(("hi", false)));
        app.status.as_mut().unwrap().2 = Instant::now() - STATUS_TTL;
        assert!(app.status().is_none());
    }
}
