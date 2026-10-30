//! The YAML config file and the places the app keeps its files.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::Deserialize;

use crate::theme::ThemeSpec;

/// Written on first start; every key is the default, commented out.
const DEFAULT_CONFIG: &str = "\
# yt-dlptui config. Unknown keys are errors.

# Where finished videos go (default: your Downloads folder).
# download_dir: ~/Downloads

# Colour theme: press T in the app to preview and pick one (saved here), or
# set it by hand. One of: default, tokyo-night, catppuccin-frappe, catppuccin-mocha,
# catppuccin-macchiato, catppuccin-latte, dracula, one-dark, gruvbox-dark,
# nord, solarized-dark, rose-pine, monokai, github-dark, everforest, kanagawa.
# The RGB themes need a true-colour terminal. Each built-in also has an
# editable file in the themes/ folder next to this config, and any other .yml
# there is a theme named after the file; or define one under `themes:` below.
theme: default

# Let the terminal's own background show instead of the theme's page colour.
transparent_background: true

# The yt-dlp program to run: a name on your PATH or a full path.
# yt_dlp: yt-dlp

# Extra arguments for every yt-dlp run, e.g. [\"--cookies-from-browser\", \"firefox\"].
# extra_args: []

# Your own themes. Every colour is optional and inherits from `base`
# (one of the built-in themes, default: `default`). Colours are #rrggbb,
# an ANSI name such as `red`, or a 0-255 index.
#
# themes:
#   my-theme:
#     base: dracula
#     accent: \"#ff79c6\"
#     fg: \"#f8f8f2\"
#   # Keys: bg fg muted accent border selection_bg success info warning error special
";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub download_dir: Option<String>,
    pub theme: String,
    pub transparent_background: bool,
    pub yt_dlp: String,
    pub extra_args: Vec<String>,
    pub themes: BTreeMap<String, ThemeSpec>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: None,
            theme: "default".into(),
            transparent_background: true,
            yt_dlp: "yt-dlp".into(),
            extra_args: Vec::new(),
            themes: BTreeMap::new(),
        }
    }
}

impl Config {
    /// The folder downloads go to, with a leading `~` expanded.
    pub fn download_dir(&self) -> Result<PathBuf> {
        let home = || dirs::home_dir().ok_or_else(|| eyre!("cannot determine your home directory"));
        match self.download_dir.as_deref().map(str::trim) {
            Some(dir) if !dir.is_empty() => match dir.strip_prefix('~') {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    Ok(home()?.join(rest.trim_start_matches('/')))
                }
                _ => Ok(PathBuf::from(dir)),
            },
            _ => match dirs::download_dir() {
                Some(dir) => Ok(dir),
                None => Ok(home()?.join("Downloads")),
            },
        }
    }
}

/// `$XDG_CONFIG_HOME/yt-dlptui/config.yml`, which is
/// `~/.config/yt-dlptui/config.yml` unless you have moved XDG_CONFIG_HOME.
pub fn default_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => dirs::home_dir()
            .ok_or_else(|| eyre!("cannot determine your home directory"))?
            .join(".config"),
    };
    Ok(base.join("yt-dlptui").join("config.yml"))
}

/// `~/.local/share/yt-dlptui` (`~/Library/Application Support/yt-dlptui` on
/// macOS): the saved download list.
pub fn data_dir() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| eyre!("cannot determine your data directory"))?
        .join("yt-dlptui"))
}

/// Parse config text. A file with only comments/whitespace is the defaults.
pub fn parse(text: &str) -> Result<Config, serde_yaml_ng::Error> {
    let blank = text
        .lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'));
    if blank {
        return Ok(Config::default());
    }
    serde_yaml_ng::from_str(text)
}

/// Read the config, writing the commented template first when there is none.
/// A broken file is an error naming the file, never a silent fallback.
pub fn load(path: &Path) -> Result<Config> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text).wrap_err_with(|| format!("invalid config {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Best effort: a read-only config directory just means defaults.
            let _ = write_atomic(path, DEFAULT_CONFIG);
            Ok(Config::default())
        }
        Err(e) => Err(e).wrap_err_with(|| format!("cannot read config {}", path.display())),
    }
}

/// Read the config for an in-place edit. A file that already fails to parse is
/// reported as such, so a hand-made mistake isn't blamed on the value being
/// saved.
fn read_for_edit(path: &Path) -> Result<String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
        Err(e) => return Err(e).wrap_err_with(|| format!("cannot read {}", path.display())),
    };
    parse(&text).wrap_err_with(|| {
        format!(
            "{} has an error, so it was not changed (fix it first)",
            path.display()
        )
    })?;
    Ok(text)
}

/// Set a top-level `key: value` in the file, keeping every other line (and
/// comments) untouched. `value` is raw YAML. The edit is checked to still
/// parse before anything is written.
pub fn set_value(path: &Path, key: &str, value: &str) -> Result<()> {
    let existing = read_for_edit(path)?;
    let updated = replace_key(&existing, key, value);
    parse(&updated).wrap_err_with(|| format!("refusing to write invalid {key}: {value}"))?;
    write_atomic(path, &updated)
}

fn replace_key(text: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}:");
    let mut replaced = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|line| {
            if !replaced && line.starts_with(&prefix) {
                replaced = true;
                // Keep a trailing comment (and the spacing before it), if any.
                let rest = &line[prefix.len()..];
                match rest.find(" #") {
                    Some(i) => format!("{key}: {value}{}", &rest[rest[..i].trim_end().len()..]),
                    None => format!("{key}: {value}"),
                }
            } else {
                line.to_string()
            }
        })
        .collect();
    if !replaced {
        // A top-level key at column 0 is valid after any preceding block.
        out.push(format!("{key}: {value}"));
    }
    out.join("\n") + "\n"
}

