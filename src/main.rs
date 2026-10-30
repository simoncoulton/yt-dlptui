mod app;
mod config;
mod download;
mod fmt;
mod theme;
mod tui;
mod ui;
mod ytdlp;

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use color_eyre::eyre::{Result, WrapErr, bail};

use app::App;

/// A terminal UI for yt-dlp: add a URL, pick a format, watch it download.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// A video URL to look up on startup.
    url: Option<String>,

    /// Directory to download into [default: `download_dir` from the config,
    /// else your Downloads folder].
    #[arg(short, long)]
    dir: Option<PathBuf>,

    /// Config file to use [default: ~/.config/yt-dlptui/config.yml].
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Use this theme instead of the one in the config for this run.
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Print the available themes and exit.
    #[arg(long)]
    list_themes: bool,
}

fn main() -> ExitCode {
    color_eyre::install().ok();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("yt-dlptui: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(p) => p,
        None => config::default_path()?,
    };
    let config = config::load(&config_path)?;
    // Built-ins are written out as files on first run, then everything in the
    // themes folder next to the config is offered.
    let themes_dir = config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("themes");
    let themes = theme::load_themes(&themes_dir, &config.themes).wrap_err_with(|| {
        format!(
            "invalid themes (config {} or folder {})",
            config_path.display(),
            themes_dir.display()
        )
    })?;
    if cli.list_themes {
        for t in &themes {
            println!("{}", t.name);
        }
        return Ok(());
    }

    let theme_name = cli.theme.clone().unwrap_or_else(|| config.theme.clone());
    if theme::find(&themes, &theme_name).is_none() {
        let names: Vec<&str> = themes.iter().map(|t| t.name.as_str()).collect();
        bail!(
            "unknown theme '{theme_name}'; available themes: {}",
            names.join(", ")
        );
    }
    let download_dir = match cli.dir {
        Some(d) => d,
        None => config.download_dir()?,
    };

    // Fail before taking over the terminal if yt-dlp is not there.
    ytdlp::version(&config).await?;

    let store_path = config::data_dir()?.join("downloads.json");
    let downloads = download::load(&store_path)?;
    let mut app = App::new(theme::default_theme(), download_dir, downloads);
    app.transparent = config.transparent_background;
    app.set_themes(themes, &theme_name);

    let terminal = tui::init_terminal()?;
    let result = tui::run(
        terminal,
        app,
        tui::Settings {
            config,
            config_path,
            store_path,
            url: cli.url,
        },
    )
    .await;
    tui::restore_terminal();
    result
}
