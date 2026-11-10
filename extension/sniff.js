// What counts as a video link among the requests a page makes. Pure
// functions, so they run under `node --test` as well as in the browser.

const PLAYLIST_TYPES = [
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
];
const VIDEO_TYPES = [
  "video/mp4",
  "video/webm",
  "video/x-matroska",
  "video/quicktime",
  "video/x-flv",
  "video/x-m4v",
];
const VIDEO_EXTS = ["mp4", "m4v", "webm", "mov", "mkv", "flv"];
// A video file smaller than this is a preview, an advert's loop or a
// fragment, not what the page is about.
export const MIN_VIDEO_BYTES = 1024 * 1024;
// Links kept for one tab; the oldest go first.
export const MAX_PER_TAB = 50;

/// `"playlist"`, `"video"`, or `null` for anything else.
///
/// `status` is the HTTP status, `type` the Content-Type, `size` the whole
/// file's length when the response tells it (else `null`).
export function classify({ url, status, type, size }) {
  if (status < 200 || status > 299) return null;
  let u;
  try {
    u = new URL(url);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  const mime = (type || "").split(";")[0].trim().toLowerCase();
  const ext = extension(u.pathname);
  if (ext === "m3u8" || PLAYLIST_TYPES.includes(mime)) return "playlist";
  // Pieces of a stream, never whole videos.
  if (mime === "video/mp2t" || ext === "ts" || ext === "m4s") return null;
  const looks =
    VIDEO_TYPES.includes(mime) ||
    (VIDEO_EXTS.includes(ext) && (mime === "" || mime === "application/octet-stream"));
  if (!looks) return null;
  if (size !== null && size !== undefined && size < MIN_VIDEO_BYTES) return null;
  return "video";
}

/// The whole file's length from response headers: the total of a
/// Content-Range (a player asks for pieces), else Content-Length.
export function sizeOf(headers) {
  const get = (name) =>
    headers.find((h) => h.name.toLowerCase() === name)?.value ?? null;
  const range = get("content-range");
  if (range) {
    const total = range.split("/")[1];
    return total && total !== "*" ? Number(total) || null : null;
  }
  const length = get("content-length");
  return length === null ? null : Number(length) || null;
}

/// The same video asked for again (a signed link renewed, a range request)
/// is one entry: links are told apart without their query string.
export function keyOf(url) {
  const u = new URL(url);
  return u.origin + u.pathname;
}

/// The last useful part of the path: `…/ep01/index.m3u8` is `ep01`.
export function nameOf(url) {
  const parts = new URL(url).pathname.split("/").filter(Boolean).map(decode);
  const generic = /^(index|master|playlist|main|video|prog_index)$/i;
  for (let i = parts.length - 1; i >= 0; i--) {
    const stem = parts[i].replace(/\.[^.]*$/, "");
    if (stem && !generic.test(stem)) return parts[i];
  }
  return parts.at(-1) || new URL(url).hostname;
}

/// Add `entry` to a tab's list: a link seen before moves to the end with its
/// newest URL. Videos are listed only while no playlist is: with a playlist
/// around, video files are its pieces.
export function remember(list, entry) {
  let next = list.filter((e) => e.key !== entry.key);
  next.push(entry);
  if (next.some((e) => e.kind === "playlist")) {
    next = next.filter((e) => e.kind === "playlist");
  }
  return next.slice(-MAX_PER_TAB);
}

/// Only where the request came from, never who made it: cookies and
/// authorization stay in the browser.
export function originHeaders(headers) {
  const out = {};
  for (const h of headers || []) {
    const name = h.name.toLowerCase();
    if (name === "referer" || name === "origin") out[name] = h.value;
  }
  return out;
}

function extension(path) {
  const last = path.split("/").pop() || "";
  const dot = last.lastIndexOf(".");
  return dot < 0 ? "" : last.slice(dot + 1).toLowerCase();
}

function decode(s) {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}