/// Quote a string as a YAML scalar if it needs it.
pub fn yaml_scalar(s: &str) -> String {
    serde_yaml_ng::to_string(s)
        .map(|t| t.trim_end().to_string())
        .unwrap_or_else(|_| format!("{s:?}"))
}

/// Write via a temp file + rename so a crash can't leave a half-written
/// file. Follows a symlinked file (e.g. managed by stow) instead of replacing
/// the link.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .ok_or_else(|| eyre!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, text)?;
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yt-dlptui-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fields compared one by one: `ThemeSpec` has no `PartialEq`.
    fn same(a: &Config, b: &Config) {
        assert_eq!(a.download_dir, b.download_dir);
        assert_eq!(a.theme, b.theme);
        assert_eq!(a.transparent_background, b.transparent_background);
        assert_eq!(a.yt_dlp, b.yt_dlp);
        assert_eq!(a.extra_args, b.extra_args);
        assert_eq!(a.themes.len(), b.themes.len());
    }

    #[test]
    fn default_template_equals_defaults() {
        let keys = [
            "download_dir",
            "theme",
            "transparent_background",
            "yt_dlp",
            "extra_args",
        ];
        // Every key line of the template, whether or not it is commented out.
        let all_keys: String = DEFAULT_CONFIG
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l))
            .filter(|l| keys.contains(&l.split(':').next().unwrap_or("")) && l.contains(": "))
            .map(|l| format!("{l}\n"))
            .collect();
        let parsed = parse(&all_keys).unwrap();
        let expected = Config {
            download_dir: Some("~/Downloads".into()),
            ..Config::default()
        };
        same(&parsed, &expected);
        same(&parse(DEFAULT_CONFIG).unwrap(), &Config::default());
    }

    #[test]
    fn missing_file_is_created_and_empty_file_is_default() {
        let dir = scratch("cfg");
        let path = dir.join("sub").join("config.yml");
        same(&load(&path).unwrap(), &Config::default());
        assert!(path.exists());
        same(&load(&path).unwrap(), &Config::default());
        fs::write(&path, "# nothing\n\n").unwrap();
        same(&load(&path).unwrap(), &Config::default());
    }

    #[test]
    fn bad_files_are_errors_naming_the_file() {
        let dir = scratch("bad");
        let path = dir.join("config.yml");
        fs::write(&path, "them: nord\n").unwrap();
        let err = format!("{:#}", load(&path).unwrap_err());
        assert!(err.contains("config.yml") && err.contains("them"), "{err}");
    }

    #[test]
    fn parses_custom_themes() {
        let c = parse("theme: mine\nthemes:\n  mine:\n    base: dracula\n    accent: '#ff0000'\n")
            .unwrap();
        assert_eq!(c.theme, "mine");
        assert!(c.themes.contains_key("mine"));
    }

    #[test]
    fn set_value_edits_in_place_keeping_comments() {
        let dir = scratch("set");
        let path = dir.join("config.yml");
        fs::write(
            &path,
            "# top\ntheme: default   # pick one\nyt_dlp: yt-dlp\n",
        )
        .unwrap();
        set_value(&path, "theme", &yaml_scalar("tokyo-night")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# top\ntheme: tokyo-night   # pick one\nyt_dlp: yt-dlp\n"
        );
        set_value(&path, "transparent_background", "false").unwrap();
        let c = load(&path).unwrap();
        assert_eq!(
            (c.theme.as_str(), c.transparent_background),
            ("tokyo-night", false)
        );
        // `theme` is not confused with `themes:`, and a missing key is appended.
        assert_eq!(
            replace_key("themes:\n  a: {}\n", "theme", "x"),
            "themes:\n  a: {}\ntheme: x\n"
        );
        assert_eq!(
            replace_key("# theme: foo\n", "theme", "x"),
            "# theme: foo\ntheme: x\n"
        );
    }

    #[test]
    fn set_value_creates_the_file_and_refuses_a_broken_one() {
        let dir = scratch("setnew");
        let path = dir.join("config.yml");
        set_value(&path, "theme", "nord").unwrap();
        assert_eq!(load(&path).unwrap().theme, "nord");
        fs::write(&path, "oops: 1\n").unwrap();
        let err = format!("{:#}", set_value(&path, "theme", "nord").unwrap_err());
        assert!(err.contains("was not changed"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "oops: 1\n");
        assert!(set_value(&dir.join("c2.yml"), "transparent_background", "perhaps").is_err());
    }

    #[test]
    fn set_value_follows_a_symlinked_config() {
        let dir = scratch("link");
        let real = dir.join("real.yml");
        let link = dir.join("config.yml");
        fs::write(&real, "theme: default\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        set_value(&link, "theme", "dracula").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "theme: dracula\n");
    }

    #[test]
    fn tilde_is_expanded() {
        let cfg = |d: &str| Config {
            download_dir: Some(d.into()),
            ..Config::default()
        };
        let home = dirs::home_dir().unwrap();
        assert_eq!(cfg("~/v").download_dir().unwrap(), home.join("v"));
        assert_eq!(cfg("~").download_dir().unwrap(), home);
        assert_eq!(cfg("/x/y").download_dir().unwrap(), PathBuf::from("/x/y"));
        assert_eq!(
            cfg("~bob/x").download_dir().unwrap(),
            PathBuf::from("~bob/x")
        );
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = scratch("atomic");
        let path = dir.join("a.json");
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
    }
}
