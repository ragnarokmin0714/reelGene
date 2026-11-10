// The list of video links found in the current tab, each with a button that
// hands it to reelgene through Native Messaging.

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

function row(link) {
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
  where.textContent = new URL(link.url).host;
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

async function main() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const key = `tab:${tab?.id}`;
  const links = tab ? (await chrome.storage.session.get(key))[key] || [] : [];
  // Newest first: the video playing now is the last one found.
  $("list").append(...links.toReversed().map(row));
  $("empty").hidden = links.length > 0;
  showStatus(await ask({ cmd: "hello" }));
}

main();
