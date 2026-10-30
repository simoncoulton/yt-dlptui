//! A download as the UI sees it, and the saved list that survives restarts.

use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::{
    config::write_atomic,
    ytdlp::{Choice, Event, Job, Progress},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// yt-dlp is running but has not reported anything yet.
    Starting,
    Downloading,
    Paused,
    Done,
    Error,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Starting => "Starting",
            Status::Downloading => "Downloading",
            Status::Paused => "Paused",
            Status::Done => "Done",
            Status::Error => "Error",
        }
    }

    /// Whether a yt-dlp process should be running.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Starting | Status::Downloading)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Download {
    pub id: u64,
    pub url: String,
    pub title: String,
    /// The format as the user saw it in the picker.
    pub label: String,
    pub selector: String,
    /// Size expected when the download was added; 0 when unknown.
    pub size_hint: u64,
    pub dir: PathBuf,
    pub status: Status,
    pub downloaded: u64,
    pub total: u64,
    pub file: Option<PathBuf>,
    pub error: Option<String>,
    // The rest is about the current run only.
    #[serde(skip)]
    pub speed: u64,
    /// Bytes of streams already finished in this run (video, then audio).
    #[serde(skip)]
    finished_bytes: u64,
    #[serde(skip)]
    stream_done: u64,
    #[serde(skip)]
    stream_total: u64,
}

impl Download {
    pub fn new(id: u64, url: String, title: String, choice: &Choice, dir: PathBuf) -> Self {
        Self {
            id,
            url,
            title,
            label: choice.label.clone(),
            selector: choice.selector.clone(),
            size_hint: choice.size.unwrap_or(0),
            dir,
            status: Status::Starting,
            downloaded: 0,
            total: choice.size.unwrap_or(0),
            file: None,
            error: None,
            speed: 0,
            finished_bytes: 0,
            stream_done: 0,
            stream_total: 0,
        }
    }

    pub fn job(&self) -> Job {
        Job {
            url: self.url.clone(),
            selector: self.selector.clone(),
            dir: self.dir.clone(),
            temp_dir: self.temp_dir(),
        }
    }

    /// Where yt-dlp keeps this download's partial files.
    pub fn temp_dir(&self) -> PathBuf {
        self.dir.join(".yt-dlptui").join(self.id.to_string())
    }

    /// (Re)start: forget the last run's numbers but keep what is on screen
    /// until yt-dlp reports again.
    pub fn restart(&mut self) {
        self.status = Status::Starting;
        self.error = None;
        self.speed = 0;
        self.finished_bytes = 0;
        self.stream_done = 0;
        self.stream_total = 0;
    }

    pub fn pause(&mut self) {
        self.status = Status::Paused;
        self.speed = 0;
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Progress(p) => self.apply_progress(p),
            Event::File(path) => self.file = Some(path),
        }
    }

    fn apply_progress(&mut self, p: Progress) {
        self.status = Status::Downloading;
        if p.finished {
            self.finished_bytes += p.downloaded;
            self.stream_done = 0;
            self.stream_total = 0;
            self.speed = 0;
        } else {
            self.stream_done = p.downloaded;
            self.stream_total = p.total.unwrap_or(0);
            self.speed = p.speed.unwrap_or(0);
        }
        self.downloaded = self.finished_bytes + self.stream_done;
        // The estimate from the picker covers the stream still to come; trust
        // what yt-dlp reports once it is larger.
        self.total = self
            .size_hint
            .max(self.finished_bytes + self.stream_total)
            .max(self.downloaded);
    }

    /// yt-dlp exited.
    pub fn exited(&mut self, result: Result<(), String>) {
        self.speed = 0;
        match result {
            Ok(()) => {
                self.status = Status::Done;
                self.error = None;
                // The real size replaces the estimate.
                if let Some(size) = self.file.as_ref().and_then(|f| fs::metadata(f).ok()) {
                    self.downloaded = size.len();
                } else if self.finished_bytes > 0 {
                    self.downloaded = self.finished_bytes;
                }
                self.total = self.downloaded;
                self.remove_empty_temp();
            }
            Err(e) => {
                self.status = Status::Error;
                self.error = Some(e);
            }
        }
    }

    /// yt-dlp moves finished files out of the temp folder; what is left is an
    /// empty shell (`remove_dir` refuses anything that still holds files).
    fn remove_empty_temp(&self) {
        let _ = fs::remove_dir(self.temp_dir());
        let _ = fs::remove_dir(self.dir.join(".yt-dlptui"));
    }

    /// Seconds left at the current speed.
    pub fn eta(&self) -> Option<u64> {
        (self.status == Status::Downloading && self.speed > 0 && self.total > self.downloaded)
            .then(|| (self.total - self.downloaded) / self.speed)
    }

    /// Delete what this download left on disk: the partial files, and the
    /// finished file when `delete_file`. Returns what failed.
    pub fn delete_files(&self, delete_file: bool) -> Vec<String> {
        let mut failed = Vec::new();
        let mut remove = |path: &Path, dir: bool| {
            let result = if dir {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            match result {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    failed.push(format!("{}: {e}", path.display()));
                }
                _ => {}
            }
        };
        remove(&self.temp_dir(), true);
        if delete_file && let Some(file) = &self.file {
            remove(file, false);
        }
        // The shared parent is only ours while it is empty.
        let _ = fs::remove_dir(self.dir.join(".yt-dlptui"));
        failed
    }
}

