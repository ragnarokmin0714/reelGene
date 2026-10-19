# Roadmap

## v0.1 — built, awaiting verification

Clipboard detection, the queue (add, pause, resume, retry, remove; survives
restarts), HLS and direct downloads (AES-128, fMP4, byte ranges, separate
audio), series with an episode picker, naming templates, the window (tray,
animations, sounds, English and Traditional Chinese), the CLI, portable mode.

Since then, from the v0.2 list: **single-instance hand-off** (starting
reelgene again brings the window back; `reelgene get/add URL` passes its
links to the reelgene already running); **a free-space check** (a download
that cannot fit stops early with how much it needs).

Before calling it released:
- build and run on a real Windows machine (CI compiles it; nobody has run it);
- look at the window on a real screen, Chinese titles included;
- one download through a real ffmpeg.

## v0.2

- **Video link detection from the browser**: an extension watches network
  requests for playlists and hands them over through Native Messaging — never
  through a local HTTP port, which any web page could call.
- **yt-dlp fallback** for sites without a resolver, using the user's own
  yt-dlp.
- **Speed limit, download schedule, proxy** (HTTP and SOCKS).
- **Cookie import**, treated as passwords: DPAPI on Windows, the system
  keyring on Linux; not stored at all in portable mode by default.
- **One-click ffmpeg download**: a pinned version, SHA-256 checked, into the
  portable folder.
- **A Rust TS → MP4 remuxer** (H.264/H.265 + AAC, about 1,500 lines), so the
  common case needs no ffmpeg. CI compares it against ffmpeg on the same
  samples with ffprobe (duration, frame count, A/V sync); unusual streams
  fall back to ffmpeg, then to `.ts`.
- **System notifications** when a batch finishes.
- **`reelgene doctor` resolver checks**: one real request per resolver, to
  tell which ones a site change has broken.

## Later

- Auto-update, only with signed updates (minisign).
- Code signing for the Windows executable.
