//! Pure rendering: `draw(frame, &app)` has no side effects on app state.
//! Every colour comes from `app.theme`.

use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Position, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Sparkline,
        Table, TableState, Wrap,
    },
};

use crate::{
    app::{App, FormatPicker, Input, Mode},
    download::{Download, Status},
    fmt,
    theme::Theme,
    ytdlp,
};

pub fn draw(frame: &mut Frame, app: &App) {
    let t = &app.theme;
    // The theme's page colour, or the terminal's own when transparent.
    frame.render_widget(Block::new().style(t.base()), frame.area());

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(14),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [list, details] = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).areas(body);

    draw_header(frame, app, header);
    draw_list(frame, app, list);
    draw_details(frame, app, details);
    draw_footer(frame, app, footer);

    match &app.mode {
        Mode::Normal => {}
        Mode::Add => draw_add(frame, app),
        Mode::Probing => draw_probing(frame, app),
        Mode::Formats => {
            if let Some(picker) = &app.picker {
                draw_formats(frame, t, picker);
            }
        }
        Mode::ConfirmRemove { id, delete_files } => {
            if let Some(d) = app.downloads.iter().find(|d| d.id == *id) {
                draw_confirm(frame, t, d, *delete_files);
            }
        }
        Mode::ConfirmQuit => draw_confirm_quit(frame, app),
        Mode::Theme => draw_themes(frame, app),
        Mode::Help => draw_help(frame, t),
    }
}

fn status_color(t: &Theme, s: Status) -> Color {
    match s {
        Status::Starting => t.special,
        Status::Downloading => t.success,
        Status::Paused => t.warning,
        Status::Done => t.info,
        Status::Error => t.error,
    }
}

/// Every pane and popup uses softly rounded corners.
fn rounded<'a>() -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded)
}

fn block<'a>(t: &Theme) -> Block<'a> {
    rounded().border_style(Style::new().fg(t.border))
}

/// A popup's frame: accent border, title and key hints.
fn modal<'a>(t: &Theme, heading: &str, hints: &str) -> Block<'a> {
    rounded()
        .border_style(Style::new().fg(t.accent))
        .title(title(t, format!(" {heading} ")))
        .title_bottom(hint_line(t, hints))
}

fn title<'a>(t: &Theme, text: impl Into<String>) -> Span<'a> {
    Span::styled(text.into(), Style::new().fg(t.accent).bold())
}

/// A modal's key hints, e.g. `" Enter save  Esc cancel "`: each key in the
/// accent colour, its meaning muted. Items are separated by two spaces and the
/// first word of an item is the key.
fn hint_line<'a>(t: &Theme, hints: &str) -> Line<'a> {
    let mut spans = vec![Span::styled(" ", t.muted())];
    for (i, item) in hints.trim().split("  ").enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", t.muted()));
        }
        let (k, label) = item.split_once(' ').unwrap_or((item, ""));
        spans.push(Span::styled(
            k.to_string(),
            Style::new().fg(t.accent).bold(),
        ));
        if !label.is_empty() {
            spans.push(Span::styled(format!(" {label}"), t.muted()));
        }
    }
    spans.push(Span::styled(" ", t.muted()));
    Line::from(spans)
}

fn key<'a>(t: &Theme, k: &'static str) -> Span<'a> {
    Span::styled(k, Style::new().fg(t.accent).bold())
}

/// Clear a popup area and restore the theme's base style.
fn popup(frame: &mut Frame, t: &Theme, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(t.base()), area);
}

/// A rect of up to `width` x `height`, centred, never larger than `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    rect
}

