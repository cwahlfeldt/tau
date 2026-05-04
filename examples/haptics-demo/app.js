// Wires every button[data-cmd] to the corresponding haptics call.
// The plugin is exposed by tau at window.__TAURI_PLUGIN_HAPTICS__.

const status = document.getElementById("status");

function setStatus(msg, kind) {
  status.textContent = msg;
  status.classList.remove("ok", "err");
  if (kind) status.classList.add(kind);
}

function readyCheck() {
  const h = window.__TAURI_PLUGIN_HAPTICS__;
  if (!h) {
    setStatus(
      "haptics plugin not available — this demo only works on Android or iOS",
      "err",
    );
    for (const b of document.querySelectorAll("button[data-cmd]")) {
      b.disabled = true;
    }
    return false;
  }
  return true;
}

async function trigger(cmd, arg) {
  const h = window.__TAURI_PLUGIN_HAPTICS__;
  const t0 = performance.now();
  try {
    let result;
    switch (cmd) {
      case "vibrate":
        result = await h.vibrate(Number(arg));
        break;
      case "impactFeedback":
        result = await h.impactFeedback(arg);
        break;
      case "notificationFeedback":
        result = await h.notificationFeedback(arg);
        break;
      case "selectionFeedback":
        result = await h.selectionFeedback();
        break;
      default:
        throw new Error(`unknown command ${cmd}`);
    }
    const ms = Math.round(performance.now() - t0);
    if (result && result.status === "error") {
      setStatus(`${cmd}(${arg ?? ""}) → error: ${JSON.stringify(result.error)}`, "err");
    } else {
      setStatus(`${cmd}(${arg ?? ""}) → ok (${ms} ms)`, "ok");
    }
  } catch (e) {
    setStatus(`${cmd}(${arg ?? ""}) → threw: ${e?.message ?? e}`, "err");
  }
}

addEventListener("DOMContentLoaded", () => {
  if (!readyCheck()) return;
  setStatus("ready — tap any button");
  for (const button of document.querySelectorAll("button[data-cmd]")) {
    button.addEventListener("click", () => {
      trigger(button.dataset.cmd, button.dataset.arg);
    });
  }
});
