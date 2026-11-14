//! Every string the window shows, in English and Traditional Chinese.
//!
//! One struct per language: a string missing from either is a compile error,
//! not a blank button found later.

use reelgene_core::cookies::Summary;
use reelgene_core::engine::{Notice, Status};

pub struct Text {
    pub code: &'static str,
    pub name: &'static str,

    pub input_hint: &'static str,
    pub add: &'static str,
    pub settings: &'static str,
    pub start: &'static str,
    pub stop: &'static str,
    pub clear_done: &'static str,
    pub watch_clipboard: &'static str,

    pub grabbed: fn(usize) -> String,
    pub add_all: &'static str,
    pub dismiss: &'static str,
    pub dismiss_all: &'static str,
    pub private_link: &'static str,
    pub reading: &'static str,

    pub empty_title: &'static str,
    pub empty: &'static str,
    pub watching: &'static str,
    pub job_done: fn(usize, usize) -> String,
    pub job_failed: fn(usize) -> String,
    pub pause: &'static str,
    pub resume: &'static str,
    pub retry: &'static str,
    pub remove: &'static str,
    pub open_folder: &'static str,

    pub queued: &'static str,
    pub retry_in: fn(&str) -> String,
    pub resolving: &'static str,
    pub downloading: &'static str,
    pub assembling: &'static str,
    pub paused: &'static str,
    pub done: &'static str,
    pub failed: &'static str,

    pub url_expired: &'static str,
    pub restarted: &'static str,
    pub not_vod: &'static str,
    pub kept_ts: &'static str,
    pub audio_separate: &'static str,
    pub short_duration: fn(f64, f64) -> String,
    pub left: fn(&str) -> String,

    pub counts: fn(usize, usize, usize, usize) -> String,
    pub no_ffmpeg: &'static str,

    pub pick_title: &'static str,
    pub series: &'static str,
    pub all: &'static str,
    pub all_qualities: &'static str,
    pub paste: &'static str,
    pub paste_button: &'static str,
    pub paste_link: &'static str,
    pub select_all: &'static str,
    pub clear_input: &'static str,
    pub clipboard_empty: &'static str,
    pub none: &'static str,
    pub range_hint: &'static str,
    pub apply: &'static str,
    pub quality: &'static str,
    pub best: &'static str,
    pub worst: &'static str,
    pub folder: &'static str,
    pub saved_as: fn(&str) -> String,
    pub add_n: fn(usize) -> String,
    pub cancel: &'static str,

    pub remove_title: &'static str,
    pub delete_files: &'static str,

    pub added: fn(&str) -> String,
    /// A title and what it was told about its quality.
    pub added_told: fn(&str, &str) -> String,
    /// A quality's height and size (per episode for a series), as much as
    /// is known.
    pub quality_note: fn(Option<u32>, Option<&str>, bool) -> Option<String>,
    pub quality_unknown: &'static str,
    /// The size of the episodes ticked, all together.
    pub episodes_total: fn(&str, usize) -> String,
    pub batch_done: fn(usize, usize) -> String,