/// Truncate `s` to at most `width` chars, marking the cut with an ellipsis.
fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else if width == 0 {
        String::new()
    } else {
        let head: String = s.chars().take(width - 1).collect();
        format!("{head}…")
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = block(t).title(title(t, " yt-dlptui ")).title_bottom(
        Line::styled(format!(" {} ", app.download_dir.display()), t.muted()).right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [text, spark] =
        Layout::horizontal([Constraint::Length(44), Constraint::Min(0)]).areas(inner);
    let line = Line::from(vec![
        Span::styled("↓ ", Style::new().fg(t.success)),
        Span::raw(fmt::rate_or_zero(app.total_speed())),
        Span::raw(format!(
            "   {} downloads, {} active",
            app.downloads.len(),
            app.active_count()
        )),
    ]);
    frame.render_widget(Paragraph::new(line), text);

    // Only show the most recent samples that fit the width.
    let skip = app.down_history.len().saturating_sub(spark.width as usize);
    let data: Vec<u64> = app.down_history.iter().skip(skip).copied().collect();
    frame.render_widget(
        Sparkline::default()
            .data(data)
            .style(Style::new().fg(t.success)),
        spark,
    );
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = block(t).title(title(t, " Downloads "));

    if app.downloads.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from("No downloads yet.").centered(),
            Line::from(vec![
                Span::raw("Press "),
                key(t, "a"),
                Span::raw(" to add a video URL."),
            ])
            .centered(),
        ])
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    // The format column only earns its width on wide terminals; narrower ones
    // still see it in the details pane.
    let show_format = area.width >= 100;
    let mut titles = vec!["Title"];
    if show_format {
        titles.push("Format");
    }
    titles.extend(["Size", "Progress", "Speed", "ETA", "Status"]);
    let header = Row::new(titles).style(Style::new().fg(t.accent).bold());

    let rows = app.downloads.iter().map(|d| {
        let pct = fmt::percent(d.downloaded, d.total);
        let color = Style::new().fg(status_color(t, d.status));
        let mut cells = vec![Cell::from(d.title.clone())];
        if show_format {
            cells.push(Cell::from(d.label.clone()).style(t.muted()));
        }
        cells.extend([
            Cell::from(size_text(d.total)),
            Cell::from(format!("{} {:>5.1}%", fmt::bar(pct, 10), pct)).style(color),
            Cell::from(fmt::rate(d.speed)),
            Cell::from(d.eta().map_or("-".into(), fmt::duration)),
            Cell::from(d.status.label()).style(color),
        ]);
        Row::new(cells)
    });

    let mut widths = vec![Constraint::Min(16)];
    if show_format {
        widths.push(Constraint::Length(22));
    }
    widths.extend([
        Constraint::Length(10),
        Constraint::Length(18),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Length(11),
    ]);

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(t.selection())
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

