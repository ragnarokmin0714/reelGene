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
window clicked through its accessibility tree (kittest).

**Not yet verified, and the first things to check:**
- **Windows has never been compiled or run.** The development machine has no
  MSVC tools, so everything under `cfg(windows)` (tray-icon, PlaySound,
  console attach, locale, reduced motion, Mark of the Web, the hand-off
  pipe, explorer `/select`) is checked by CI only.
- **The window has never been seen.** The development machine has no
  display; layout, CJK rendering through the system font fallback, the tray
  and sounds need a look on a real desktop.
- The ffmpeg remux path is tested for its arguments, not against a real
  ffmpeg.

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
persisted, so after a crash running episodes are simply queued. Events go out
on a broadcast channel (1024 deep; a lagging receiver re-snapshots). Progress
is throttled to about ten updates a second per episode, with a trailing flush
so a stalled segment does not hide the real count. `Event::Idle` marks the
end of a batch: the moment for the "done" sound.

**The store** is SQLite on one thread behind a channel, WAL mode, schema
version in `PRAGMA user_version`; a database from a newer build is refused,
not touched. One process per data folder, enforced by an OS file lock
(`instance.rs`), because the scheduler keeps the queue in memory.

**Handing over** (`handoff.rs`). A second reelgene that finds the lock taken
does not just refuse: started bare it asks the running one to show its
window; `get`/`add URL` pass the links (without options, which would be lost
on the way, so those refuse). The window and `reelgene get`/`run` listen.
One JSON line each way over a Unix socket in a 0700 directory
(`$XDG_RUNTIME_DIR`, else the data folder; peer uid checked) or a named pipe
that rejects remote clients; never a localhost port, which any web page can
reach. The endpoint name is an FNV-1a hash of the data folder, stable across
versions. Requests are untrusted: capped at 64 KiB, timed out, http(s) only.
A handler answers at once and reads the links in the background, so
`drive` keeps going while handed links are still being read, and decides it
is finished from a queue snapshot, not only from `Event::Idle`, which may
have gone by before the handed job's message arrived.

**HTTP policy** (`http.rs`): reqwest's own retry only covers protocol NACKs,
so reelgene retries transient failures itself (network, 408/425/429/5xx) with
exponential backoff and equal jitter, honours `Retry-After` (seconds or HTTP
date; longer than two minutes goes back to the scheduler), and backs off a
whole host on 429/503. A per-host semaphore caps connections (default 6).
Timeouts are per read, not per request. Bodies held in memory are capped;
files stream to disk. A failure counts against the retry budget only if no
bytes arrived since the last one.

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
optional. It only ever gets absolute local paths with a `file:` prefix,
`-protocol_whitelist file` before every input, `-c copy`, `-nostdin`, a
timeout, and is killed on cancel. Without it, TS stays TS and separate audio
is saved beside the video.

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

## Layout

Four packages, each depending only on the ones below it in this picture. The
program is the root package; the three libraries it is built from are under
`crates/` (their package names keep the `reelgene-` prefix):

```
src/                    package `reelgene`: the executable
  main.rs cli.rs term.rs  start-up, the command line
tests/cli.rs            the real binary against a local server
crates/core/            package `reelgene-core`: the engine, no UI
  src/error.rs          one typed error; is_transient / is_expired_url
  src/net.rs            redact, is_private, may_follow
  src/hls.rs crypto.rs  M3U8 parsing, AES-128 (from reelhold)
  src/http.rs           the one client: retries, backoff, ranges, resume
  src/paths.rs          safe names and the naming template
  src/detect.rs         links in clipboard text
  src/resolver/         the site seam: mod.rs, direct.rs, extra.rs
  src/store.rs          SQLite actor
  src/engine/           scheduler (mod.rs), worker.rs, meter.rs
  src/mux.rs            concat, ffmpeg, media checks, Mark of the Web
  tests/                http.rs, engine.rs, support/ (fake CDN)
crates/app/             package `reelgene-app`: what the front ends share, no UI
  src/app.rs            App::open (places, lock, settings, ffmpeg, log) and the engine
  src/places.rs settings.rs instance.rs handoff.rs logging.rs report.rs format.rs
  src/platform.rs       which, reveal, console attach, locale, reduced motion
crates/gui/             package `reelgene-gui`: the window; the only one that knows egui
  src/gui/              mod (Gui, run), desktop, model, view, dialogs, text,
                        theme, widgets, tests
  src/tray.rs sound.rs clipboard.rs icon.rs
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
attached to a draft release with SHA-256 checksums for review before
publishing.
