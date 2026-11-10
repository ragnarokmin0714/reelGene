import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classify,
  keyOf,
  MAX_PER_TAB,
  MIN_VIDEO_BYTES,
  nameOf,
  originHeaders,
  remember,
  sizeOf,
} from "../sniff.js";

const ok = (url, type = "", size = null) => classify({ url, status: 200, type, size });

test("playlists by name or by type", () => {
  assert.equal(ok("https://cdn.example/v/index.m3u8?sig=1"), "playlist");
  assert.equal(ok("https://cdn.example/play", "application/vnd.apple.mpegurl"), "playlist");
  assert.equal(ok("https://cdn.example/play", "audio/x-mpegURL; charset=utf-8"), "playlist");
  assert.equal(classify({ url: "https://cdn.example/a.m3u8", status: 403, type: "" }), null);
});

test("whole videos, but not stream pieces or previews", () => {
  assert.equal(ok("https://cdn.example/movie.mp4", "video/mp4", 50e6), "video");
  assert.equal(ok("https://cdn.example/movie.webm", "application/octet-stream"), "video");
  assert.equal(ok("https://cdn.example/seg1.ts", "video/mp2t"), null);
  assert.equal(ok("https://cdn.example/seg1.m4s", "video/mp4"), null);
  assert.equal(ok("https://cdn.example/loop.mp4", "video/mp4", MIN_VIDEO_BYTES - 1), null);
  assert.equal(ok("https://cdn.example/page.html", "text/html"), null);
  assert.equal(ok("https://cdn.example/clip.mp4", "text/html"), null, "an error page");
  assert.equal(ok("blob:https://site.example/abc", "video/mp4"), null);
});

test("sizes from ranges and lengths", () => {
  assert.equal(sizeOf([{ name: "Content-Range", value: "bytes 0-1023/987654" }]), 987654);
  assert.equal(sizeOf([{ name: "content-range", value: "bytes 0-1023/*" }]), null);
  assert.equal(sizeOf([{ name: "Content-Length", value: "2048" }]), 2048);
  assert.equal(sizeOf([]), null);
});

test("names skip generic parts and keys drop the query", () => {
  assert.equal(nameOf("https://cdn.example/show/ep01/index.m3u8"), "ep01");
  assert.equal(nameOf("https://cdn.example/a/%E7%AC%AC1%E9%9B%86.mp4"), "第1集.mp4");
  assert.equal(nameOf("https://cdn.example/master.m3u8"), "master.m3u8");
  assert.equal(keyOf("https://cdn.example/a.m3u8?sig=1&t=2"), "https://cdn.example/a.m3u8");
});

test("a tab's list: renewed links move, playlists push videos out, capped", () => {
  const e = (key, kind = "playlist") => ({ key, kind, url: key });
  let list = remember([], e("a"));
  list = remember(list, e("b"));
  list = remember(list, e("a"));
  assert.deepEqual(list.map((x) => x.key), ["b", "a"]);
  list = remember([e("v1", "video")], e("v2", "video"));
  assert.equal(list.length, 2);
  list = remember(list, e("p"));
  assert.deepEqual(list.map((x) => x.key), ["p"]);
  for (let i = 0; i < MAX_PER_TAB + 5; i++) list = remember(list, e(`k${i}`));
  assert.equal(list.length, MAX_PER_TAB);
  assert.equal(list.at(-1).key, `k${MAX_PER_TAB + 4}`);
});

test("only where a request came from, never credentials", () => {
  const got = originHeaders([
    { name: "Referer", value: "https://site.example/watch" },
    { name: "Origin", value: "https://site.example" },
    { name: "Cookie", value: "session=secret" },
    { name: "Authorization", value: "Bearer secret" },
  ]);
  assert.deepEqual(got, { referer: "https://site.example/watch", origin: "https://site.example" });
});