    pub naming: &'static str,
    pub section_downloads: &'static str,
    pub section_connections: &'static str,
    pub section_look: &'static str,
    pub section_behaviour: &'static str,
    pub naming_help: &'static str,
    pub parallel: &'static str,
    pub segments: &'static str,
    pub per_host: &'static str,
    pub ffmpeg: &'static str,
    pub autostart: &'static str,
    pub auto_add: &'static str,
    pub close_to_tray: &'static str,
    pub notify: &'static str,
    pub proxy: &'static str,
    pub convert_mp4: &'static str,
    pub ask_quality: &'static str,
    pub qualities_hint: &'static str,
    pub quality_option: fn(u32, f64, Option<&str>) -> String,
    pub quality_total: fn(&str, usize) -> String,
    pub ffmpeg_ready: fn(&str) -> String,
    pub ffmpeg_still_missing: fn(&str) -> String,
    pub ffmpeg_uses: fn(&str, &str) -> String,
    pub ffmpeg_needed: fn(&str) -> String,
    pub ts_kept: &'static str,
    pub get_ffmpeg: &'static str,
    pub section_tools: &'static str,
    pub section_browser: &'static str,
    pub tools_folder: &'static str,
    pub tools_not_absolute: &'static str,
    pub reset_default: &'static str,
    pub ff_import: &'static str,
    pub yt_import: &'static str,
    pub ff_import_help: &'static str,
    pub ffmpeg_is_archive: &'static str,
    pub yt_import_help: &'static str,
    pub yt_source: &'static str,
    pub yt_source_hint: &'static str,
    pub yt_source_help: &'static str,
    pub yt_source_bad: &'static str,
    pub folder_is_a_file: &'static str,
    pub folder_made_on_download: &'static str,
    pub tools_made_on_fetch: &'static str,
    /// The checkbox to take ffmpeg and yt-dlp from the folder in use.
    pub tools_move: fn(&str) -> String,
    pub tools_moved: fn(&str) -> String,
    pub tools_in_download_folder: &'static str,
    pub tools_in_downloads: &'static str,
    pub ff_about: &'static str,
    pub ff_update: &'static str,
    pub ff_fetching: &'static str,
    pub ff_missing: &'static str,
    pub no_ffmpeg_help: &'static str,
    pub check_again: &'static str,
    pub yt_about: &'static str,
    pub yt_missing: &'static str,
    pub yt_official: fn(&str, &str) -> String,
    pub yt_unchecked: fn(&str) -> String,
    pub yt_unofficial_runs: fn(&str) -> String,
    pub yt_looking: &'static str,
    pub yt_fetching: &'static str,
    pub yt_checking: &'static str,
    pub yt_fetch: &'static str,
    pub yt_update: &'static str,
    pub yt_check: &'static str,
    pub yt_fetched: fn(&str) -> String,
    pub yt_checked: fn(&str) -> String,
    pub yt_path_hint: &'static str,
    pub yt_key: &'static str,
    pub yt_key_builtin: &'static str,
    pub yt_key_from_file: &'static str,
    pub yt_key_file: &'static str,
    pub yt_key_hint: &'static str,
    pub yt_key_help: &'static str,
    pub yt_unofficial: &'static str,
    pub yt_unofficial_help: &'static str,
    pub cookies_heading: &'static str,
    pub cookies_about: &'static str,
    pub cookies_none: &'static str,
    /// What was imported, with the date it was.
    pub cookies_have: fn(&Summary, &str) -> String,
    pub cookies_kept: &'static str,
    pub cookies_import: &'static str,
    pub cookies_replace: &'static str,
    pub cookies_remove: &'static str,
    pub cookies_imported: &'static str,
    pub cookies_removed: &'static str,
    pub cookies_how: &'static str,
    pub cookies_steps: &'static str,
    pub browser_about: &'static str,
    pub browser_none: &'static str,
    pub browser_set_up_for: fn(&[&str]) -> String,
    pub browser_out_of_date: &'static str,
    /// A browser that starts another copy of reelgene, and its path.
    pub browser_other: fn(&str, &str) -> String,
    pub browser_set_up: &'static str,
    pub browser_set_up_again: &'static str,
    pub browser_open_folder: &'static str,
    pub browser_remove: &'static str,
    pub browser_how: &'static str,
    pub browser_steps: &'static str,
    pub browser_done: fn(&[&str]) -> String,
    pub browser_removed: &'static str,
    pub speed_limit: &'static str,
    pub no_limit: &'static str,
    pub proxy_hint: &'static str,
    pub proxy_help: &'static str,
    pub proxy_bad: &'static str,
    pub notify_title: &'static str,
    pub notify_body: fn(usize, usize) -> String,
    pub animations: &'static str,
    pub follow_system: &'static str,
    pub on: &'static str,
    pub off: &'static str,
    pub sound: &'static str,
    pub volume: &'static str,
    pub quiet_hours: &'static str,
    pub download_hours: &'static str,
    pub pace: &'static str,
    pub pace_help: &'static str,
    pub held: fn(u8, u8) -> String,
    pub language: &'static str,
    pub theme: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
    pub to_light: &'static str,
    pub to_dark: &'static str,
    pub save: &'static str,
    pub restart_needed: &'static str,
    pub browse: &'static str,
    pub report: &'static str,
    pub report_help: &'static str,
    pub report_saved: fn(&str) -> String,

    pub show_window: &'static str,
    pub quit: &'static str,
    pub cannot_start: &'static str,
    pub ok: &'static str,
}