fn size_text(bytes: u64) -> String {
    if bytes == 0 {
        "-".into()
    } else {
        fmt::bytes(bytes)
    }
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = block(t).title(title(t, " Details "));
    let Some(d) = app.current() else {
        frame.render_widget(block, area);
        return;
    };
    frame.render_widget(
        Paragraph::new(detail_lines(t, d))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn detail_lines(t: &Theme, d: &Download) -> Vec<Line<'static>> {
    let label = |text: &'static str| Span::styled(format!("{text:<8}"), t.muted());
    let saved = d
        .file
        .as_ref()
        .map_or_else(|| d.dir.display().to_string(), |f| f.display().to_string());
    let pct = fmt::percent(d.downloaded, d.total);
    let mut progress = vec![
        label("Status"),
        Span::styled(d.status.label(), Style::new().fg(status_color(t, d.status))),
        Span::raw(format!(
            "  {} of {} ({pct:.1}%)",
            fmt::bytes(d.downloaded),
            size_text(d.total)
        )),
    ];
    if let Some(eta) = d.eta() {
        progress.push(Span::raw(format!("  {} left", fmt::duration(eta))));
    }
    let mut lines = vec![
        Line::from(vec![label("Title"), Span::raw(d.title.clone()).bold()]),
        Line::from(vec![label("URL"), Span::raw(d.url.clone())]),
        Line::from(vec![label("Format"), Span::raw(d.label.clone())]),
        Line::from(vec![label("Saved"), Span::raw(saved)]),
        Line::from(progress),
    ];
    if let Some(e) = &d.error {
        lines.push(Line::from(vec![
            label("Error"),
            Span::styled(e.clone(), Style::new().fg(t.error)),
        ]));
    }
    lines
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let line = if let Some((msg, is_error)) = app.status() {
        let color = if is_error { t.error } else { t.success };
        Line::from(Span::styled(msg.to_string(), Style::new().fg(color)))
    } else {
        // (key, label, rank): when the terminal is too narrow, the highest
        // rank (rightmost first) is dropped until the rest fit.
        let pause = match app.current().map(|d| d.status) {
            Some(Status::Paused) => "resume",
            Some(Status::Error) => "retry",
            _ => "pause",
        };
        let mut hints: Vec<(&'static str, &'static str, u8)> = vec![
            ("a", "add", 0),
            ("p", pause, 1),
            ("d", "remove", 1),
            ("q", "quit", 0),
            ("?", "help", 0),
        ];
        // These act on the selected download, so they mean nothing without one.
        if app.current().is_none() {
            hints.retain(|h| !matches!(h.0, "p" | "d"));
        }
        let width = |h: &[(&str, &str, u8)]| {
            h.iter()
                .map(|(k, text, _)| k.chars().count() + 1 + text.len() + 2)
                .sum::<usize>()
        };
        // The last hint's trailing spaces don't need room.
        while width(&hints) > area.width as usize + 2 {
            let worst = hints.iter().map(|h| h.2).max().unwrap_or(0);
            if worst == 0 {
                break;
            }
            if let Some(i) = hints.iter().rposition(|h| h.2 == worst) {
                hints.remove(i);
            }
        }
        Line::from(
            hints
                .into_iter()
                .flat_map(|(k, text, _)| [key(t, k), Span::raw(format!(" {text}  "))])
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Draw a single-line text field and return the cursor position.
fn draw_input(
    frame: &mut Frame,
    field: Rect,
    input: &Input,
    placeholder: &str,
    t: &Theme,
) -> Option<Position> {
    if field.width == 0 || field.height == 0 {
        return None;
    }
    let width = field.width as usize;
    let text: Vec<char> = input.text().chars().collect();
    if text.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder.to_string()).style(t.muted()),
            field,
        );
        return Some(Position::new(field.x, field.y));
    }
    // Scroll horizontally so the cursor is always visible.
    let cursor = input.cursor();
    let start = (cursor + 1).saturating_sub(width);
    let visible: String = text.iter().skip(start).take(width).collect();
    frame.render_widget(Paragraph::new(visible), field);
    let col = (cursor - start).min(width - 1) as u16;
    Some(Position::new(field.x + col, field.y))
}

fn draw_add(frame: &mut Frame, app: &App) {
    let t = &app.theme;
    let full = frame.area();
    let area = centered(full, full.width.saturating_sub(4).clamp(1, 90), 7);
    popup(frame, t, area);
    let block = modal(t, "Add download", "Enter list formats  Esc cancel");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [hint, field, _, err] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new("URL of the video or page (paste works)").style(t.muted()),
        hint,
    );
    if let Some(pos) = draw_input(frame, field, &app.input, "https://", t) {
        frame.set_cursor_position(pos);
    }
    if let Some(e) = &app.add_error {
        frame.render_widget(
            Paragraph::new(e.clone())
                .style(Style::new().fg(t.error))
                .wrap(Wrap { trim: true }),
            err,
        );
    }
}

fn draw_probing(frame: &mut Frame, app: &App) {
    let t = &app.theme;
    let full = frame.area();
    let width = full.width.saturating_sub(4).clamp(1, 70);
    let area = centered(full, width, 6);
    popup(frame, t, area);
    let block = modal(t, "Checking formats", "Esc cancel");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let dots = ".".repeat(app.ticks % 4);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Asking yt-dlp what this offers{dots}")),
            Line::styled(
                fit(app.input.text().trim(), inner.width as usize),
                t.muted(),
            ),
        ]),
        inner,
    );
}