/// The saved list. A download that was running when the app closed comes back
/// paused: press `p` to resume where yt-dlp left off.
pub fn load(path: &Path) -> Result<Vec<Download>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).wrap_err_with(|| format!("cannot read {}", path.display())),
    };
    let mut list: Vec<Download> = serde_json::from_str(&text)
        .wrap_err_with(|| format!("{} is not a valid download list", path.display()))?;
    for d in &mut list {
        if d.status.is_active() {
            d.status = Status::Paused;
        }
    }
    Ok(list)
}

pub fn save(path: &Path, list: &[Download]) -> Result<()> {
    write_atomic(path, &serde_json::to_string_pretty(list)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(size: Option<u64>) -> Choice {
        Choice {
            label: "1080p mp4 [299]".into(),
            selector: "299+ba/299".into(),
            size,
            format: None,
            note: String::new(),
        }
    }

    fn dl(size: Option<u64>) -> Download {
        Download::new(7, "u".into(), "t".into(), &choice(size), "/tmp/dl".into())
    }

    fn progress(finished: bool, downloaded: u64, total: Option<u64>, speed: u64) -> Event {
        Event::Progress(Progress {
            finished,
            downloaded,
            total,
            speed: Some(speed),
        })
    }

    #[test]
    fn progress_spans_video_and_audio_streams() {
        let mut d = dl(Some(1500));
        assert_eq!(d.status, Status::Starting);
        d.apply(progress(false, 400, Some(1000), 100));
        assert_eq!(
            (d.status, d.downloaded, d.total),
            (Status::Downloading, 400, 1500)
        );
        d.apply(progress(true, 1000, Some(1000), 0));
        assert_eq!(d.downloaded, 1000);
        // The audio stream starts over at zero; the overall figure keeps climbing.
        d.apply(progress(false, 100, Some(500), 50));
        assert_eq!((d.downloaded, d.total), (1100, 1500));
        assert_eq!(d.eta(), Some(8));
    }

    #[test]
    fn total_grows_past_a_bad_estimate_and_works_without_one() {
        let mut d = dl(None);
        d.apply(progress(false, 10, Some(200), 10));
        assert_eq!(d.total, 200);
        let mut d = dl(Some(100));
        d.apply(progress(false, 150, Some(300), 10));
        assert_eq!(d.total, 300);
        let mut d = dl(None);
        d.apply(progress(false, 10, None, 0));
        assert_eq!((d.total, d.eta()), (10, None));
    }

    #[test]
    fn exit_decides_the_final_state() {
        let mut d = dl(Some(1500));
        d.dir = std::env::temp_dir().join(format!("yt-dlptui-exit-{}", std::process::id()));
        fs::create_dir_all(d.temp_dir()).unwrap();
        d.apply(progress(true, 1200, Some(1200), 0));
        d.exited(Ok(()));
        assert!(
            !d.dir.join(".yt-dlptui").exists(),
            "empty temp folder is removed"
        );
        let _ = fs::remove_dir_all(&d.dir);
        assert_eq!(
            (d.status, d.downloaded, d.total),
            (Status::Done, 1200, 1200)
        );

        let mut d = dl(None);
        d.exited(Err("Video unavailable".into()));
        assert_eq!(d.status, Status::Error);
        assert_eq!(d.error.as_deref(), Some("Video unavailable"));
        d.restart();
        assert_eq!((d.status, d.error), (Status::Starting, None));
    }

    #[test]
    fn saved_list_comes_back_paused() {
        let dir = std::env::temp_dir().join(format!("yt-dlptui-store-{}", std::process::id()));
        let path = dir.join("downloads.json");
        assert!(load(&path).unwrap().is_empty());
        let mut running = dl(Some(10));
        running.apply(progress(false, 5, Some(10), 1));
        let mut done = dl(None);
        done.id = 8;
        done.exited(Ok(()));
        save(&path, &[running, done]).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back[0].status, Status::Paused);
        assert_eq!(back[0].downloaded, 5);
        assert_eq!(back[1].status, Status::Done);
        fs::write(&path, "{").unwrap();
        assert!(format!("{:#}", load(&path).unwrap_err()).contains("downloads.json"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_removes_partials_and_optionally_the_file() {
        let dir = std::env::temp_dir().join(format!("yt-dlptui-del-{}", std::process::id()));
        let mut d = dl(None);
        d.dir = dir.clone();
        fs::create_dir_all(d.temp_dir()).unwrap();
        fs::write(d.temp_dir().join("v.part"), "x").unwrap();
        let file = dir.join("v.mp4");
        fs::write(&file, "x").unwrap();
        d.file = Some(file.clone());

        assert!(d.delete_files(false).is_empty());
        assert!(!d.temp_dir().exists());
        assert!(!dir.join(".yt-dlptui").exists());
        assert!(file.exists());
        assert!(d.delete_files(true).is_empty());
        assert!(!file.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
