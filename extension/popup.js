// The list of video links found in the current tab, each with a button that
// hands it to reelgene through Native Messaging. reelgene measures the
// playlists, so the video comes first and adverts, previews and other
// versions of it are set apart (`arrange`).

import { arrange, clock } from "./sniff.js";

// The native host reelgene registers (`reelgene browser install`).
export const HOST = "io.github.ragnarokmin0714.reelgene";

const t = (key, ...subs) => chrome.i18n.getMessage(key, subs) || key;
const $ = (id) => document.getElementById(id);

for (const el of document.querySelectorAll("[data-i18n]")) {
  el.textContent = t(el.dataset.i18n);
}

/// One message to reelgene, one reply back; a missing host is told apart
/// because it needs setting up, not retrying.
function ask(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendNativeMessage(HOST, { v: 1, ...message }, (reply) => {
      const err = chrome.runtime.lastError;
      if (err) {
        resolve({ ok: false, missing: /not found/i.test(err.message), error: err.message });
      } else {
        resolve(reply || { ok: false, error: t("noReply") });
      }
    });
  });
}

function showStatus(hello) {
  const status = $("status");
  const help = $("help");
  if (hello.ok) {
    status.className = hello.running ? "status on" : "status";
    status.textContent = t(hello.running ? "open" : "closed");
    return;
  }
  status.className = "status off";
  status.textContent = t(hello.missing ? "notSetUp" : "hostError");
  help.textContent = hello.missing ? t("setUpHelp") : hello.error;
  help.hidden = false;
}

/// Playlists reelgene is asked to measure: the newest ones.
const MAX_MEASURE = 20;

function row(link, judged) {
  const li = document.createElement("li");
  const kind = document.createElement("span");
  kind.className = "kind";
  kind.textContent = t(link.kind);
  const what = document.createElement("div");
  what.className = "what";
  const name = document.createElement("div");
  name.className = "name";
  name.textContent = link.name;
  // Never the query string: signed links carry their credential there.
  name.title = link.key;
  const where = document.createElement("div");
  where.className = "where";
  const facts = [new URL(link.url).host];
  if (judged?.live) facts.push(t("live"));
  else if (judged?.secs) facts.push(clock(judged.secs));
  if (judged?.verdict === "version") facts.push(t("versionNote"));
  if (judged?.verdict === "short" || judged?.verdict === "advert") facts.push(t("advertNote"));
  where.textContent = facts.join(" · ");
  what.append(name, where);
  const button = document.createElement("button");
  button.textContent = t("download");
  const note = document.createElement("div");
  note.className = "note";
  note.hidden = true;
  button.addEventListener("click", async () => {
    button.disabled = true;
    button.textContent = t("sending");
    const reply = await ask({
      cmd: "add",
      links: [{ url: link.url, title: link.title, referer: link.referer, origin: link.origin }],
    });
    if (reply.ok) {
      button.className = "done";
      button.textContent = t(reply.running ? "added" : "queued");
      if (!reply.running) {
        note.textContent = t("queuedNote");
        note.hidden = false;
      }
    } else {
      button.disabled = false;
      button.textContent = t("download");
      note.className = "note bad";
      note.textContent = reply.missing ? t("setUpHelp") : reply.error;
      note.hidden = false;
    }
  });
  li.append(kind, what, button, note);
  return li;
}

function show(links, judged) {
  const { main, aside } = arrange(links, judged);
  $("list").replaceChildren(...main.map((r) => row(r.link, r.judged)));
  $("aside-list").replaceChildren(...aside.map((r) => row(r.link, r.judged)));
  $("aside-title").textContent = t("asideTitle", String(aside.length));
  $("aside").hidden = aside.length === 0;
}

async function main() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const key = `tab:${tab?.id}`;
  const links = tab ? (await chrome.storage.session.get(key))[key] || [] : [];
  // Newest first until reelgene has judged them: the video playing now is
  // the last one found.
  show(links);
  $("empty").hidden = links.length > 0;
  const hello = await ask({ cmd: "hello" });
  showStatus(hello);
  const playlists = links
    .map((link, i) => ({ link, i }))
    .filter((p) => p.link.kind === "playlist")
    .slice(-MAX_MEASURE);
  if (!hello.ok || playlists.length < 2) return;
  $("measuring").hidden = false;
  const reply = await ask({
    cmd: "measure",
    links: playlists.map(({ link }) => ({ url: link.url, referer: link.referer, origin: link.origin })),
  });
  $("measuring").hidden = true;
  if (!reply.ok || !Array.isArray(reply.measured)) return;
  const judged = [];
  playlists.forEach((p, n) => {
    judged[p.i] = reply.measured[n];
  });
  show(links, judged);
}

main();
