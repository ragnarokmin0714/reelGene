# reelgene

Portable video downloader for Windows and Linux, in Rust: clipboard link
detection, a resumable queue, HLS and direct downloads, whole series at once,
an egui window and a CLI over one engine. User-facing docs are in
`README.md`; what comes next is in `ROADMAP.md`. This file holds the rules and
the decisions that are not obvious from the code.

## Status

v0.1 was built on 2026-10-18 in four steps — core foundations, engine and
store, CLI, window — and is tested headless: about 150 tests, including the
engine against a fake CDN, the real binary against a local server, and the
window clicked through its accessibility tree (kittest). The rest of
v0.1.0 (hand-off, disk space, notifications, proxy, speed limit, download
hours, yt-dlp) followed on 2026-10-19; about 190 tests. What is held back
for later is in `ROADMAP.md`.

**Not yet verified, and the first things to check:**
- **Windows has never been compiled or run.** The development machine has no
  MSVC tools, so everything under `cfg(windows)` (tray-icon, PlaySound,
  console attach, locale, reduced motion, Mark of the Web, the hand-off
  pipe, toasts, explorer `/select`) is checked by CI only.
- **The window has never been seen.** The development machine has no
  display; layout, CJK rendering through the system font fallback, the tray
  and sounds need a look on a real desktop.
- The browser extension ran in a real Chromium 141 (Playwright's arm64
  build) on 2026-11-10: found an HLS playlist with its Referer, the popup
  sent it over Native Messaging, and `reelgene run` downloaded it from a CDN
  that refuses requests without one. Never in Chrome or Edge themselves, nor
  on Windows (the registry path is type-checked only).
- The ffmpeg remux path runs against a real ffmpeg only when
  `REELGENE_TEST_FFMPEG` names one (`mux::tests::a_real_ffmpeg_…`); CI has
  none, so there it is tested for its arguments.

