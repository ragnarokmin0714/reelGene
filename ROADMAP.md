# Roadmap

## v0.1.0 — the first release

Clipboard detection, the queue (add, pause, resume, retry, remove; survives
restarts), HLS and direct downloads (AES-128, fMP4, byte ranges, separate
audio), series with an episode picker, naming templates, the window (tray,
animations, sounds, notifications, English and Traditional Chinese), the
CLI, portable mode; single-instance hand-off (starting reelgene again
brings the window back; `reelgene get/add URL` passes its links to the one
running); a free-space check; a proxy (the system's, none, or HTTP/SOCKS); a
speed limit changeable while downloads run; download hours; pacing (one
item a site at a time, with a random pause between); a yt-dlp fallback for
pages without a resolver, using the user's own yt-dlp.

Release candidates (`v0.1.0-rc.N`) are published as pre-releases to try;
`v0.1.0` follows once these pass:
- run the Windows executable on a real Windows machine (CI compiles and
  tests it; nobody has run the window there);
- look at the window on a real Linux desktop, Chinese titles included;
- one download through a real ffmpeg, and one through a real yt-dlp.

## Next

- **Picture posts**: a post of one or many pictures, or pictures and videos
  mixed, saved together under the post's name. Picture formats (JPEG, PNG,
  WebP, AVIF) join the extensions a download may keep; the window says
  "pictures" and "items" for such posts rather than "episodes".
- **Cookie import**, treated as passwords: DPAPI on Windows, the system
  keyring on Linux; not stored at all in portable mode by default. Many
  sites show posts only to someone signed in.
- The resolvers that find such posts live on the private branch, as all
  site resolvers do.

## Held

Not needed for reelgene to work; each waits for something only a person can
supply (a browser to test in, real samples, a site to try).

- **Video link detection from the browser**: an extension watches network
  requests for playlists and hands them over through Native Messaging — never
  through a local HTTP port, which any web page could call.
- **One-click ffmpeg download**: a pinned version, SHA-256 checked, into the
  portable folder.
- **A Rust TS → MP4 remuxer** (H.264/H.265 + AAC, about 1,500 lines), so the
  common case needs no ffmpeg. CI compares it against ffmpeg on the same
  samples with ffprobe (duration, frame count, A/V sync); unusual streams
  fall back to ffmpeg, then to `.ts`.
- **`reelgene doctor` resolver checks**: one real request per resolver, to
  tell which ones a site change has broken.

## Later

- Auto-update, only with signed updates (minisign).
- Code signing for the Windows executable.
