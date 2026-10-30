//! Everything that talks to yt-dlp: probing a URL for its formats, building a
//! download command, and parsing what it prints. No UI state lives here.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc::UnboundedSender,
};

use crate::config::Config;

/// Prefix of the lines we ask yt-dlp to print, so they cannot be confused with
/// anything a site puts in a title.
const PROGRESS_TAG: &str = "[ytdlptui]";
const FILE_TAG: &str = "[ytdlptui-file]";

/// What a format contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Picture and sound in one stream.
    Both,
    /// Picture only; downloaded together with the best audio.
    VideoOnly,
    AudioOnly,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Both => "video+audio",
            Kind::VideoOnly => "video",
            Kind::AudioOnly => "audio",
        }
    }
}

/// One downloadable format as reported by `yt-dlp -J`.
#[derive(Debug, Clone, PartialEq)]
pub struct Format {
    pub id: String,
    pub ext: String,
    pub resolution: String,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub vcodec: String,
    pub acodec: String,
    pub size: Option<u64>,
    pub tbr: Option<f64>,
    pub note: String,
    pub kind: Kind,
}

/// What a probe learned about a URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub title: String,
    pub duration: Option<u64>,
    pub choices: Vec<Choice>,
}

/// A row in the format picker.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// Short description kept with the download, e.g. `1080p60 mp4 [137]`.
    pub label: String,
    /// The `-f` argument.
    pub selector: String,
    /// Expected download size including the audio that is merged in.
    pub size: Option<u64>,
    /// `None` for the two "best" shortcuts.
    pub format: Option<Format>,
    /// Explanation shown for the shortcuts.
    pub note: String,
}

#[derive(Deserialize)]
struct RawInfo {
    title: Option<String>,
    id: Option<String>,
    duration: Option<f64>,
    formats: Option<Vec<RawFormat>>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: String,
    ext: Option<String>,
    resolution: Option<String>,
    height: Option<u32>,
    fps: Option<f64>,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<f64>,
    filesize_approx: Option<f64>,
    tbr: Option<f64>,
    format_note: Option<String>,
}

fn is_none(codec: &Option<String>) -> bool {
    codec.as_deref() == Some("none")
}

impl RawFormat {
    /// `None` for formats that are not a playable download (storyboards).
    fn into_format(self) -> Option<Format> {
        let note = self.format_note.unwrap_or_default();
        let ext = self.ext.unwrap_or_default();
        let (no_video, no_audio) = (is_none(&self.vcodec), is_none(&self.acodec));
        if ext == "mhtml" || note.contains("storyboard") || (no_video && no_audio) {
            return None;
        }
        // An unknown audio codec (`null`) on a stream that does have video is
        // a combined stream; only an explicit "none" means video-only.
        let kind = if no_video {
            Kind::AudioOnly
        } else if no_audio {
            Kind::VideoOnly
        } else {
            Kind::Both
        };
        Some(Format {
            id: self.format_id,
            ext,
            resolution: self.resolution.unwrap_or_default(),
            height: self.height,
            fps: self.fps,
            vcodec: self.vcodec.unwrap_or_default(),
            acodec: self.acodec.unwrap_or_default(),
            size: self
                .filesize
                .or(self.filesize_approx)
                .filter(|s| *s > 0.0)
                .map(|s| s as u64),
            tbr: self.tbr,
            note,
            kind,
        })
    }
}

/// `1080p60`, `audio`, or the raw resolution when there is no height.
pub fn quality(f: &Format) -> String {
    if f.kind == Kind::AudioOnly {
        return "audio".into();
    }
    match (f.height, f.fps) {
        (Some(h), Some(fps)) if fps > 30.5 => format!("{h}p{}", fps.round() as u32),
        (Some(h), _) => format!("{h}p"),
        _ if f.resolution.is_empty() => "video".into(),
        _ => f.resolution.clone(),
    }
}

