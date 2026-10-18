//! Turning titles from the internet into paths on disk, safely.
//!
//! Every name that reaches the filesystem comes from a website: a series title, an
//! episode title, a URL path. Any of them may contain `..`, path separators,
//! control characters, or a Windows device name. The rules here make sure the
//! result is always a plain file *inside* the download folder, on both Windows
//! and Linux:
//!
//! * separators, `<>:"|?*` and control characters become `_`;
//! * `.` and `..` cannot survive as a component;
//! * Windows device names (`CON`, `NUL`, `COM1`…, with or without an extension)
//!   get a `_` prefix, and trailing dots and spaces are trimmed (Windows drops
//!   them silently, which would make two names collide);
//! * a component is cut to [`MAX_COMPONENT_BYTES`] at a character boundary, so a
//!   long CJK title still fits Linux's 255-byte limit with room for `.part`.

use std::path::{Component, Path, PathBuf};

/// Room for the longest suffix we append (`.mp4.part`, ` (99)`), under 255 bytes.
pub const MAX_COMPONENT_BYTES: usize = 200;

/// Windows MAX_PATH is 260 UTF-16 units; leave room for temp suffixes.
const MAX_PATH_CHARS: usize = 240;

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One safe path component (a file or folder name) from arbitrary text.
pub fn sanitize_component(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let collapsed = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = truncate_bytes(collapsed.trim_matches([' ', '.']), MAX_COMPONENT_BYTES)
        .trim_end_matches([' ', '.'])
        .to_owned();
    if out.is_empty() {
        out.push('_');
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Values a naming template can use.
#[derive(Debug, Clone, Default)]
pub struct NameVars<'a> {
    pub series: &'a str,
    pub title: &'a str,
    /// Episode number as the site counts it (1-based for series).
    pub index: i64,
    /// Number of episodes in the series, for zero-padding by default.
    pub count: usize,
}

/// The default: a folder per series, episodes numbered so they sort.
pub const DEFAULT_TEMPLATE: &str = "{series}/{index} {title}";

/// Render a template like `{series}/{index:02} {title}` into a relative path.
///
/// `/` separates folders; every rendered component is sanitised on its own, so
/// a title can never introduce a folder or climb out of one. `{index}` pads to
/// the width of the episode count (`07` of 80, `007` of 120); `{index:03}`
/// forces a width. Unknown placeholders are kept literally, which shows the
/// mistake in the file name instead of silently dropping text.
pub fn render_template(template: &str, vars: &NameVars) -> PathBuf {
    let auto_width = vars.count.max(1).to_string().len().max(2);
    let mut path = PathBuf::new();
    for raw in template.split(['/', '\\']) {
        let mut out = String::new();
        let mut rest = raw;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let Some(len) = rest[start..].find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let token = &rest[start + 1..start + len];
            let (name, fmt) = token.split_once(':').unwrap_or((token, ""));
            match name {
                "series" => out.push_str(vars.series),
                "title" => out.push_str(vars.title),
                "index" => {
                    let width = fmt.parse::<usize>().unwrap_or(auto_width).min(6);
                    out.push_str(&format!("{:0width$}", vars.index));
                }
                _ => out.push_str(&rest[start..=start + len]),
            }
            rest = &rest[start + len + 1..];
        }
        out.push_str(rest);
        if !out.trim().is_empty() {
            path.push(sanitize_component(&out));
        }
    }
    if path.as_os_str().is_empty() {
        path.push(sanitize_component(vars.title));
    }
    path
}

/// `base/relative.ext`, guaranteed to stay inside `base`, with the last
/// component shortened if the whole path would pass Windows' MAX_PATH.
pub fn output_path(base: &Path, relative: &Path, ext: &str) -> Option<PathBuf> {
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let mut path = base.join(relative);
    let name = path.file_name()?.to_string_lossy().into_owned();
    let fixed = base.to_string_lossy().chars().count()
        + relative.to_string_lossy().chars().count()
        + ext.len()
        + 2;
    let budget = MAX_PATH_CHARS.saturating_sub(fixed.saturating_sub(name.chars().count()));
    let short: String = if name.chars().count() > budget && budget >= 8 {
        name.chars()
            .take(budget)
            .collect::<String>()
            .trim_end_matches([' ', '.'])
            .to_owned()
    } else {
        name
    };
    path.set_file_name(format!("{short}.{ext}"));
    path.starts_with(base).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_traversal_cannot_escape() {
        assert_eq!(sanitize_component("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("."), "_");
        assert_eq!(sanitize_component("a\\b/c"), "a_b_c");
        assert_eq!(sanitize_component("ep:1 <final>?"), "ep_1 _final__");
    }

    #[test]
    fn windows_device_names_and_trailing_dots_are_defused() {
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("nul.mp4"), "_nul.mp4");
        assert_eq!(sanitize_component("com1"), "_com1");
        assert_eq!(
            sanitize_component("CONSOLE"),
            "CONSOLE",
            "only exact device names"
        );
        assert_eq!(sanitize_component("episode. . "), "episode");
    }

    #[test]
    fn control_characters_and_runs_of_space_are_cleaned() {
        assert_eq!(sanitize_component("a\u{0}b\nc\t d"), "a_b_c_ d");
        assert_eq!(sanitize_component("   "), "_");
    }

    #[test]
    fn cjk_titles_survive_and_long_ones_are_cut_on_a_char_boundary() {
        assert_eq!(
            sanitize_component("霸道總裁愛上我 第1集"),
            "霸道總裁愛上我 第1集"
        );
        let long = "長".repeat(200); // 600 bytes
        let cut = sanitize_component(&long);
        assert!(cut.len() <= MAX_COMPONENT_BYTES);
        assert!(
            cut.chars().all(|c| c == '長'),
            "no half character at the end"
        );
    }

    #[test]
    fn template_pads_by_episode_count_and_keeps_folders_from_titles_out() {
        let vars = NameVars {
            series: "大劇 / 第二季",
            title: "第7集: 重逢",
            index: 7,
            count: 80,
        };
        assert_eq!(
            render_template(DEFAULT_TEMPLATE, &vars),
            PathBuf::from("大劇 _ 第二季").join("07 第7集_ 重逢")
        );
        assert_eq!(render_template("{index:03}", &vars), PathBuf::from("007"));
        let many = NameVars {
            count: 120,
            ..vars.clone()
        };
        assert_eq!(render_template("{index}", &many), PathBuf::from("007"));
    }

    #[test]
    fn unknown_placeholders_show_up_instead_of_vanishing() {
        let vars = NameVars {
            series: "s",
            title: "t",
            index: 1,
            count: 1,
        };
        assert_eq!(
            render_template("{title} {oops}", &vars),
            PathBuf::from("t {oops}")
        );
    }

    #[test]
    fn output_path_stays_inside_the_base_and_fits_max_path() {
        let base = Path::new("/downloads");
        let p = output_path(base, &PathBuf::from("劇").join("01 片名"), "mp4").unwrap();
        assert_eq!(p, PathBuf::from("/downloads/劇/01 片名.mp4"));
        assert!(output_path(base, Path::new("../x"), "mp4").is_none());
        assert!(output_path(base, Path::new("/etc/x"), "mp4").is_none());

        let long = PathBuf::from("s").join("x".repeat(300));
        let p = output_path(base, &long, "mp4").unwrap();
        assert!(p.to_string_lossy().chars().count() <= MAX_PATH_CHARS + 4);
        assert!(p.to_string_lossy().ends_with(".mp4"));
    }
}
