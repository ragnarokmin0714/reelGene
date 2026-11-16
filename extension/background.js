// Watches the requests pages make for playlists and video files, and keeps
// a list per tab for the popup. Nothing leaves the browser from here: links
// go to reelgene only when the user picks one in the popup.

import { classify, keyOf, nameOf, originHeaders, remember, sizeOf } from "./sniff.js";

// Firefox's `browser` returns promises everywhere; Chrome's `chrome` does in
// Manifest V3.
const api = globalThis.browser ?? chrome;

// Requests a player makes; pages, scripts and pictures never hold a video.
const TYPES = ["xmlhttprequest", "media", "other"];
const ALL = { urls: ["<all_urls>"] };

// Where each pending request came from, until its response arrives. In
// memory only: a service worker that sleeps mid-request loses a Referer, no
// more.
const origins = new Map();

// Chrome hides Referer and Origin from listeners without extraHeaders;
// Firefox shows them, and refuses the option it does not know.
const EXTRA = api.webRequest.OnBeforeSendHeadersOptions?.EXTRA_HEADERS ? ["extraHeaders"] : [];

api.webRequest.onBeforeSendHeaders.addListener(
  (d) => {
    if (d.tabId < 0) return;
    const headers = originHeaders(d.requestHeaders);
    if (headers.referer || headers.origin) origins.set(d.requestId, headers);
  },
  { ...ALL, types: TYPES },
  ["requestHeaders", ...EXTRA],
);

api.webRequest.onHeadersReceived.addListener(
  (d) => {
    const headers = origins.get(d.requestId) || {};
    origins.delete(d.requestId);
    if (d.tabId < 0) return;
    const response = d.responseHeaders || [];
    const type = response.find((h) => h.name.toLowerCase() === "content-type")?.value;
    const kind = classify({ url: d.url, status: d.statusCode, type, size: sizeOf(response) });
    if (kind) found(d.tabId, { kind, url: d.url, ...headers });
  },
  { ...ALL, types: TYPES },
  ["responseHeaders"],
);

api.webRequest.onErrorOccurred.addListener((d) => origins.delete(d.requestId), ALL);

// A new web page in the tab starts a new list.
api.webRequest.onBeforeRequest.addListener((d) => forget(d.tabId), {
  urls: ["http://*/*", "https://*/*"],
  types: ["main_frame"],
});
api.tabs.onRemoved.addListener((tabId) => forget(tabId));

// One change at a time per tab, so two links found together both stay.
const queues = new Map();
function change(tabId, fn) {
  const next = (queues.get(tabId) || Promise.resolve()).then(fn).catch(console.error);
  queues.set(tabId, next);
  return next;
}

export const tabKey = (tabId) => `tab:${tabId}`;

function found(tabId, link) {
  change(tabId, async () => {
    const tab = await api.tabs.get(tabId).catch(() => null);
    if (!tab) return;
    const key = tabKey(tabId);
    const list = (await api.storage.session.get(key))[key] || [];
    const entry = {
      ...link,
      key: keyOf(link.url),
      name: nameOf(link.url),
      title: tab.title || "",
      at: Date.now(),
    };
    const next = remember(list, entry);
    await api.storage.session.set({ [key]: next });
    await badge(tabId, next.length);
  });
}

function forget(tabId) {
  if (tabId < 0) return;
  change(tabId, async () => {
    await api.storage.session.remove(tabKey(tabId));
    await badge(tabId, 0);
  });
}

async function badge(tabId, count) {
  try {
    await api.action.setBadgeBackgroundColor({ tabId, color: "#2563EB" });
    await api.action.setBadgeText({ tabId, text: count ? String(count) : "" });
  } catch {
    // The tab closed meanwhile.
  }
}