pub static EN: Text = Text {
    code: "en",
    name: "English",
    input_hint: "Paste a video or playlist link…",
    add: "Add",
    settings: "Settings",
    start: "Start",
    stop: "Stop",
    clear_done: "Clear finished",
    watch_clipboard: "Watch clipboard",
    grabbed: |n| format!("Detected links ({n})"),
    add_all: "Add all",
    dismiss: "Dismiss",
    dismiss_all: "Clear",
    private_link: "local network: not opened by itself",
    reading: "Reading link…",
    empty_title: "Nothing to download yet",
    empty: "Paste a link above, or copy one anywhere: it shows up here.",
    watching: "Watching the clipboard",
    job_done: |done, total| format!("{done}/{total} done"),
    job_failed: |n| format!("{n} failed"),
    pause: "Pause",
    resume: "Resume",
    retry: "Retry",
    remove: "Remove",
    open_folder: "Open folder",
    queued: "Queued",
    retry_in: |t| format!("Trying again in {t}"),
    resolving: "Getting the link…",
    downloading: "Downloading",
    assembling: "Saving…",
    paused: "Paused",
    done: "Done",
    failed: "Failed",
    url_expired: "The link expired; got a fresh one",
    restarted: "The video changed on the server; starting over",
    not_vod: "Live playlist: downloading what is listed now",
    kept_ts: "Kept as .ts (with ffmpeg it becomes .mp4)",
    audio_separate: "Sound saved as a separate file: get ffmpeg (Settings → Tools) to have it joined",
    short_duration: |expected, actual| {
        format!("Plays {actual:.0}s, though the playlist says {expected:.0}s: part may be missing")
    },
    left: |t| format!("{t} left"),
    counts: |d, q, ok, f| {
        let mut s = format!("{d} downloading · {q} queued · {ok} done");
        if f > 0 {
            s.push_str(&format!(" · {f} failed"));
        }
        s
    },
    no_ffmpeg: "ffmpeg not found",
    pick_title: "Choose episodes",
    series: "Title",
    all: "All",
    all_qualities: "All qualities",
    paste: "Paste",
    paste_button: "Paste link",
    paste_link: "Paste the link you copied and read it",
    select_all: "Select all",
    clear_input: "Clear",
    clipboard_empty: "The clipboard holds no text",
    none: "None",
    range_hint: "e.g. 1-10,15",
    apply: "Apply",
    quality: "Quality",
    best: "Best",
    worst: "Smallest",
    folder: "Folder",
    saved_as: |p| format!("Saved as {p}"),
    add_n: |n| {
        if n == 1 {
            "Add 1 episode".into()
        } else {
            format!("Add {n} episodes")
        }
    },
    cancel: "Cancel",
    remove_title: "Remove from the queue?",
    delete_files: "Also delete the finished files",
    added: |t| format!("Added: {t}"),
    added_told: |t, told| format!("Added: {t} ({told})"),
    quality_note: |height, size, series| {
        let each = if series { " an episode" } else { "" };
        match (height, size) {
            (Some(h), Some(s)) => Some(format!("{h}p · about {s}{each}")),
            (Some(h), None) => Some(format!("{h}p")),
            (None, Some(s)) => Some(format!("about {s}{each}")),
            (None, None) => None,
        }
    },
    quality_unknown: "The site does not say",
    episodes_total: |size, n| format!("about {size} for the {n} chosen"),
    batch_done: |done, failed| {
        if failed == 0 {
            format!("All done: {done} finished")
        } else {
            format!("Finished: {done} done, {failed} failed")
        }
    },
    naming: "File names",
    section_downloads: "Downloads",
    section_connections: "Connections",
    section_look: "Look and sounds",
    section_behaviour: "Behaviour",
    naming_help: "{series} {index} {title}; / makes a folder",
    parallel: "Episodes at once",
    segments: "Segments per episode",
    per_host: "Connections per site",
    ffmpeg: "ffmpeg (blank: find it)",
    proxy: "Proxy",
    convert_mp4: "Convert to MP4",
    ask_quality: "Ask which quality when a video comes in several",
    qualities_hint: "Tick one, or several to download each",
    quality_option: |h, mbps, size| match size {
        Some(size) => format!("{h}p · {mbps:.1} Mbps · about {size}"),
        None => format!("{h}p · {mbps:.1} Mbps"),
    },
    quality_total: |size, n| format!("About {size} for the {n} chosen"),
    ffmpeg_ready: |v| format!("ffmpeg {v} found: downloads become MP4"),
    ffmpeg_still_missing: |dir| format!("No ffmpeg in {dir} yet"),
    ffmpeg_uses: |v, path| format!("Uses ffmpeg {v}: {path}"),
    ffmpeg_needed: |exe| {
        // Windows users have no package to install, and ffmpeg's GitHub
        // page offers only source: say which build to take.
        let own = if cfg!(windows) {
            format!(
                " (ffmpeg's GitHub page and its zip are source code with no {exe}: take a build, such as BtbN's win64 zip without \"-shared\" in its name, and copy only bin\\{exe})"
            )
        } else {
            String::new()
        };
        format!(
            "MP4 needs ffmpeg, and so does joining separate sound; reelgene does not include it. Get ffmpeg fetches it into this folder; or put your own {exe} there and press Check again{own}. Until then downloads stay .ts and sound is saved separately:"
        )
    },
    ts_kept: "Downloads stay as they came (.ts).",
    get_ffmpeg: "Get ffmpeg",
    section_tools: "Tools",
    section_browser: "Browser",
    tools_folder: "Tools folder",
    tools_not_absolute: "Use a full path",
    reset_default: "Back to the default",
    ff_import: "Import ffmpeg…",
    yt_import: "Import yt-dlp…",
    ff_import_help: "An ffmpeg you downloaded: a build's .zip or .tar.xz (only its bin/ffmpeg is taken), or the program itself. Not a \"shared\" build.",
    ffmpeg_is_archive: "This is an archive, not the program: clear this field and use Import ffmpeg… above, which takes ffmpeg out of a build's zip",
    yt_import_help: "A yt-dlp you downloaded. It is checked like one reelgene fetches: it must be the latest official build for this system.",
    yt_source: "Download source",
    yt_source_hint: "Blank: yt-dlp's own releases on GitHub",
    yt_source_help: "A mirror of yt-dlp's releases: the folder address holding SHA2-256SUMS and the release files. Safe to use, as what comes from it must still carry yt-dlp's signature.",
    yt_source_bad: "Not a folder's web address (http or https)",
    folder_is_a_file: "This is a file, not a folder",
    folder_made_on_download: "The folder does not exist yet; it is made with the first download.",
    tools_made_on_fetch: "The folder does not exist yet; it is made when a tool is fetched or moved there.",
    tools_move: |from| format!("Move ffmpeg and yt-dlp here from {from}"),
    tools_moved: |to| format!("ffmpeg and yt-dlp moved to {to}"),
    tools_in_download_folder: "Not inside the download folder: downloaded files could land beside the programs",
    tools_in_downloads: "Not inside your Downloads folder: downloaded files could land beside the programs",
    ff_about: "ffmpeg turns .ts into .mp4 and joins separate sound to the picture. Its developers publish no builds; Get ffmpeg fetches BtbN's release build from GitHub (about 150 MB) and checks it against the checksum published with it. That proves the download is intact, not who made it; here ffmpeg only ever copies local files.",
    ff_update: "Update ffmpeg",
    ff_fetching: "Fetching ffmpeg (about 150 MB) and checking it…",
    ff_missing: "ffmpeg is not installed: downloads stay .ts and separate sound is saved beside the video.",
    no_ffmpeg_help: "Get ffmpeg: it joins separate sound to the picture and makes MP4s",
    check_again: "Check again",
    yt_about: "For pages no resolver handles, reelgene asks yt-dlp where the video is, then downloads it itself. Only the official build runs: its checksum must be in yt-dlp's latest release, signed with yt-dlp's key.",
    yt_missing: "yt-dlp is not installed.",
    yt_official: |date, file| format!("Official yt-dlp {date} ({file}), signature checked"),
    yt_unchecked: |path| {
        format!("{path}: checked against yt-dlp's signed release before it first runs")
    },
    yt_unofficial_runs: |path| format!("{path}: runs without a check (unofficial builds allowed)"),
    yt_looking: "Looking for yt-dlp…",
    yt_fetching: "Fetching yt-dlp and checking its signature…",
    yt_checking: "Checking yt-dlp against its signed release…",
    yt_fetch: "Get yt-dlp",
    yt_update: "Update yt-dlp",
    yt_check: "Check now",
    yt_fetched: |date| format!("yt-dlp {date} installed, signature checked"),
    yt_checked: |date| format!("yt-dlp {date}: official, signature checked"),
    yt_path_hint: "Blank: the one reelgene fetches",
    yt_key: "Signing key",
    yt_key_builtin: "built in",
    yt_key_from_file: "from the key file",
    yt_key_file: "Key file",
    yt_key_hint: "Blank: the built-in key",
    yt_key_help: "Only if yt-dlp changes its key: use the public.key from yt-dlp's own repository, and compare its fingerprint with the one yt-dlp announces. A key from anyone else lets their builds through.",
    yt_unofficial: "Run unofficial builds without checking them",
    yt_unofficial_help: "For a yt-dlp from pip or a distribution package. It then runs unchecked, so a changed copy would run too.",
    cookies_heading: "Cookies for signing in",
    cookies_about: "For videos a site shows only to someone signed in, such as age-restricted or members-only ones. yt-dlp gets the cookies only after it says a video needs signing in, for that one run; they go nowhere else.",
    cookies_none: "No cookies imported.",
    cookies_have: |s, date| {
        let mut sites = s
            .sites
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if s.sites.len() > 4 {
            sites.push_str(&format!(" +{} more", s.sites.len() - 4));
        }
        let expired = if s.expired > 0 {
            format!(", {} expired", s.expired)
        } else {
            String::new()
        };
        format!(
            "{} cookies for {sites} (imported {date}{expired})",
            s.cookies
        )
    },
    cookies_kept: if cfg!(windows) {
        "Kept encrypted for your Windows account: another account or computer cannot read them."
    } else {
        "Kept in a file only your account can read."
    },
    cookies_import: "Import cookies.txt…",
    cookies_replace: "Import again…",
    cookies_remove: "Remove cookies",
    cookies_imported: "Cookies imported. Delete the file you exported now: reelgene keeps its own copy.",
    cookies_removed: "Cookies removed",
    cookies_how: "How to export them safely",
    cookies_steps: "1. Make a separate account for this, not your main one (for age-restricted videos, an adult one). Downloading with cookies can break a site's terms, and the account may be locked.\n2. Open a private window and sign in with that account. Allow your cookie export add-on in private windows.\n3. Export that site's cookies as cookies.txt (the Netscape format).\n4. Close the private window at once and do not browse with it again: sites renew cookies as you browse, and the exported ones would stop working.\n5. Import the file here, then delete it.\n\nWhen videos ask to sign in again, export and import anew. Remove the cookies when you no longer need them.",
    browser_about: "The reelgene extension finds the videos a page plays (HLS playlists and video files) and sends the one you pick here, with the page's title and the Referer its server checks. Cookies and passwords stay in the browser. For Chrome, Edge, Brave, Vivaldi and Chromium.",
    browser_none: "Not set up.",
    browser_set_up_for: |names| format!("Set up for {}.", names.join(", ")),
    browser_out_of_date: "The extension's folder is from another version: set up again.",
    browser_other: |name, path| format!("{name} starts another reelgene: {path}"),
    browser_set_up: "Set up",
    browser_set_up_again: "Set up again",
    browser_open_folder: "Open extension folder",
    browser_remove: "Remove",
    browser_how: "Loading it in the browser",
    browser_steps: "Once, after setting up:\n1. Open the browser's Extensions page (chrome://extensions, edge://extensions).\n2. Turn on Developer mode.\n3. Choose “Load unpacked” and pick the extension folder above.\n\nThen play a video and click reelgene's button in the toolbar: the number on it is how many videos it found. Links sent while reelgene is closed wait in its queue.",
    browser_done: |names| {
        let names = names.join(", ");
        format!("reelgene answers {names} now. Load the extension there once (see the steps).")
    },
    browser_removed: "The browsers no longer start reelgene. Remove the extension on their Extensions page too.",
    speed_limit: "Speed limit",
    no_limit: "No limit",
    proxy_hint: "system · none · socks5h://host:port",
    proxy_help: "Blank uses the system's proxy; \"none\" goes direct; or an http://, https://, socks5:// or socks5h:// address (socks5h lets the proxy look up names). Takes effect after a restart.",
    proxy_bad: "Not a proxy address: http://, https://, socks5:// or socks5h://",
    autostart: "Continue downloads when reelgene starts",
    auto_add: "Queue detected links by themselves",
    close_to_tray: "Keep running in the tray when the window closes",
    notify: "Notify me when downloads finish and the window is in the background",
    notify_title: "Downloads finished",
    notify_body: |done, failed| match (done, failed) {
        (d, 0) => format!("{d} done"),
        (0, f) => format!("{f} failed"),
        (d, f) => format!("{d} done, {f} failed"),
    },
    animations: "Animations",
    follow_system: "Follow system",
    on: "On",
    off: "Off",
    sound: "Sounds",
    volume: "Volume",
    quiet_hours: "Quiet hours",
    download_hours: "Download hours",
    pace: "Pause between items",
    pace_help: "A random pause in seconds before each next item from the same site, with one item from a site at a time: gentler on the site, and less likely to trip its rate limits.",
    held: |from, to| format!("Waiting for download hours ({from}:00–{to}:00)"),
    language: "Language",
    theme: "Theme",
    light: "Light",
    dark: "Dark",
    to_light: "Switch to the light theme",
    to_dark: "Switch to the dark theme",
    save: "Save",
    restart_needed: "Some changes apply after reelgene restarts",
    browse: "Browse…",
    report: "Export a problem report",
    report_help: "Settings, the queue and the recent log in one text file; no link secrets, no home folder name",
    report_saved: |p| format!("Problem report saved: {p}"),
    show_window: "Show reelgene",
    quit: "Quit",
    cannot_start: "reelgene cannot start",
    ok: "OK",
};