## Commands

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # exactly what CI runs
cargo fmt --all
cargo deny check
```

## Hard rules

1. **Never name a specific video site** in anything that reaches this
   repository: code, comments, docs, test data, commit messages, branch
   names. reelgene is a general-purpose downloader. Site resolvers live on a
   private branch that changes only `crates/core/src/resolver/extra.rs`
   and adds its own modules, so it rebases onto `main` without conflicts:
   `main` never edits `extra.rs` beyond its placeholder.
2. **No watermark, logo or overlay removal; no DRM circumvention.** DRM is
   detected at parse time and refused with the system's name (`hls::check_key`).
3. **`reelgene-core` has no UI dependencies** — no egui, tray, or terminal
   progress bars. The window and the CLI are equal consumers of its API.
4. **Site changes touch only `resolver/`.** If a site fix needs the engine,
   the store or `hls.rs` changed, the abstraction leaked; that is the bug.
5. **Nothing shown or logged carries a query string** (`net::redact`):
   signed URLs keep their credential there.
6. **External programs** (ffmpeg, sound players, xdg-open, explorer, gsettings)
   are run by absolute path found in absolute `PATH` entries, with an
   argument array, never through a shell, never from the current directory.

## Load-bearing decisions

**Recipes, not URLs.** A job stores `resolver` + `spec` (+ each episode's
`hint`), never a media URL: signed CDN URLs expire in minutes. `resolve()`
runs before every attempt, every resume and every re-resolve after a
401/403/410 (at most three per attempt). `discover()` runs once, when a link
is added.

**Fallbacks, in turn, for links the user adds that nobody claims**
(`Registry::fallbacks`): first `resolver/page.rs`, the video the page names
itself (`<video>`/`<source>` src, `og:video`; a small tag scanner, not an
HTML parser, that never fails), then yt-dlp. A DRM refusal ends the chain:
nothing protected is fetched by another route.

**yt-dlp is a fallback, not a resolver of sites** (`resolver/ytdlp.rs`). It
claims nothing (`matches` is false), so clipboard links never reach an
external program; `Engine::discover` turns to `Registry::fallback` only for
a link the user added that nobody claims. It answers facts only (`-J`):
discovery lists entries, `resolve` asks for a fresh link every attempt like
any resolver, and the engine downloads, so the guards all apply (a public
page's video on a private address is refused, tested). Run with `--ignore-config` (a config could add
downloads or `--exec`), the link after `--`, a timeout, capped output. It is
asked for one file with picture and sound, else any HLS video *for its master
playlist* (`manifest_url`): such masters list the sound as audio renditions,
which the engine already fetches and joins. Never separate DASH files.

**Only an official yt-dlp runs** (`tools.rs`, `pgp.rs`). yt-dlp signs each
release's `SHA2-256SUMS` with its OpenPGP key, built in as `yt-dlp.asc`. Before
every run the file is hashed: the checksum recorded when it last passed
(`<data>/yt-dlp-checked.json`) lets it run; otherwise the latest release's list
is fetched, its signature checked, and the file must be in it. Unofficial
builds run only with `yt_dlp_unofficial`, unchecked. A list signed earlier
than the recorded one is refused, so an old signed release cannot be replayed.
A key set in settings (`yt_dlp_key`) replaces the built-in one; if it cannot
be read nothing passes, rather than falling back. "Get yt-dlp" /
`reelgene yt-dlp` downloads the release file for this system into the tools
folder, checks it before the rename, and records it. The resolver is
registered even with no yt-dlp present (`Error::Missing`), so fetching one
needs no restart; `Engine::discover` then keeps the page's own reason and
adds the hint. `pgp.rs` parses packets only (v4 RSA key and detached v4
signature over a binary file, SHA-256/384/512, unknown critical subpackets
refused); SHA and RSA are aws-lc-rs, already in the build through rustls:
hand-written RSA that is wrong accepts forgeries. Tests sign with a key made
on the spot (`tests/support/signer.rs`) and check the real release of
2026-10-21 (`tests/data/`).

**Cookies, for yt-dlp only** (`cookies.rs`, `app/seal.rs`). The user
imports a Netscape `cookies.txt`; it is checked (not text, JSON, no cookie,
all expired: refused; 1 MiB cap) and kept as `<data>/cookies.sealed`: a
`reelgene-cookies 1 <seal> <imported>` line, then the text sealed by DPAPI
on Windows (user scope, no UI) or plain in an owner-only file elsewhere.
Another seal's file is "import again", never a crash. yt-dlp runs without
cookies first; only when its error asks to sign in (`asks_to_sign_in`: its
`--cookies` hint, "sign in to confirm", …) and cookies were imported does it
run once more with `--cookies <copy>`, a 0600 copy beside the jar removed on
drop; what yt-dlp writes back is kept if it still reads as cookies and the
jar was not replaced or removed meanwhile. Crash leftovers are swept in
`App::open`, under the instance lock. Cookies never reach the engine, the
queue, logs or the report; summaries show counts and domains only. Import
and removal act at once, not on Save.

**Telling, and choosing, qualities.** `Engine::qualities` resolves the first
episode and lists its master playlist's variants, one per height (the
richest, which is what `Quality::Height` then picks), sized from
`AVERAGE-BANDWIDTH` (else `BANDWIDTH`) times the length of one variant's
playlist. Every link read is looked up this way, and `engine::choose` (the
same choice the download makes) picks the one the settings' quality gets;
its height and size are stored with the job (`QualityNote`, schema 2:
`job.height`, `job.size`) only to be told — the toast, the picker, a badge
beside the series (dropped when the row is too narrow; the title's tooltip
keeps it), `reelgene add`/`list`. Asking is off by default since settings
version 2 (Roger, 2026-11-09: choosing felt redundant, being told did not);
a version-1 file's `ask_quality: true` was the old written-out default and is
turned off on load (`Settings::upgraded`). With it on, a link the user adds
that offers two or more opens the picker even for one episode; one job per
quality ticked, and with several the template gains ` [720p]` so the files
stay apart. Links queued by themselves never ask.

**Resume granularity.** Series per episode (scheduler), playlist per segment,
single file per byte (`Range` + `If-Range` with the stored ETag or
Last-Modified, so a changed remote file is downloaded whole, not spliced).
A segment's identity is its media sequence number. Stored progress carries a
playlist fingerprint (variant, segment count, first sequence, init section,
duration); any change discards the episode's progress — splicing two encodes
makes a file that plays for 40 seconds and breaks. Workers read resume state
from the store, never from the scheduler's copy (which never sees what a
worker wrote; this was a real bug).

**Commands in, events out.** One scheduler task owns all episode state and is
its only writer; workers report back with a message. Pausing cancels a
worker's token and keeps everything finished; nothing about "running" is
persisted, so after a crash running episodes are simply queued. Download
hours are a second switch, `Command::Hold`, apart from the user's
Start/Stop: a queue started outside the hours waits for them, one the user
stopped stays stopped when they come. The engine has no clock; the app's
`schedule.rs` checks local time every half minute (and at once when the
hours change). Pacing (`EngineConfig::pace`, `Command::SetPace`) is per site,
keyed by the job's source host: one item a site at a time, and a random
pause in `[least, most]` seconds after each before the next starts (a
`Msg::Tick` wakes the scheduler). Items start with their resolver asking the
site, so that is paced too; segments from the CDN are not. Events go out on a broadcast channel (1024 deep; a lagging receiver re-snapshots). Progress
is throttled to about ten updates a second per episode, with a trailing flush
so a stalled segment does not hide the real count. `Event::Idle` marks the
end of a batch: the moment for the "done" sound.

**The store** is SQLite on one thread behind a channel, WAL mode, schema
version in `PRAGMA user_version`; a database from a newer build is refused,
not touched. One process per data folder, enforced by an OS file lock
(`instance.rs`), because the scheduler keeps the queue in memory.

**Handing over** (`handoff.rs`). A second reelgene that finds the lock taken
does not just refuse: started bare it asks the running one to show its
window; `get`/`add URL` pass the links with their `--title` and `-H`
headers (`take`; plain links still go as `add`, which older instances
understand); other options would be lost on the way, so those refuse. The
window and `reelgene get`/`run` listen.
One JSON line each way over a Unix socket in a 0700 directory
(`$XDG_RUNTIME_DIR`, else the data folder; peer uid checked) or a named pipe
that rejects remote clients; never a localhost port, which any web page can
reach. The endpoint name is an FNV-1a hash of the data folder, stable across
versions. Requests are untrusted: capped at 64 KiB, timed out, http(s) only.
A handler answers at once and reads the links in the background, so
`drive` keeps going while handed links are still being read, and decides it
is finished from a queue snapshot, not only from `Event::Idle`, which may
have gone by before the handed job's message arrived.

**The browser extension** (`extension/`, `app/browser.rs`, `src/native.rs`).
An MV3 extension for Chromium browsers watches `webRequest` for playlists
(`.m3u8` or an mpegurl type) and video files (by type or extension, 1 MiB
or more, and only while the tab has no playlist: with one, they are its
pieces), keeps a list per tab in `storage.session`, and the popup sends one
over Native Messaging. Never a localhost port. Only the request's `Referer`
and `Origin` travel with it (read with `extraHeaders`), never cookies or
`Authorization`: those would be session credentials stored in the queue,
and replaying a site's sign-in is not reelgene's business. The extension's
ID is fixed by the public key in its manifest (the private key was thrown
away; a store version would get its own ID); `allowed_origins` names only
it and `native::run` checks the origin argument again. The browser starts
the very executable with that origin as its first argument (`main` checks
before parsing a command line, and attaches no console: stdout is the
message pipe, which is why `cli::add` reports through a callback). A
message is a native-endian u32 length and JSON, capped at 64 KiB; `hello`
says whether a reelgene runs, `add` hands the links to it (`take`) or, with
none running, queues them as `reelgene add` would. The extension's files
are built into the program (`browser::EXTENSION`, `include_bytes!`) and
written to `<data>/browser-extension` by `install`, so the two are always
one version; the user loads that folder unpacked once. Host manifests go to
`~/.config/<browser>/NativeMessagingHosts/` on Linux (only for browsers
whose folder exists; Chromium really reads `<profile dir>/NativeMessagingHosts`,
and the default profile is that folder) and on Windows to
`<data>/<host>.json`, named by a key per browser,
`HKCU\Software\<vendor>\<browser>\NativeMessagingHosts\<host>`.
At window start `browser::refresh` rewrites the extension if it differs and
re-points registrations whose program is gone (a moved portable folder),
leaving ones that start another existing copy alone. Headers given to a
direct link are put in its spec by `Discovery::give_headers` before the
quality lookup, which fetches the playlist with them.

**HTTP policy** (`http.rs`): reqwest's own retry only covers protocol NACKs,
so reelgene retries transient failures itself (network, 408/425/429/5xx) with
exponential backoff and equal jitter, honours `Retry-After` (seconds or HTTP
date; longer than two minutes goes back to the scheduler), and backs off a
whole host on 429/503. A per-host semaphore caps connections (default 6).
Timeouts are per read, not per request. Bodies held in memory are capped;
files stream to disk. A failure counts against the retry budget only if no
bytes arrived since the last one. The proxy (`http::Proxy`) is the system's
by default, `none`, or an http(s)/socks5(h) URL that replaces the system's;
one that does not parse stops the engine from starting rather than silently
going direct, and its password is never shown (`Display`, the report, error
messages that do not echo the input). The speed limit is one `Limiter` per
client, shared by every transfer: a clock that each chunk moves forward by
its size at the allowed rate, idle time earning at most half a second of
burst; changing it takes effect with the next chunk of every running
transfer.

**The local network.** `net::is_private` decides from the URL alone (no DNS).
Auto-detected private links are listed, never fetched by themselves. A public
source may not lead into private addresses: redirects (reqwest policy),
variant, segment and key URLs, and a resolver's output are all checked with
`net::may_follow`. A link the user adds to their own NAS may go anywhere.

**Checking what is saved.** A segment or file whose first bytes are a web
page, JSON or XML (`mux::looks_like_text`; only clear text, since every media
container starts with binary) is `Error::NotMedia`. It counts as an expired
URL, because CDNs commonly answer a refused signature with an error page and
status 200: re-resolve, at most three times, then fail with that message. The
check runs before decryption, or an encrypted segment's error page would show
up as a decryption failure. The joined file must be exactly the recorded
parts (else the episode is downloaded again: something changed them on disk),
an ffmpeg MP4 under half its input means a lost stream (keep the source), and
a TS file playing far shorter than its playlist (`mux::ts_duration`, first
and last video PTS) gets `Notice::ShortDuration` but is kept.

**Disk space** is checked before the disk is full, not when it is
(`Worker::check_space`, `Error::DiskFull`, not transient). A file is checked
once the server states its size, before the body; a playlist after three
segments, estimated from their real sizes track by track (separate audio is a
tenth of the video's bitrate), because a variant's `BANDWIDTH` is a peak and
a bare media playlist has no size at all. The need counts the parts still to
come, the joined copy and, with ffmpeg, the MP4, plus 256 MiB of headroom.
`EngineConfig::free_space` is a function so tests can fill the disk.

**Logging.** `log` facade; the program writes one file a day to
`<data>/logs/`, kept 14 days: `reelgene*` targets at info, everything else
only at warn. The core logs what events do not show (retries, host backoff,
re-resolves, restarts, refused pages); `logging::follow` writes the queue's
events. Lines carry only redacted URLs and never clipboard content. The
problem report (`report.rs`) adds settings, the queue and the last 400 log
lines, with the home folder masked as `~`.

**Files.** Every name from a website goes through `paths::sanitize_component`
(separators, control characters, Windows device names, trailing dots, a
200-byte cap at a character boundary); the template renders component by
component; `output_path` refuses anything that leaves the download folder.
Partial files live in `<download folder>/.reelgene/<episode id>/` so the
final rename never crosses filesystems. Single files keep only media
extensions.

**ffmpeg** is not bundled (size, security updates, GPL obligations) and is
optional, but separate sound is only joined with it, and some sites never
offer picture and sound together. "Get ffmpeg" / `reelgene ffmpeg`
(`tools/ffmpeg.rs`) fetches BtbN's static LGPL build of the newest release
branch from the rolling `latest` release, checks the archive against
`checksums.sha256` (unsigned: integrity, not origin; nobody signs ffmpeg
builds, and BtbN prunes old builds, so a hash pinned in reelgene would break),
and copies out only `…/bin/ffmpeg(.exe)`, capped at 1 GiB; the archive is
deleted. zip and tar.xz are read with pure-Rust crates. The tools folder's
ffmpeg — fetched or dropped in — has its SHA-256 recorded on first use
(`<data>/ffmpeg-checked.json`); a changed file is not run until "Check again"
(`find_ffmpeg(.., accept)`) or `reelgene ffmpeg --accept`. One set in settings
or on PATH is not watched: the user's or the system's. "Check again" probes
and hands it to the engine (`Engine::set_ffmpeg`) without a restart.
`convert_mp4` off keeps TS even with ffmpeg present. It only ever gets
absolute local paths with a `file:` prefix, `-protocol_whitelist file`
before every input, `-c copy`, `-nostdin`, a timeout, and is killed on
cancel. `-format_whitelist` (`mux::INPUT_FORMATS`: mpegts, mov, matroska,
bare audio) is there because on ffmpeg 9.0 a ".ts" that was really an
ffconcat script read another local file into the MP4; no codec whitelist,
since stream probing opens decoders and a missed codec would break real
downloads. Without ffmpeg, TS stays TS and separate audio is saved beside the
video. A real ffmpeg 9.0 joined a 720p VP9 video with AAC sound on
2026-10-24. "Import ffmpeg…" / `reelgene ffmpeg --from` takes a build's
zip or tar.xz (only `bin/ffmpeg`, read to the end so a `-shared` build is
refused by its `avcodec` libraries; .7z is not read) or the program; the
old one is set aside as `.old` and put back unless the new one probes as
ffmpeg. ffmpeg's source code (GitHub's "Source code" zip, the site's
tarballs: `configure` and `libavcodec/` at the top) is refused by that
name, not as "no bin/ffmpeg". An archive is never the ffmpeg program
(`tools::is_archive`): picked with the program's Browse… it is imported,
typed it cannot be saved, and one already in settings is skipped with a
warning rather than run (Windows answered "not a valid Win32
application"). A dated snapshot build (`Ffmpeg::built`, BtbN's `-20261023`
suffix) over 183 days old gets a warning (`app::ffmpeg_age_warning`);
undated release and distribution builds are not judged, as distributions
patch their own.

**Getting yt-dlp other ways.** "Import yt-dlp…" / `reelgene yt-dlp --from`
checks a downloaded file against the latest signed list, as a fetch does,
and it must fit this system (`fits_this_system`: its own release file, any
.exe on Windows, the Python `yt-dlp` elsewhere); the copy is hashed again
before the rename. `yt_dlp_source` replaces GitHub with a mirror folder
(`app::yt_dlp_source`: http(s), no query, a trailing slash added): safe
because the signature and the no-going-back rule still decide.

**The tools folder** (`app::tools_dirs`) is `tools` beside a portable
program (before rc.6 it was the program's own folder, still searched, never
written), else `<data>/tools`, or `Settings::tools_dir`. `tools_dir_problem`
refuses one inside the download folder or the system's Downloads folder
(`Places::user_downloads`): downloaded files must never land beside the
programs, as Windows loads a program's DLLs from its own folder first. A
refused folder in settings falls back to the default with a warning; the
window will not save one. The window shows folders in use in their fields
(not as grey hints, which cannot be edited) and saves one equal to the
default as `None`, so a portable copy keeps following its own folder; a
relative or file path cannot be saved (it used to be dropped silently).
Changing the tools folder moves ffmpeg and yt-dlp along unless unticked
(`app::move_tools`, copy-and-remove across disks, never over a file
already there); their records go by checksum, so they still pass, and
program paths in settings that pointed into the old folder follow
(`app::moved_path`). A program path in settings that is the tools folder's
own copy (`ToolsDirs::own`; picked there with Browse…) counts as none set,
in the engine and in the form: it is reelgene's to fetch and watch, and
"Get yt-dlp" used to refuse it as "a yt-dlp of their own".

**How it looks.** When stability, performance and compatibility are close,
appearance decides (Roger, 2026-10-19). All colours, spacing and type live in
`gui/theme.rs` — one palette for dark and one for light, both following the
system — and the small pieces in `gui/widgets.rs`: painter-drawn icons
(egui 0.37 bundles no icon font), icon buttons, filled buttons, badges, the
slim progress bar and the card frame. Every clickable widget names itself
through `WidgetInfo`, which tooltips, screen readers and kittest all rely on.
The typeface is Inter (Regular and SemiBold, `assets/fonts/`): egui ships one
light weight, and without bold there is no hierarchy. Font licences are in
`assets/licenses/`, printed by `reelgene licenses` and attached to releases.

**Seeing the window on a machine without a display.**
`cargo test -p reelgene-gui --features screenshots -- --ignored screens`
renders the main states (empty, queue in both themes and both languages,
detected links, picker, settings, a narrow window) to `target/screens/*.png`
through wgpu on Mesa's software renderer. Look at them after any visual
change. The feature is the only way wgpu enters a build; the program uses
glow, and cargo-deny keeps wgpu out of the default graph.

**The window** (`gui/`): eframe/egui 0.37 with glow (smaller than wgpu, works
in VMs). The minor version is pinned because egui breaks its API between
minors: upgrade in a commit of its own. `model.rs` turns events into rows and
effects (sounds, toasts, time left) with no egui in it, so it is unit-tested; drawing
collects `Action`s and `Gui::act` carries them out afterwards. CJK comes from
eframe's system font fallback; egui 0.37 bundles no symbol font, so the check
and info marks are painted. The initial Start/Stop state comes from how the
engine was started, not from settings. Strings live in `gui/text.rs`, one
struct per language, so a missing translation is a compile error. eframe
persists only the window's size and place (`<data>/window.ron`, so portable
installs keep it beside them); `persist_egui_memory` is off because stale UI
state survives updates and hides layout changes. Folder and file pickers are
`rfd` (the desktop portal on Linux, no GTK), run on a thread of their own
because the dialog blocks its thread until it closes.

**Windows that come and go.** Wayland cannot hide a window, so there
closing to the tray really closes it: `run` loops over `run_native`, and with
no window open `Desktop::wait_for_tray` carries out tray commands until one
asks for a window again. Everything that must outlive a window lives in
`gui/desktop.rs`: the clipboard watcher, sounds (decided from engine events,
not from the window), links being read (a single video is queued from the
background task itself), and the message queue to the window. A closing
`Gui` hands its unfinished business (detected links, series waiting for the
picker) to the stash, and the next one takes it. On Windows and X11 the
window hides instead; Windows could not reopen anyway, because its tray's
clicks arrive through the window's message loop.

**Desktop glue.** Clipboard: a thread polls once a second, ignores what was
there at startup, and passes on only links that some resolver claims, so
copying ordinary pages adds nothing. Tray: `ksni` (D-Bus, no GTK) on Linux,
`tray-icon` on Windows (created on the window's thread). Closing goes to the
tray only if a tray actually registered. Sounds are synthesised WAVs played with PlaySound or
`pw-play`/`paplay`/`aplay`: linking an audio library would need ALSA on Linux
and stop reelgene from starting at all where it is missing.
Notifications (`notify.rs`) come when a batch ends and the window is not in
front: D-Bus `Notify` through the zbus ksni already brings (tested against a
private `dbus-daemon` with a stand-in service), a WinRT toast on Windows
through the `windows` crate eframe already brings. Windows drops toasts from
a program it does not know, so the app id is registered under
`HKCU\Software\Classes\AppUserModelId\reelgene` first — the one trace
reelgene leaves in the registry.

## Layout

Four packages, each depending only on the ones below it in this picture. The
program is the root package; the three libraries it is built from are under
`crates/` (their package names keep the `reelgene-` prefix):

```
src/                    package `reelgene`: the executable
  main.rs cli.rs term.rs  start-up, the command line
  native.rs             started by a browser: the extension's native host
extension/              the browser extension (MV3), built into the program
  sniff.js              what counts as a video link (node --test: test/)
tests/cli.rs            the real binary against a local server
crates/core/            package `reelgene-core`: the engine, no UI
  src/error.rs          one typed error; is_transient / is_expired_url
  src/net.rs            redact, is_private, may_follow
  src/hls.rs crypto.rs  M3U8 parsing, AES-128 (from reelhold)
  src/pgp.rs            OpenPGP signature check
  src/cookies.rs        imported cookies: check, seal, lend to yt-dlp
  src/tools/            yt_dlp.rs (official only), ffmpeg.rs (fetch, watch)
  src/http.rs           the one client: retries, backoff, ranges, resume
  src/paths.rs          safe names and the naming template
  src/detect.rs         links in clipboard text
  src/resolver/         the site seam: mod.rs, direct.rs, page.rs, ytdlp.rs, extra.rs
  src/store.rs          SQLite actor
  src/engine/           scheduler (mod.rs), worker.rs, meter.rs
  src/mux.rs            concat, ffmpeg, media checks, Mark of the Web
  tests/                http.rs, engine.rs, support/ (fake CDN)
crates/app/             package `reelgene-app`: what the front ends share, no UI
  src/app.rs            App::open (places, lock, settings, ffmpeg, log) and the engine
  src/places.rs settings.rs instance.rs handoff.rs logging.rs report.rs format.rs
  src/platform.rs       which, reveal, console attach, locale, reduced motion
  src/browser.rs        the extension's host: registration, messages
  src/seal.rs           how cookies are kept: DPAPI on Windows
crates/gui/             package `reelgene-gui`: the window; the only one that knows egui
  src/gui/              mod (Gui, run), desktop, model, view, dialogs, text,
                        theme, widgets, tests
  src/tray.rs sound.rs notify.rs clipboard.rs icon.rs
  assets/               Inter fonts; font licences
```

The rest of the program reaches the window only through `reelgene_gui::run`,
so the toolkit can be replaced (or a second one tried) without touching the
CLI, the settings or the engine. `cargo build -p reelgene --no-default-features`
builds the command line alone, without any window toolkit; CI keeps it
compiling, because features the window turns on for the whole graph (once:
`log`'s `std`) are missing there.

## Testing

The engine is tested against `tests/support`, a hand-rolled HTTP/1.1 server
with scripted faults (status codes with `Retry-After`, dropped connections,
stalls). Every behaviour — resume, expiry, pause, shutdown and restart,
changed playlists, DRM, the local-network guard — has an end-to-end test
there. GUI tests use English strings (the test context has no CJK fonts; the
one Chinese test allows missing glyphs) and must give every input widget an
accessible name, which kittest checks.

## Conventions

- Conventional Commits; a body only for what the diff cannot say, as terse
  bullets.
- Comments say why. Match the surrounding code's density.
- Before every commit, grep for site names (rule 1).

## Releases

A `v*` tag runs `.github/workflows/release.yml`: Windows x86-64 built
natively, Linux x86-64 and ARM64 with `cargo zigbuild` against glibc 2.31, all
published as a release with SHA-256 checksums, as gamegene does. A tag with
a `-` (`v0.1.0-rc.1`) is a pre-release: downloadable for trying on real
machines, never shown as the latest; the plain tag follows once it passes.
