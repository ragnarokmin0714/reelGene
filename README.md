# reelgene

A portable video downloader for Windows and Linux. Copy a link and it shows
up; pick episodes; the downloads survive closing the program, losing the
network or leaving it overnight, and pick up where they stopped.

> **Status: v0.1 preview.** Everything below is built and tested — 150-odd
> automated tests, including the window driven headless — but it has not yet
> been used on a real Windows machine or looked at on a real screen. Treat it
> as a preview until a release says otherwise. What comes next is in
> [`ROADMAP.md`](ROADMAP.md).

## What it does

- **Watches the clipboard** (on by default, one switch to turn off). Copy a
  link anywhere and it is listed, ready to add — or added by itself, if you
  prefer. Ordinary web pages are ignored; only links reelgene can download
  appear.
- **HLS playlists and direct video links**: `.m3u8` (AES-128 encrypted
  included, fMP4 or MPEG-TS, separate audio tracks) and plain files such as
  `.mp4`.
- **Series in one go**: when a link holds many episodes, tick the ones you
  want — all, none, or a range like `1-10,15` — and they queue together.
- **Resumes everything**: a playlist resumes segment by segment, a file byte
  by byte. Signed links that expire are fetched again by themselves. If the
  video changed on the server in the meantime, the episode starts over
  instead of splicing two versions into a broken file.
- **A queue you control**: pause, resume, retry, remove; how many episodes
  at once; how many connections per site (kept low by default, which also
  keeps accounts from looking like abuse). Rate limits from servers are
  respected, and a temporary failure retries by itself later.
- **Checks what it saves**: a server that answers with an error page
  instead of video is caught (after trying fresh links) rather than saved as
  a broken file; a file that plays far shorter than its playlist says is
  flagged.
- **File names you choose**: `{series}/{index} {title}` by default, so
  episodes sort and each series gets a folder.
- **A window and a command line**, the same engine underneath. The window
  has a tray icon, English and Traditional Chinese, optional animations that
  follow the system's reduce-motion setting, and two optional sounds — one
  when the whole batch is done, one when something fails — with quiet hours.
- **Portable**: one executable, no installer.

## What it will not do

- **DRM**: content protected by Widevine, PlayReady or FairPlay is detected
  and refused with that name. reelgene does not break protection.
- **Watermarks**: reelgene never removes watermarks, logos or other marks
  that identify the rights holder.
- Many sites' terms forbid downloading, and downloading a lot with a logged-in
  account can get it suspended. reelgene is a tool for personal use; what you
  download is your responsibility.

## Getting it

Download from the releases page:

| System | File |
|---|---|
| Windows 10/11 (x86-64) | `reelgene.exe` |
| Linux x86-64 / ARM64 | `reelgene-x86_64-linux`, `reelgene-aarch64-linux` |

Each release lists SHA-256 checksums. The Linux builds run on any
distribution with glibc 2.31 or newer (Ubuntu 20.04+, Debian 11+, RHEL and
Rocky 9+); servers without a desktop can still use the command line.

**Portable mode**: put an empty file named `reelgene.portable` next to the
executable, and settings, the queue and downloads all live beside it.

**Windows**: the executable is not code-signed yet, so SmartScreen may warn
the first time. A program that watches the clipboard and downloads files can
also look suspicious to antivirus software; the source is here to check.

**Linux**: the tray icon needs a desktop that shows StatusNotifierItem icons
— KDE, Xfce and most others do; GNOME needs the AppIndicator extension.
Without a tray, closing the window quits. Sounds play through `pw-play`,
`paplay` or `aplay`, whichever is installed.

## The window

Run `reelgene` with no arguments. Paste a link in the box at the top (or just
copy one anywhere), and it is read and queued; a series opens the episode
picker first. **Start** and **Stop** run or hold the whole queue; each series
and episode has its own pause, resume, retry, remove and open-folder buttons.
**Settings** covers the download folder, file names, quality, how much runs
at once, ffmpeg, the clipboard, sounds, animations and language, and can
export a problem report. The window remembers its size and place. Starting
reelgene again while it runs brings the window back instead of opening a
second one. When a batch ends while the window is closed or in the
background, a system notification says how it went (Settings can turn it
off).

## The command line