pub static ZH_TW: Text = Text {
    code: "zh-TW",
    name: "繁體中文",
    input_hint: "貼上影片或播放清單網址…",
    add: "加入",
    settings: "設定",
    start: "開始",
    stop: "停止",
    clear_done: "清除已完成",
    watch_clipboard: "監看剪貼簿",
    grabbed: |n| format!("偵測到的網址（{n}）"),
    add_all: "全部加入",
    dismiss: "略過",
    dismiss_all: "清除",
    private_link: "區域網路位址，不會自動連線",
    reading: "讀取網址中…",
    empty_title: "還沒有任何下載",
    empty: "在上方貼上網址，或在任何地方複製網址，就會出現在這裡。",
    watching: "正在監看剪貼簿",
    job_done: |done, total| format!("完成 {done}/{total}"),
    job_failed: |n| format!("失敗 {n}"),
    pause: "暫停",
    resume: "繼續",
    retry: "重試",
    remove: "移除",
    open_folder: "開啟資料夾",
    queued: "排隊中",
    retry_in: |t| format!("{t}後重試"),
    resolving: "取得網址中…",
    downloading: "下載中",
    assembling: "存檔中…",
    paused: "已暫停",
    done: "完成",
    failed: "失敗",
    url_expired: "網址已過期，已重新取得",
    restarted: "伺服器上的影片已變更，重新下載",
    not_vod: "直播或未完成的清單：只下載目前列出的部分",
    kept_ts: "保留為 .ts（有 ffmpeg 可轉成 .mp4）",
    audio_separate: "聲音另存為獨立檔案：到「設定 → 工具」取得 ffmpeg 就會自動合併",
    short_duration: |expected, actual| {
        format!("實際只有 {actual:.0} 秒，清單寫 {expected:.0} 秒：可能缺了一段")
    },
    left: |t| format!("剩 {t}"),
    counts: |d, q, ok, f| {
        let mut s = format!("下載中 {d} · 排隊 {q} · 完成 {ok}");
        if f > 0 {
            s.push_str(&format!(" · 失敗 {f}"));
        }
        s
    },
    no_ffmpeg: "找不到 ffmpeg",
    pick_title: "選擇集數",
    series: "劇名",
    all: "全選",
    all_qualities: "全部畫質",
    paste: "貼上",
    paste_button: "貼上網址",
    paste_link: "貼上剛複製的網址並開始讀取",
    select_all: "全選",
    clear_input: "清除",
    clipboard_empty: "剪貼簿裡沒有文字",
    none: "全不選",
    range_hint: "例如 1-10,15",
    apply: "套用",
    quality: "畫質",
    best: "最佳",
    worst: "最小",
    folder: "資料夾",
    saved_as: |p| format!("存成 {p}"),
    add_n: |n| format!("加入 {n} 集"),
    cancel: "取消",
    remove_title: "要從佇列移除嗎？",
    delete_files: "一併刪除已完成的檔案",
    added: |t| format!("已加入：{t}"),
    added_told: |t, told| format!("已加入：{t}（{told}）"),
    quality_note: |height, size, series| {
        let each = if series { "每集" } else { "" };
        match (height, size) {
            (Some(h), Some(s)) => Some(format!("{h}p · {each}約 {s}")),
            (Some(h), None) => Some(format!("{h}p")),
            (None, Some(s)) => Some(format!("{each}約 {s}")),
            (None, None) => None,
        }
    },
    quality_unknown: "網站沒有提供畫質資訊",
    episodes_total: |size, n| format!("所選 {n} 集共約 {size}"),
    batch_done: |done, failed| {
        if failed == 0 {
            format!("全部完成：{done} 集")
        } else {
            format!("下載結束：完成 {done} 集，失敗 {failed} 集")
        }
    },
    naming: "檔名格式",
    section_downloads: "下載",
    section_connections: "連線",
    section_look: "外觀與音效",
    section_behaviour: "行為",
    naming_help: "{series} 劇名、{index} 集數、{title} 標題；/ 建立資料夾",
    parallel: "同時下載集數",
    segments: "每集同時下載片段",
    per_host: "每個網站連線數",
    ffmpeg: "ffmpeg（留空自動尋找）",
    proxy: "代理伺服器",
    convert_mp4: "轉成 MP4",
    ask_quality: "影片有多種畫質時，詢問要下載哪些",
    qualities_hint: "勾一個，或勾多個各下載一份",
    quality_option: |h, mbps, size| match size {
        Some(size) => format!("{h}p · {mbps:.1} Mbps · 約 {size}"),
        None => format!("{h}p · {mbps:.1} Mbps"),
    },
    quality_total: |size, n| format!("所選 {n} 項共約 {size}"),
    ffmpeg_ready: |v| format!("已找到 ffmpeg {v}：下載後會轉成 MP4"),
    ffmpeg_still_missing: |dir| format!("{dir} 裡還沒有 ffmpeg"),
    ffmpeg_uses: |v, path| format!("使用 ffmpeg {v}：{path}"),
    ffmpeg_needed: |exe| {
        let own = if cfg!(windows) {
            format!(
                "（ffmpeg 在 GitHub 上的專案和它的 zip 都是原始碼，裡面沒有 {exe}：要下載編譯好的版本，例如 BtbN 檔名有 win64、沒有「-shared」的 zip，只複製 bin\\{exe}）"
            )
        } else {
            String::new()
        };
        format!(
            "轉成 MP4、合併聲音都需要 ffmpeg，reelgene 沒有內附。按「取得 ffmpeg」會下載到這個資料夾；或把你自己的 {exe} 放進去，再按「重新偵測」{own}。在那之前，下載會維持 .ts，聲音另存："
        )
    },
    ts_kept: "下載後維持原本格式（.ts）。",
    get_ffmpeg: "取得 ffmpeg",
    section_tools: "工具",
    section_browser: "瀏覽器",
    tools_folder: "工具資料夾",
    tools_not_absolute: "請輸入完整路徑",
    reset_default: "恢復預設",
    ff_import: "匯入 ffmpeg 檔案…",
    yt_import: "匯入 yt-dlp 檔案…",
    ff_import_help: "你自己下載的 ffmpeg：編譯版的 .zip 或 .tar.xz（只取出其中的 bin/ffmpeg），或程式本身。不能是「shared」版。",
    ffmpeg_is_archive: "這是壓縮檔，不是程式：請清空這一欄，改用上面的「匯入 ffmpeg 檔案…」，它會從編譯版的 zip 取出 ffmpeg",
    yt_import_help: "你自己下載的 yt-dlp。會和 reelgene 自己下載的一樣驗證：必須是這個系統用的最新官方版本。",
    yt_source: "下載來源",
    yt_source_hint: "留空：yt-dlp 在 GitHub 的官方發行",
    yt_source_help: "yt-dlp 發行檔的鏡像站：放有 SHA2-256SUMS 和發行檔的資料夾網址。可以放心使用：從那裡下載的東西，一樣必須有 yt-dlp 的簽章。",
    yt_source_bad: "這不是資料夾的網址（要 http 或 https）",
    folder_is_a_file: "這是檔案，不是資料夾",
    folder_made_on_download: "資料夾還不存在，第一次下載時會自動建立。",
    tools_made_on_fetch: "資料夾還不存在，下載或搬移工具時會自動建立。",
    tools_move: |from| format!("把 ffmpeg 和 yt-dlp 從 {from} 一起搬過來"),
    tools_moved: |to| format!("已把 ffmpeg 和 yt-dlp 搬到 {to}"),
    tools_in_download_folder: "不能放在下載資料夾裡：下載的檔案可能落在程式旁邊",
    tools_in_downloads: "不能放在系統的「下載」資料夾裡：下載的檔案可能落在程式旁邊",
    ff_about: "ffmpeg 負責把 .ts 轉成 .mp4，並把分開的聲音合併回影片。ffmpeg 官方不提供執行檔；「取得 ffmpeg」會從 GitHub 下載 BtbN 打包的穩定版（約 150 MB），並比對隨附的雜湊值。這只能證明下載完整，無法證明是誰做的；不過 ffmpeg 在這裡只處理本機檔案。",
    ff_update: "更新 ffmpeg",
    ff_fetching: "正在下載 ffmpeg（約 150 MB）並檢查…",
    ff_missing: "尚未安裝 ffmpeg：下載會維持 .ts，聲音另存在影片旁邊。",
    no_ffmpeg_help: "取得 ffmpeg：用來合併聲音、轉成 MP4",
    check_again: "重新偵測",
    yt_about: "遇到沒有專屬解析器的網頁時，reelgene 會問 yt-dlp 影片在哪裡，再由 reelgene 自己下載。只會執行官方版本：它的雜湊值必須列在 yt-dlp 最新版的清單裡，而且清單要有 yt-dlp 金鑰的簽章。",
    yt_missing: "尚未安裝 yt-dlp。",
    yt_official: |date, file| format!("官方 yt-dlp {date}（{file}），簽章已驗證"),
    yt_unchecked: |path| format!("{path}：第一次執行前會先比對 yt-dlp 簽署過的官方版本"),
    yt_unofficial_runs: |path| format!("{path}：不經驗證直接執行（已允許非官方版本）"),
    yt_looking: "正在尋找 yt-dlp…",
    yt_fetching: "正在下載 yt-dlp 並驗證簽章…",
    yt_checking: "正在比對 yt-dlp 簽署過的官方版本…",
    yt_fetch: "取得 yt-dlp",
    yt_update: "更新 yt-dlp",
    yt_check: "立即驗證",
    yt_fetched: |date| format!("已安裝 yt-dlp {date}，簽章已驗證"),
    yt_checked: |date| format!("yt-dlp {date}：官方版本，簽章已驗證"),
    yt_path_hint: "留空：使用 reelgene 取得的版本",
    yt_key: "簽章金鑰",
    yt_key_builtin: "內建",
    yt_key_from_file: "來自金鑰檔",
    yt_key_file: "金鑰檔",
    yt_key_hint: "留空：使用內建金鑰",
    yt_key_help: "只有在 yt-dlp 更換金鑰時才需要：請使用 yt-dlp 官方 repo 裡的 public.key，並和 yt-dlp 公布的指紋核對。用了其他人給的金鑰，等於放行他們做的版本。",
    yt_unofficial: "不驗證，直接執行非官方版本",
    yt_unofficial_help: "給用 pip 或系統套件安裝的 yt-dlp 用。這種 yt-dlp 不會被驗證，被竄改過的版本也會照樣執行。",
    cookies_heading: "登入用的 Cookie",
    cookies_about: "用在網站只給登入者看的影片，例如年齡限制或會員限定的影片。只有 yt-dlp 回報影片需要登入時，cookie 才會交給它，而且只用那一次，不會送到其他地方。",
    cookies_none: "尚未匯入 cookie。",
    cookies_have: |s, date| {
        let mut sites = s
            .sites
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        if s.sites.len() > 4 {
            sites.push_str(&format!(" 等 {} 個網站", s.sites.len()));
        }
        let expired = if s.expired > 0 {
            format!("，{} 個已過期", s.expired)
        } else {
            String::new()
        };
        format!("{sites} 的 {} 個 cookie（{date} 匯入{expired}）", s.cookies)
    },
    cookies_kept: if cfg!(windows) {
        "已用你的 Windows 帳號加密保存：其他帳號或其他電腦都讀不到。"
    } else {
        "保存在只有你的帳號能讀取的檔案裡。"
    },
    cookies_import: "匯入 cookies.txt…",
    cookies_replace: "重新匯入…",
    cookies_remove: "移除 cookie",
    cookies_imported: "已匯入 cookie。請刪除你匯出的檔案：reelgene 已保存自己的一份。",
    cookies_removed: "已移除 cookie",
    cookies_how: "如何安全地匯出",
    cookies_steps: "1. 另外開一個帳號專門給這裡用，不要用主帳號（年齡限制的影片要用已成年的帳號）。用 cookie 下載可能違反網站的使用條款，帳號有被鎖的風險。\n2. 開一個無痕視窗，登入那個帳號。匯出 cookie 的擴充套件要允許在無痕視窗執行。\n3. 把那個網站的 cookie 匯出成 cookies.txt（Netscape 格式）。\n4. 匯出後馬上關掉無痕視窗，不要再用它瀏覽：網站會在你瀏覽時更換 cookie，剛匯出的就會失效。\n5. 在這裡匯入那個檔案，然後把它刪掉。\n\n影片又開始要求登入時，照同樣步驟重新匯出、再匯入。不再需要時，按「移除 cookie」。",
    browser_about: "reelgene 擴充功能會找出網頁正在播放的影片（HLS 播放清單和影片檔），把你選的那個連同網頁標題、伺服器會檢查的 Referer 一起送來這裡。Cookie 和密碼都留在瀏覽器裡。支援 Chrome、Edge、Brave、Vivaldi 和 Chromium。",
    browser_none: "尚未設定。",
    browser_set_up_for: |names| format!("已為 {} 設定。", names.join("、")),
    browser_out_of_date: "擴充功能資料夾是其他版本的：請重新設定。",
    browser_other: |name, path| format!("{name} 會啟動另一個 reelgene：{path}"),
    browser_set_up: "設定",
    browser_set_up_again: "重新設定",
    browser_open_folder: "開啟擴充功能資料夾",
    browser_remove: "移除",
    browser_how: "在瀏覽器載入",
    browser_steps: "設定後做一次就好：\n1. 打開瀏覽器的擴充功能頁面（chrome://extensions、edge://extensions）。\n2. 開啟「開發人員模式」。\n3. 選「載入未封裝項目」，挑上面那個擴充功能資料夾。\n\n之後播放影片，再按工具列上的 reelgene 按鈕：上面的數字是找到幾個影片。reelgene 沒開時送出的連結，會在佇列裡等它開啟。",
    browser_done: |names| {
        let names = names.join("、");
        format!("reelgene 現在會回應 {names}。請在瀏覽器載入擴充功能一次（見步驟）。")
    },
    browser_removed: "瀏覽器不會再啟動 reelgene。也請到瀏覽器的擴充功能頁面把它移除。",
    speed_limit: "限速",
    no_limit: "不限速",
    proxy_hint: "留空用系統 · none · socks5h://主機:埠",
    proxy_help: "留空：使用系統的代理設定；none：直接連線；或填 http://、https://、socks5://、socks5h:// 位址（socks5h 由代理伺服器查網域）。重新啟動後生效。",
    proxy_bad: "不是代理伺服器位址：要用 http://、https://、socks5:// 或 socks5h://",
    autostart: "啟動時自動繼續下載",
    auto_add: "偵測到的網址直接加入佇列",
    close_to_tray: "關閉視窗時留在系統匣繼續執行",
    notify: "視窗不在前景時，下載結束就發出系統通知",
    notify_title: "下載結束",
    notify_body: |done, failed| match (done, failed) {
        (d, 0) => format!("完成 {d} 個"),
        (0, f) => format!("失敗 {f} 個"),
        (d, f) => format!("完成 {d} 個，失敗 {f} 個"),
    },
    animations: "動畫",
    follow_system: "跟隨系統",
    on: "開",
    off: "關",
    sound: "音效",
    volume: "音量",
    quiet_hours: "勿擾時段",
    download_hours: "下載時段",
    pace: "項目之間暫停",
    pace_help: "同一個網站的下一項開始前，隨機暫停幾秒，而且同一網站一次只下載一項：減輕網站負擔，也比較不會觸發它的流量限制。",
    held: |from, to| format!("等待下載時段（{from}:00–{to}:00）"),
    language: "語言",
    theme: "主題",
    light: "淺色",
    dark: "深色",
    to_light: "切換為淺色主題",
    to_dark: "切換為深色主題",
    save: "儲存",
    restart_needed: "部分設定重新啟動 reelgene 後生效",
    browse: "選擇…",
    report: "匯出問題回報",
    report_help: "把設定、佇列和最近的紀錄存成一個文字檔；不含網址裡的密鑰，也不含家目錄名稱",
    report_saved: |p| format!("問題回報已存到：{p}"),
    show_window: "顯示 reelgene",
    quit: "結束",
    cannot_start: "reelgene 無法啟動",
    ok: "確定",
};

