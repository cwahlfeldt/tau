// Read identity straight from the bundled tauri.conf.json that tau
// generated. window.__TAURI__ exists because withGlobalTauri is enabled
// for local-file wraps (see tau's scaffold).
async function showIdentity() {
  try {
    const app = window.__TAURI__?.app;
    if (!app) throw new Error("Tauri JS API unavailable");
    const [name, version, identifier] = await Promise.all([
      app.getName(),
      app.getVersion(),
      app.getIdentifier(),
    ]);
    document.getElementById("title").textContent = name;
    document.getElementById("conf-name").textContent = name;
    document.getElementById("conf-id").textContent = identifier;
    document.getElementById("conf-version").textContent = version;
  } catch (err) {
    document.getElementById("title").textContent = "Configured Demo";
    document.getElementById("identity").textContent =
      `(Tauri API unavailable: ${err.message})`;
  }
}

// data/notes.json is bundled because of the `include` glob — there is no
// <link>/<script>/<img> referencing it from index.html.
async function showNotes() {
  const list = document.getElementById("notes");
  try {
    const res = await fetch("./data/notes.json");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const notes = await res.json();
    list.innerHTML = "";
    for (const n of notes) {
      const li = document.createElement("li");
      li.textContent = n;
      list.appendChild(li);
    }
  } catch (err) {
    list.innerHTML = `<li>failed to fetch data/notes.json: ${err.message}</li>`;
  }
}

document.addEventListener("DOMContentLoaded", () => {
  showIdentity();
  showNotes();
});