```sh
reelgene get https://example.com/show/index.m3u8          # download now
reelgene get URL --episodes 1-5,8 --quality 720 -o ~/Videos
reelgene add URL -H "Referer: https://example.com/"       # queue for later
reelgene run [--limit 2M]                                  # work through the queue
reelgene list                                              # what is queued, done, failed
reelgene resume [JOB]                                      # queue failed or paused again
reelgene pause [JOB]
reelgene rm JOB [--delete-files]
reelgene doctor                                            # where things are, ffmpeg, resolvers
reelgene report                                            # a problem report, in this folder
reelgene licenses                                          # reelgene's and its fonts' licences
```

`Ctrl-C` stops cleanly; `reelgene run` continues later. The exit code is 0
when everything finished, 1 when something failed. Only one reelgene runs per
data folder. While one runs (the window, or `reelgene get` / `run`),
`reelgene get URL` and `reelgene add URL` pass their links to it and return;
options such as `-o` cannot be passed that way, and other commands wait for
it to close (`doctor` always works).

Headers given with `-H` are saved with the job, because a resume needs them
too; do not pass anything you would not keep on disk.

On Windows the release build is a GUI program; run from a terminal, it
attaches to it for output, but the prompt may return before the output ends.

## ffmpeg (optional)

reelgene downloads and joins everything itself. With
[ffmpeg](https://ffmpeg.org) available it also turns `.ts` into `.mp4`
(copying the streams, not re-encoding: seconds, no quality loss) and merges
separate audio into the video. Without it you get `.ts`, and separate audio
is saved next to the video.

reelgene looks for ffmpeg in the path set in Settings, then next to a
portable executable, then on `PATH` (never in the current folder). ffmpeg is
only ever given local files: reelgene does all the networking itself.

## Where files live

| | Settings | Queue | Downloads |
|---|---|---|---|
| Portable | `data/` beside the executable | `data/` | `downloads/` |
| Windows | `%APPDATA%\reelgene` | `%LOCALAPPDATA%\reelgene` | `Downloads\reelgene` |
| Linux | `~/.config/reelgene` | `~/.local/share/reelgene` | your Downloads folder `/reelgene` |

`REELGENE_HOME` puts everything in one folder instead. `settings.json` is
written on first run with every option spelled out. A log file a day goes in
`logs/` next to the queue and is kept for two weeks.

When something goes wrong, **Settings → Export a problem report** (or
`reelgene report`) writes one text file with the settings, the queue and the
recent log. Links in it have their query strings removed and your home
folder appears as `~`; read it before sending it anyway.

## Privacy and security

- **Clipboard**: only well-formed http(s) links are taken from it; the rest
  of what you copy is never stored, logged or sent anywhere.
- **Local network**: links to your own machine or network (a router's admin
  page, `localhost`) are listed but never opened without a click, and a link
  from the internet may not lead reelgene into your network, by redirect or
  by playlist.
- **Signed links stay private**: anything reelgene shows or logs has the
  query string removed, where such links carry their credentials.
- **Files**: names from websites are cleaned so nothing can escape the
  download folder or collide with Windows device names; a file download keeps
  only media extensions, so no `.exe` can land in your downloads. On Windows,
  downloaded files are marked as coming from the internet, as browsers do.
- **Handing links over** between two reelgenes uses a private channel only
  your account can open (a Unix socket in a folder of yours; a named pipe
  closed to the network on Windows), never a network port that a web page
  could reach.
- **TLS** certificate checks cannot be turned off. No code is loaded at run
  time: everything is compiled in.

## Building

Rust 1.95 or newer:

```sh
cargo build --release          # target/release/reelgene
cargo test --workspace
cargo build --release -p reelgene --no-default-features   # command line only, no window
```

Linux release builds use [`cargo zigbuild`](https://github.com/rust-cross/cargo-zigbuild)
to target glibc 2.31. The design rules and the reasons behind them are in
[`CLAUDE.md`](CLAUDE.md).

Support for particular websites (listing a series' episodes from its page)
is not part of this repository; `resolver/extra.rs` is where a build adds its
own.

## License

MIT or Apache-2.0, at your option: [`LICENSE-MIT`](LICENSE-MIT),
[`LICENSE-APACHE`](LICENSE-APACHE).

The window uses the [Inter](https://rsms.me/inter/) typeface (SIL Open Font
License 1.1) and egui's built-in fonts; their licences are in
`crates/gui/assets/licenses/`, come with each release as
`THIRD-PARTY-NOTICES.txt`, and are shown by `reelgene licenses`.
