//! Every string the window shows, in English and Traditional Chinese.
//!
//! One struct per language: a string missing from either is a compile error,
//! not a blank button found later.

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
    pub animations: &'static str,
    pub follow_system: &'static str,
    pub on: &'static str,
    pub off: &'static str,
    pub sound: &'static str,
    pub volume: &'static str,
    pub quiet_hours: &'static str,
    pub language: &'static str,
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
    audio_separate: "Sound saved as a separate file",
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
    autostart: "Continue downloads when reelgene starts",
    auto_add: "Queue detected links by themselves",
    close_to_tray: "Keep running in the tray when the window closes",
    animations: "Animations",
    follow_system: "Follow system",
    on: "On",
    off: "Off",
    sound: "Sounds",
    volume: "Volume",
    quiet_hours: "Quiet hours",
    language: "Language",
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
    audio_separate: "聲音另存為獨立檔案",
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
    autostart: "啟動時自動繼續下載",
    auto_add: "偵測到的網址直接加入佇列",
    close_to_tray: "關閉視窗時留在系統匣繼續執行",
    animations: "動畫",
    follow_system: "跟隨系統",
    on: "開",
    off: "關",
    sound: "音效",
    volume: "音量",
    quiet_hours: "勿擾時段",
    language: "語言",
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