fn draw_formats(frame: &mut Frame, t: &Theme, picker: &FormatPicker) {
    let full = frame.area();
    let width = full.width.saturating_sub(4).clamp(1, 110);
    let height = (picker.choices.len() as u16 + 6).min(full.height.saturating_sub(2).max(1));
    let area = centered(full, width, height);
    popup(frame, t, area);
    let block = modal(
        t,
        "Choose a format",
        "Enter download  j/k move  g/G ends  Esc back",
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [info, table_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
    let mut about =
        vec![Span::raw(fit(&picker.title, (info.width as usize).saturating_sub(12))).bold()];
    if let Some(secs) = picker.duration {
        about.push(Span::styled(
            format!("  {}", fmt::duration(secs)),
            t.muted(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(about)), info);

    let header = Row::new([
        "Format", "Ext", "Quality", "Video", "Audio", "Size", "Type", "Note",
    ])
    .style(Style::new().fg(t.accent).bold());
    let rows = picker.choices.iter().map(|c| match &c.format {
        None => Row::new([
            Cell::from(c.label.clone()),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(c.note.clone()).style(t.muted()),
        ]),
        Some(f) => Row::new([
            Cell::from(fit(&f.id, 12)),
            Cell::from(f.ext.clone()),
            Cell::from(ytdlp::quality(f)),
            Cell::from(codec(&f.vcodec)),
            Cell::from(codec(&f.acodec)),
            Cell::from(c.size.map_or("-".into(), fmt::bytes)),
            Cell::from(f.kind.label()),
            Cell::from(f.note.clone()).style(t.muted()),
        ]),
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Min(0),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(t.selection())
    .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(table, table_area, &mut state);
}

/// `avc1.4D400B` -> `avc1`; unknown or absent -> `-`.
fn codec(full: &str) -> String {
    match full.split('.').next().unwrap_or("") {
        "" | "none" => "-".into(),
        short => short.to_string(),
    }
}

fn draw_confirm(frame: &mut Frame, t: &Theme, d: &Download, delete_files: bool) {
    let full = frame.area();
    let area = centered(full, full.width.saturating_sub(4).clamp(1, 70), 9);
    popup(frame, t, area);
    let (heading, lines) = if delete_files {
        (
            " Remove download AND DELETE the video? ",
            vec![
                Line::from(d.title.clone()).bold(),
                Line::from(vec![
                    Span::styled("Saved in ", t.muted()),
                    Span::raw(d.dir.display().to_string()),
                ]),
                Line::from(""),
                Line::styled(
                    "This permanently deletes the downloaded file.",
                    Style::new().fg(t.error).bold(),
                ),
                Line::from(vec![
                    Span::styled("y", Style::new().fg(t.error).bold()),
                    Span::raw(" yes, delete it    "),
                    key(t, "n"),
                    Span::raw(" / Esc  cancel"),
                ]),
            ],
        )
    } else {
        (
            " Remove download? ",
            vec![
                Line::from(d.title.clone()).bold(),
                Line::styled(
                    "A finished video stays on disk; unfinished parts are deleted.",
                    t.muted(),
                ),
                Line::from(""),
                Line::from(vec![
                    key(t, "y"),
                    Span::raw(" / Enter  remove    "),
                    key(t, "n"),
                    Span::raw(" / Esc  cancel"),
                ]),
                Line::from(vec![
                    Span::styled("D", Style::new().fg(t.error).bold()),
                    Span::raw("  remove and delete the video instead…"),
                ]),
            ],
        )
    };
    let accent = if delete_files { t.error } else { t.accent };
    let block = rounded()
        .border_style(Style::new().fg(accent))
        .title(Span::styled(heading, Style::new().fg(accent).bold()));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_confirm_quit(frame: &mut Frame, app: &App) {
    let t = &app.theme;
    let full = frame.area();
    let area = centered(full, full.width.saturating_sub(4).clamp(1, 62), 7);
    popup(frame, t, area);
    let n = app.active_count();
    let lines = vec![
        Line::from(format!(
            "{n} download{} in progress.",
            if n == 1 { " is" } else { "s are" }
        ))
        .bold(),
        Line::styled(
            "They pause when you quit; press p to resume them next time.",
            t.muted(),
        ),
        Line::from(""),
        Line::from(vec![
            key(t, "y"),
            Span::raw(" / Enter  quit    "),
            key(t, "n"),
            Span::raw(" / Esc  keep downloading"),
        ]),
    ];
    let block = rounded()
        .border_style(Style::new().fg(t.warning))
        .title(Span::styled(" Quit? ", Style::new().fg(t.warning).bold()));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_themes(frame: &mut Frame, app: &App) {
    let t = &app.theme;
    let Some(picker) = &app.theme_picker else {
        return;
    };
    let full = frame.area();
    let height = (app.themes.len() as u16 + 3).clamp(4, full.height.saturating_sub(2).max(4));
    let area = centered(full, full.width.saturating_sub(4).clamp(1, 56), height);
    popup(frame, t, area);
    let block = modal(
        t,
        "Theme",
        "↑↓ preview  b background  Enter save  Esc cancel",
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [bg_row, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Background  ", t.muted()),
            Span::styled(
                if app.transparent {
                    "transparent (terminal's own)"
                } else {
                    "theme colour"
                },
                Style::new().fg(t.accent),
            ),
        ])),
        bg_row,
    );

    let name_w = app
        .themes
        .iter()
        .map(|th| th.name.chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = app
        .themes
        .iter()
        .map(|th| {
            let mut spans = vec![Span::raw(format!("{:<name_w$}  ", th.name))];
            // Each theme's own colours, so you can compare before choosing.
            spans.extend(
                th.swatch()
                    .into_iter()
                    .map(|c| Span::styled("██ ", Style::new().fg(c))),
            );
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(t.selection())
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(picker.cursor));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn draw_help(frame: &mut Frame, t: &Theme) {
    let rows: [(&str, &str); 13] = [
        ("a", "Add a download: enter a URL, then choose a format"),
        ("p, Space", "Pause, resume or retry the selected download"),
        ("d, Del", "Remove it (asks first)"),
        ("D", "Remove it and delete the video (asks first)"),
        ("j k, ↓ ↑", "Move"),
        ("g G", "First / last"),
        ("T", "Pick a theme (live preview; b toggles the background)"),
        ("q, Ctrl-C", "Quit; asks first if downloads are running"),
        ("?", "This help"),
        ("", ""),
        ("Add dialog", "Enter lists formats; Ctrl-U clears the field"),
        ("Format list", "Enter downloads, Esc goes back to the URL"),
        ("Paste", "Pasting a URL on the list opens the Add dialog"),
    ];
    let full = frame.area();
    let area = centered(
        full,
        full.width.saturating_sub(4).clamp(1, 76),
        rows.len() as u16 + 2,
    );
    popup(frame, t, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, text)| {
            Line::from(vec![
                Span::styled(format!("{k:<12}"), Style::new().fg(t.accent).bold()),
                Span::raw(*text),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(modal(t, "Help", "Esc close")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Action,
        theme::{builtin_themes, default_theme, find},
        ytdlp::{Choice, Format, Kind, Probe},
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn render_buf(app: &App, w: u16, h: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let buf = render_buf(app, w, h);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn format(id: &str, kind: Kind, height: Option<u32>) -> Format {
        Format {
            id: id.into(),
            ext: "mp4".into(),
            resolution: String::new(),
            height,
            fps: None,
            vcodec: if kind == Kind::AudioOnly {
                "none"
            } else {
                "avc1.4D401F"
            }
            .into(),
            acodec: if kind == Kind::VideoOnly {
                "none"
            } else {
                "mp4a.40.2"
            }
            .into(),
            size: Some(1_500_000),
            tbr: None,
            note: "medium".into(),
            kind,
        }
    }

    fn probe() -> Probe {
        let mut choices = vec![Choice {
            label: "best".into(),
            selector: "bv*+ba/b".into(),
            size: None,
            format: None,
            note: "best video + audio, merged".into(),
        }];
        for (id, kind, h) in [
            ("137", Kind::VideoOnly, Some(1080)),
            ("140", Kind::AudioOnly, None),
        ] {
            let f = format(id, kind, h);
            choices.push(Choice {
                label: format!("{id} label"),
                selector: id.into(),
                size: f.size,
                format: Some(f),
                note: String::new(),
            });
        }
        Probe {
            title: "Me at the zoo".into(),
            duration: Some(19),
            choices,
        }
    }

    fn app_with(theme: Theme) -> App {
        App::new(theme, "/home/me/Videos".into(), Vec::new())
    }

    fn at_formats(theme: Theme) -> App {
        let mut app = app_with(theme);
        press(&mut app, KeyCode::Char('a'));
        for c in "https://example.com/v".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        app.probed(seq, url, Ok(probe()));
        app
    }

    fn with_download(theme: Theme) -> App {
        let mut app = at_formats(theme);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        app
    }

    #[test]
    fn empty_list_explains_how_to_start() {
        let text = render(&app_with(default_theme()), 80, 24);
        assert!(text.contains("yt-dlptui"));
        assert!(text.contains("No downloads yet."));
        assert!(text.contains("/home/me/Videos"));
        assert!(text.contains("add") && text.contains("quit") && text.contains("help"));
        // Nothing is selected, so no hints about it.
        assert!(!text.contains("pause") && !text.contains("remove"));
    }

    #[test]
    fn the_format_modal_lists_every_choice() {
        let text = render(&at_formats(default_theme()), 100, 24);
        for want in [
            "Choose a format",
            "Me at the zoo",
            "19s",
            "best",
            "best video + audio",
            "137",
            "1080p",
            "avc1",
            "1.4 MiB",
            "video",
            "140",
            "audio",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(text.contains("Enter download"));
    }

    #[test]
    fn the_list_shows_progress_and_details() {
        let mut app = with_download(default_theme());
        let id = app.current().unwrap().id;
        app.apply_update(
            id,
            crate::ytdlp::Update::Event(crate::ytdlp::Event::Progress(crate::ytdlp::Progress {
                finished: false,
                downloaded: 750_000,
                total: Some(1_500_000),
                speed: Some(150_000),
            })),
        );
        let text = render(&app, 120, 30);
        for want in [
            "Me at the zoo",
            "50.0%",
            "Downloading",
            "https://example.com/v",
            "5s left",
            "pause",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn errors_show_inside_the_dialog() {
        let mut app = app_with(default_theme());
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('x'));
        let Some(Action::Probe { seq, url }) = press(&mut app, KeyCode::Enter) else {
            panic!("expected a probe");
        };
        assert!(render(&app, 80, 24).contains("Checking formats"));
        app.probed(seq, url, Err("Unsupported URL: x".into()));
        let text = render(&app, 80, 24);
        assert!(text.contains("Unsupported URL: x") && text.contains("Add download"));
    }

    #[test]
    fn confirm_and_help_render() {
        let mut app = with_download(default_theme());
        press(&mut app, KeyCode::Char('d'));
        let text = render(&app, 80, 24);
        assert!(text.contains("Remove download?") && text.contains("Me at the zoo"));
        press(&mut app, KeyCode::Char('D'));
        assert!(render(&app, 80, 24).contains("AND DELETE"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('?'));
        assert!(render(&app, 80, 24).contains("Add a download"));
    }

    #[test]
    fn nothing_panics_on_tiny_terminals() {
        let mut app = with_download(default_theme());
        let mut screens = vec![];
        // One app per mode, from the list to every dialog.
        screens.push(with_download(default_theme()));
        screens.push(at_formats(default_theme()));
        let mut add = app_with(default_theme());
        press(&mut add, KeyCode::Char('a'));
        for _ in 0..200 {
            press(&mut add, KeyCode::Char('w'));
        }
        screens.push(add);
        press(&mut app, KeyCode::Char('d'));
        screens.push(app);
        let mut help = app_with(default_theme());
        press(&mut help, KeyCode::Char('?'));
        screens.push(help);
        let mut probing = app_with(default_theme());
        press(&mut probing, KeyCode::Char('a'));
        press(&mut probing, KeyCode::Char('x'));
        press(&mut probing, KeyCode::Enter);
        screens.push(probing);
        for app in &screens {
            for (w, h) in [
                (0, 0),
                (1, 1),
                (5, 3),
                (12, 4),
                (30, 10),
                (60, 14),
                (200, 60),
            ] {
                render_buf(app, w, h);
            }
        }
    }

    #[test]
    fn popups_keep_the_page_background_in_opaque_and_transparent_modes() {
        let nord = find(&builtin_themes(), "nord").unwrap().clone();
        for transparent in [false, true] {
            let theme = nord.clone().transparent(transparent);
            let want = theme.bg;
            assert_eq!(transparent, want == Color::Reset);
            for app in [at_formats(theme.clone()), with_download(theme.clone())] {
                let buf = render_buf(&app, 100, 30);
                // Every cell, popup interior included, has the page colour.
                for y in 0..30 {
                    for x in 0..100 {
                        let cell = &buf[(x, y)];
                        assert!(
                            cell.bg == want || cell.bg == theme.selection_bg,
                            "({x},{y}) {:?} has bg {:?}, want {want:?}",
                            cell.symbol(),
                            cell.bg
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn modal_key_hints_use_the_accent_colour() {
        let theme = find(&builtin_themes(), "dracula").unwrap().clone();
        let app = at_formats(theme.clone());
        let buf = render_buf(&app, 100, 24);
        let found = (0..24).any(|y| {
            (0..100).any(|x| {
                buf[(x, y)].symbol() == "E"
                    && buf[(x, y)].fg == theme.accent
                    && (1..5).all(|dx| {
                        ["n", "t", "e", "r"][dx as usize - 1] == buf[(x + dx, y)].symbol()
                    })
            })
        });
        assert!(found, "no accent-coloured `Enter` hint found");
    }

    #[test]
    fn quit_confirmation_and_theme_picker_render() {
        let mut app = with_download(default_theme());
        press(&mut app, KeyCode::Char('q'));
        let text = render(&app, 80, 24);
        assert!(
            text.contains("Quit?") && text.contains("1 download is in progress"),
            "{text}"
        );
        assert!(text.contains("keep downloading"));

        press(&mut app, KeyCode::Esc);
        app.set_themes(builtin_themes(), "default");
        press(&mut app, KeyCode::Char('T'));
        let text = render(&app, 80, 30);
        for want in [
            "Theme",
            "tokyo-night",
            "kanagawa",
            "Background",
            "Enter save",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn theme_picker_and_quit_dialog_keep_the_background_and_fit_tiny_terminals() {
        let nord = find(&builtin_themes(), "nord").unwrap().clone();
        for transparent in [false, true] {
            let mut app = with_download(nord.clone().transparent(transparent));
            app.transparent = transparent;
            app.set_themes(builtin_themes(), "nord");
            press(&mut app, KeyCode::Char('q'));
            let quit = render_buf(&app, 100, 30);
            press(&mut app, KeyCode::Esc);
            press(&mut app, KeyCode::Char('T'));
            let picker = render_buf(&app, 100, 30);
            for buf in [&quit, &picker] {
                for y in 0..30 {
                    for x in 0..100 {
                        let c = &buf[(x, y)];
                        assert!(
                            c.bg == app.theme.bg || c.bg == app.theme.selection_bg,
                            "({x},{y}) {:?} has bg {:?}",
                            c.symbol(),
                            c.bg
                        );
                    }
                }
            }
            for (w, h) in [(0, 0), (1, 1), (6, 3), (30, 8)] {
                render_buf(&app, w, h);
            }
            press(&mut app, KeyCode::Esc);
            press(&mut app, KeyCode::Char('q'));
            for (w, h) in [(0, 0), (1, 1), (6, 3), (30, 8)] {
                render_buf(&app, w, h);
            }
        }
    }

    #[test]
    fn long_titles_are_truncated_not_wrapped() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(fit("abc", 0), "");
    }
}