/// Parse `yt-dlp -J` output into the choices offered to the user.
pub fn parse_probe(json: &str) -> Result<Probe> {
    let info: RawInfo =
        serde_json::from_str(json).wrap_err("yt-dlp printed something unexpected")?;
    let mut formats: Vec<Format> = info
        .formats
        .unwrap_or_default()
        .into_iter()
        .filter_map(RawFormat::into_format)
        .collect();
    if formats.is_empty() {
        bail!("no downloadable formats found for this URL");
    }

    // Pictures first, tallest and then fattest on top; audio-only last.
    let rank = |f: &Format| (f.kind == Kind::AudioOnly, std::cmp::Reverse(f.height));
    formats.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then(
            b.tbr
                .unwrap_or(0.0)
                .partial_cmp(&a.tbr.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    // What `+ba` will add to a video-only download.
    let audio_size = formats
        .iter()
        .filter(|f| f.kind == Kind::AudioOnly)
        .max_by(|a, b| {
            a.tbr
                .unwrap_or(0.0)
                .partial_cmp(&b.tbr.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .and_then(|f| f.size);
    let has_audio = formats.iter().any(|f| f.kind == Kind::AudioOnly);

    let mut choices = vec![
        Choice {
            label: "best".into(),
            selector: "bv*+ba/b".into(),
            size: None,
            format: None,
            note: "best video + audio".into(),
        },
        Choice {
            label: "best audio".into(),
            selector: "ba/b".into(),
            size: None,
            format: None,
            note: "best audio only".into(),
        },
    ];
    for f in formats {
        let (selector, size) = match f.kind {
            Kind::VideoOnly if has_audio => (
                format!("{}+ba/{}", f.id, f.id),
                f.size.map(|s| s + audio_size.unwrap_or(0)),
            ),
            _ => (f.id.clone(), f.size),
        };
        choices.push(Choice {
            label: format!("{} {} [{}]", quality(&f), f.ext, f.id),
            selector,
            size,
            format: Some(f),
            note: String::new(),
        });
    }

    Ok(Probe {
        title: info.title.or(info.id).unwrap_or_else(|| "untitled".into()),
        duration: info.duration.map(|d| d as u64),
        choices,
    })
}

fn command(config: &Config) -> Command {
    let mut cmd = Command::new(&config.yt_dlp);
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    cmd
}

/// `yt-dlp --version`, to fail early with a useful message when it is missing.
pub async fn version(config: &Config) -> Result<String> {
    let out = command(config)
        .arg("--version")
        .output()
        .await
        .wrap_err_with(|| {
            format!(
                "cannot run '{}': install yt-dlp or set `yt_dlp` in the config",
                config.yt_dlp
            )
        })?;
    if !out.status.success() {
        bail!(
            "'{} --version' failed: {}",
            config.yt_dlp,
            last_error(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The most useful line of yt-dlp's stderr: the last `ERROR:` one, else the
/// last non-empty line.
fn last_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let all: Vec<&str> = lines.by_ref().collect();
    let line = all
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or(all.last())
        .copied()
        .unwrap_or("yt-dlp failed without saying why");
    line.trim_start_matches("ERROR:").trim().to_string()
}

/// Ask yt-dlp which formats `url` offers. Dropping the future kills the process.
pub async fn probe(config: &Config, url: &str) -> Result<Probe> {
    let out = command(config)
        .args(["-J", "--no-playlist", "--no-warnings"])
        .args(&config.extra_args)
        .arg("--")
        .arg(url)
        .output()
        .await
        .wrap_err_with(|| format!("cannot run '{}'", config.yt_dlp))?;
    if !out.status.success() {
        return Err(eyre!(last_error(&out.stderr)));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

/// One progress report for a single stream (video and audio are separate).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub finished: bool,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress(Progress),
    /// The final file, printed once it is in place.
    File(PathBuf),
}

fn number(s: &str) -> Option<u64> {
    s.parse::<f64>()
        .ok()
        .filter(|n| *n >= 0.0)
        .map(|n| n as u64)
}

/// Parse one line of yt-dlp's stdout; anything else is ignored.
pub fn parse_line(line: &str) -> Option<Event> {
    if let Some(path) = line.strip_prefix(FILE_TAG) {
        return Some(Event::File(PathBuf::from(path)));
    }
    let fields: Vec<&str> = line.strip_prefix(PROGRESS_TAG)?.split('|').collect();
    let [status, downloaded, total, estimate, speed, ..] = fields[..] else {
        return None;
    };
    Some(Event::Progress(Progress {
        finished: status == "finished",
        downloaded: number(downloaded).unwrap_or(0),
        total: number(total)
            .or_else(|| number(estimate))
            .filter(|t| *t > 0),
        speed: number(speed),
    }))
}

/// Everything needed to start (or resume) one download.
#[derive(Debug, Clone)]
pub struct Job {
    pub url: String,
    pub selector: String,
    pub dir: PathBuf,
    /// yt-dlp keeps partial files here, so removing a download can clean up
    /// exactly what it made.
    pub temp_dir: PathBuf,
}

/// What a running download reports back.
#[derive(Debug)]
pub enum Update {
    Event(Event),
    /// yt-dlp exited; `Err` carries its last error line.
    Exited(std::result::Result<(), String>),
}

fn download_args(config: &Config, job: &Job) -> Vec<String> {
    let template = format!(
        "download:{PROGRESS_TAG}%(progress.status)s|%(progress.downloaded_bytes)s|\
         %(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s"
    );
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut args: Vec<String> = [
        "--newline",
        "--no-colors",
        "--no-warnings",
        "--no-playlist",
        "--progress",
        "--no-simulate",
        "--progress-template",
        &template,
        "--print",
        &format!("after_move:{FILE_TAG}%(filepath)s"),
        "-f",
        &job.selector,
        "-P",
        &format!("home:{}", path(&job.dir)),
        "-P",
        &format!("temp:{}", path(&job.temp_dir)),
        "-o",
        "%(title)s [%(id)s].%(ext)s",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(config.extra_args.iter().cloned());
    args.push("--".into());
    args.push(job.url.clone());
    args
}

/// Run a download to the end, sending updates as they happen. Run this in a
/// task and abort the task to stop it: the process is killed when dropped, and
/// yt-dlp resumes from its partial files next time.
pub async fn download(config: Config, id: u64, job: Job, tx: UnboundedSender<(u64, Update)>) {
    let result = run(&config, &job, |event| {
        let _ = tx.send((id, Update::Event(event)));
    })
    .await;
    let _ = tx.send((id, Update::Exited(result)));
}

async fn run(
    config: &Config,
    job: &Job,
    mut on_event: impl FnMut(Event),
) -> std::result::Result<(), String> {
    let mut child = command(config)
        .args(download_args(config, job))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run '{}': {e}", config.yt_dlp))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    // Drained on its own so a chatty yt-dlp can never block on a full pipe.
    let errors = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await;
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(event) = parse_line(&line) {
            on_event(event);
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let stderr = errors.await.unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(last_error(&stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"{
        "id": "abc", "title": "A video", "duration": 19.0,
        "formats": [
            {"format_id": "sb0", "ext": "mhtml", "format_note": "storyboard", "vcodec": "none", "acodec": "none"},
            {"format_id": "140", "ext": "m4a", "resolution": "audio only", "vcodec": "none", "acodec": "mp4a.40.2", "filesize": 300, "tbr": 129.0},
            {"format_id": "249", "ext": "webm", "vcodec": "none", "acodec": null, "tbr": 47.0},
            {"format_id": "134", "ext": "mp4", "resolution": "320x240", "height": 240, "fps": 15, "vcodec": "avc1", "acodec": "none", "filesize": 1000, "tbr": 136.0},
            {"format_id": "299", "ext": "mp4", "resolution": "1920x1080", "height": 1080, "fps": 60, "vcodec": "avc1", "acodec": "none", "filesize_approx": 5000.5, "tbr": 5000.0},
            {"format_id": "18", "ext": "mp4", "resolution": "640x360", "height": 360, "fps": 30, "vcodec": "avc1", "acodec": "mp4a.40.2", "filesize": 2000, "tbr": 300.0}
        ]
    }"#;

    #[test]
    fn probe_orders_and_filters_formats() {
        let probe = parse_probe(INFO).unwrap();
        assert_eq!(probe.title, "A video");
        assert_eq!(probe.duration, Some(19));
        let ids: Vec<&str> = probe
            .choices
            .iter()
            .map(|c| c.format.as_ref().map_or("-", |f| f.id.as_str()))
            .collect();
        // Two shortcuts, then tallest first, audio last, storyboard gone.
        assert_eq!(ids, ["-", "-", "299", "18", "134", "140", "249"]);
    }

    #[test]
    fn video_only_formats_get_audio_added() {
        let probe = parse_probe(INFO).unwrap();
        let by_id = |id: &str| {
            probe
                .choices
                .iter()
                .find(|c| c.format.as_ref().is_some_and(|f| f.id == id))
                .unwrap()
        };
        assert_eq!(by_id("299").selector, "299+ba/299");
        assert_eq!(by_id("299").size, Some(5000 + 300));
        assert_eq!(by_id("299").label, "1080p60 mp4 [299]");
        assert_eq!(by_id("18").selector, "18");
        assert_eq!(by_id("140").selector, "140");
        assert_eq!(by_id("140").label, "audio m4a [140]");
        // Unknown audio codec on an audio-only stream is still audio.
        assert_eq!(by_id("249").format.as_ref().unwrap().kind, Kind::AudioOnly);
        assert_eq!(probe.choices[0].selector, "bv*+ba/b");
    }

    #[test]
    fn video_only_without_any_audio_stays_alone() {
        let json = r#"{"title":"t","formats":[
            {"format_id":"v","ext":"mp4","height":720,"vcodec":"avc1","acodec":"none"}]}"#;
        let probe = parse_probe(json).unwrap();
        assert_eq!(probe.choices[2].selector, "v");
    }

    #[test]
    fn probe_errors_are_readable() {
        assert!(parse_probe("not json").is_err());
        let err = parse_probe(r#"{"title":"t","formats":[]}"#).unwrap_err();
        assert!(err.to_string().contains("no downloadable formats"));
    }

    #[test]
    fn parses_progress_lines() {
        let ev = parse_line("[ytdlptui]downloading|1024|195278|NA|79411.4|2").unwrap();
        assert_eq!(
            ev,
            Event::Progress(Progress {
                finished: false,
                downloaded: 1024,
                total: Some(195_278),
                speed: Some(79_411),
            })
        );
        // Fragmented streams only have an estimate; a stalled one has no speed.
        let Event::Progress(p) = parse_line("[ytdlptui]downloading|10|NA|500.0|NA|NA").unwrap()
        else {
            panic!("not progress");
        };
        assert_eq!((p.total, p.speed), (Some(500), None));
        let Event::Progress(p) = parse_line("[ytdlptui]finished|10|10|NA|NA|NA").unwrap() else {
            panic!("not progress");
        };
        assert!(p.finished);
    }

    #[test]
    fn parses_the_final_file_and_ignores_noise() {
        assert_eq!(
            parse_line("[ytdlptui-file]/tmp/a [x].mp4"),
            Some(Event::File(PathBuf::from("/tmp/a [x].mp4")))
        );
        assert_eq!(parse_line("[download] 50% of 3MiB"), None);
        assert_eq!(parse_line("[ytdlptui]garbled"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn picks_the_error_line() {
        let stderr = b"WARNING: x\nERROR: [youtube] abc: Video unavailable\n";
        assert_eq!(last_error(stderr), "[youtube] abc: Video unavailable");
        assert_eq!(last_error(b"boom\n"), "boom");
        assert_eq!(last_error(b""), "yt-dlp failed without saying why");
    }

    #[test]
    fn the_url_cannot_become_an_option() {
        let job = Job {
            url: "--exec=evil".into(),
            selector: "b".into(),
            dir: "/d".into(),
            temp_dir: "/d/.t".into(),
        };
        let args = download_args(&Config::default(), &job);
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[dashes + 1], "--exec=evil");
        assert_eq!(args.len(), dashes + 2);
    }
}