pub static ALL: [&Text; 2] = [&EN, &ZH_TW];

/// The configured language, else the system's, else English.
pub fn pick(setting: Option<&str>) -> &'static Text {
    let wanted = setting
        .map(str::to_owned)
        .or_else(reelgene_app::platform::system_language);
    match wanted {
        Some(code) if code.to_ascii_lowercase().starts_with("zh") => &ZH_TW,
        _ => &EN,
    }
}

impl Text {
    pub fn status(&self, status: &Status) -> String {
        match status {
            Status::Queued => self.queued.into(),
            Status::Waiting { retry_in, .. } => {
                (self.retry_in)(&reelgene_app::format::duration(*retry_in))
            }
            Status::Resolving => self.resolving.into(),
            Status::Downloading => self.downloading.into(),
            Status::Assembling => self.assembling.into(),
            Status::Paused => self.paused.into(),
            Status::Done { .. } => self.done.into(),
            Status::Failed { error } => format!("{}: {error}", self.failed),
        }
    }

    pub fn notice(&self, notice: &Notice) -> String {
        match notice {
            Notice::UrlExpired { .. } => self.url_expired.into(),
            Notice::Restarted => self.restarted.into(),
            Notice::NotVod => self.not_vod.into(),
            Notice::KeptTs { .. } => self.kept_ts.into(),
            Notice::AudioSeparate { .. } => self.audio_separate.into(),
            Notice::ShortDuration { expected, actual } => (self.short_duration)(*expected, *actual),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_are_complete_and_distinct() {
        for t in ALL {
            assert!(!t.input_hint.is_empty() && !t.ok.is_empty());
            assert!((t.counts)(1, 2, 3, 0).contains('3'));
            assert!((t.add_n)(5).contains('5'));
        }
        assert_ne!(EN.add, ZH_TW.add);
    }

    #[test]
    fn a_configured_language_wins_and_any_chinese_locale_gets_chinese() {
        assert_eq!(pick(Some("zh-TW")).code, "zh-TW");
        assert_eq!(pick(Some("zh_CN.UTF-8")).code, "zh-TW");
        assert_eq!(pick(Some("en")).code, "en");
        assert_eq!(pick(Some("fr")).code, "en", "no French yet: English");
    }

    #[test]
    fn failed_status_carries_its_reason() {
        let s = EN.status(&Status::Failed {
            error: "HTTP 404".into(),
        });
        assert_eq!(s, "Failed: HTTP 404");
        assert_eq!(ZH_TW.status(&Status::Paused), "已暫停");
    }
}
