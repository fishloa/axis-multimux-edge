(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const rows = $("stream-rows");
  const tpl = $("row-tpl");
  let profiles = [];

  const num = (id) => Number($(id).value);

  function profileOptions(select, current) {
    select.replaceChildren();
    const blank = new Option("Choose a profile…", "");
    select.add(blank);
    const names = profiles.map((p) => p.name);
    if (current && !names.includes(current)) {
      select.add(new Option(current + " (missing on camera)", current));
    }
    for (const p of profiles) {
      const label = p.settings ? `${p.name} — ${p.settings}` : `${p.name} — ${p.error}`;
      const o = new Option(label, p.name);
      o.disabled = !p.settings;
      select.add(o);
    }
    select.value = current || "";
  }

  function chipsFor(row) {
    const p = profiles.find((x) => x.name === row.querySelector(".profile").value);
    const chips = row.querySelector(".chips");
    chips.replaceChildren();
    for (const k of (p && p.ignored_keys) || []) {
      const c = document.createElement("span");
      c.className = "chip";
      c.textContent = k + " ignored";
      chips.append(c);
    }
  }

  function updateUrl(row) {
    const name = row.querySelector(".name").value.trim();
    const path = name ? "hls/" + encodeURIComponent(name) + "/media.m3u8" : "";
    row.querySelector(".url").textContent = path ? new URL(path, location.href).pathname : "";
    row.querySelector(".play").href = name ? "player.html#" + encodeURIComponent(name) : "#";
    row.querySelector(".remove").setAttribute("aria-label", name ? "Remove " + name : "Remove");
  }

  function selectText(el) {
    const sel = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(el);
    sel.removeAllRanges();
    sel.addRange(range);
    note("Press Ctrl/Cmd+C to copy", "");
  }

  function addRow(m, isDefault) {
    const row = tpl.content.firstElementChild.cloneNode(true);
    row.querySelector(".name").value = m.name;
    profileOptions(row.querySelector(".profile"), m.profile);
    row.querySelector(".default").checked = isDefault;
    row.querySelector(".name").addEventListener("input", () => updateUrl(row));
    row.querySelector(".profile").addEventListener("change", () => chipsFor(row));
    row.querySelector(".remove").addEventListener("click", () => {
      if (row.querySelector(".default").checked) $("default-main").checked = true;
      row.remove();
    });
    row.querySelector(".copy").addEventListener("click", () => {
      const url = row.querySelector(".url");
      const path = url.textContent;
      if (!path) return;
      const href = new URL(path, location.href).href;
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(href).then(() => note("Copied", "ok"), () => selectText(url));
      } else {
        selectText(url);
      }
    });
    updateUrl(row);
    chipsFor(row);
    rows.append(row);
  }

  function fill(cfg) {
    $("f-main-channel").value = cfg.main.channel;
    $("f-main-width").value = cfg.main.width;
    $("f-main-height").value = cfg.main.height;
    $("f-main-framerate").value = cfg.main.framerate;
    $("f-main-codec").value = cfg.main.codec;
    $("f-max-encodes").value = cfg.max_encodes;
    $("f-idle").value = cfg.idle_timeout_secs;
    $("f-target").value = cfg.target_duration_secs;
    $("f-part").value = cfg.part_target_ms;
    $("f-window").value = cfg.window_segments;
    rows.replaceChildren();
    for (const m of cfg.streams) addRow(m, cfg.default_stream === m.name);
    $("default-main").checked = !Array.from(rows.children).some((r) => r.querySelector(".default").checked);
  }

  function read() {
    const streams = [];
    let def = null;
    for (const row of rows.children) {
      const name = row.querySelector(".name").value.trim();
      streams.push({ name, profile: row.querySelector(".profile").value });
      if (row.querySelector(".default").checked) def = name || null;
    }
    return {
      main: { channel: num("f-main-channel"), width: num("f-main-width"), height: num("f-main-height"),
              framerate: num("f-main-framerate"), codec: $("f-main-codec").value },
      streams, default_stream: def,
      max_encodes: num("f-max-encodes"), idle_timeout_secs: num("f-idle"),
      target_duration_secs: num("f-target"), part_target_ms: num("f-part"), window_segments: num("f-window"),
    };
  }

  const FIELD_IDS = { "main.channel": "f-main-channel", "main.width": "f-main-width", "main.height": "f-main-height",
    "main.framerate": "f-main-framerate", "main.codec": "f-main-codec", max_encodes: "f-max-encodes",
    idle_timeout_secs: "f-idle", target_duration_secs: "f-target", part_target_ms: "f-part", window_segments: "f-window" };

  function clearErrors() {
    document.querySelectorAll(".invalid").forEach((e) => e.classList.remove("invalid"));
    document.querySelectorAll(".err").forEach((e) => (e.textContent = ""));
  }

  function showErrors(errors) {
    for (const e of errors) {
      const m = /^streams\[(\d+)\]\.(name|profile)$/.exec(e.field);
      if (m) {
        const row = rows.children[Number(m[1])];
        if (!row) continue;
        const input = row.querySelector("." + m[2]);
        input.classList.add("invalid");
        input.parentElement.querySelector(".err").textContent = e.message;
      } else if (FIELD_IDS[e.field]) {
        $(FIELD_IDS[e.field]).classList.add("invalid");
      }
    }
  }

  function note(text, cls) {
    $("save-note").textContent = text;
    $("save-note").className = "note " + (cls || "");
  }

  function setBusy(busy) {
    $("save").disabled = busy;
    $("add-stream").disabled = busy;
    rows.querySelectorAll("input, select, button").forEach((e) => (e.disabled = busy));
  }

  async function save() {
    clearErrors();
    setBusy(true);
    note("Applying…");
    try {
      const r = await fetch("admin/config", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(read()) });
      if (r.status === 400) {
        const body = await r.json();
        showErrors(body.errors);
        note(body.errors.map((e) => `${e.field}: ${e.message}`).join(" · "), "err");
      } else if (!r.ok) {
        note("Save failed: " + (await r.text()), "err");
      } else {
        note("Applied", "ok");
        loadStatus();
      }
    } catch (e) {
      note("Save failed: " + e, "err");
    } finally {
      setBusy(false);
    }
  }

  function cell(text, cls) {
    const td = document.createElement("td");
    td.textContent = text;
    if (cls) td.className = cls;
    return td;
  }

  async function loadStatus() {
    try {
      const s = await (await fetch("admin/status")).json();
      const pill = $("encodes");
      pill.textContent = `${s.encodes.in_use}/${s.encodes.max} encodes`;
      pill.classList.toggle("full", s.encodes.in_use >= s.encodes.max);
      $("config-error").hidden = !s.last_error;
      $("config-error").textContent = s.last_error || "";
      const body = $("status-rows");
      body.replaceChildren();
      for (const st of s.streams) {
        const tr = document.createElement("tr");
        tr.append(cell(st.names.join(", ")), cell(st.settings), cell(st.state, "state-" + st.state),
          cell(String(st.fps)), cell(st.idle_secs + " s"), cell(st.last_error || "—"));
        body.append(tr);
      }
      $("status-empty").hidden = s.streams.length > 0;
    } catch (e) {
      $("encodes").textContent = "status unavailable";
    }
  }

  async function init() {
    try {
      const p = await (await fetch("admin/profiles")).json();
      profiles = p.profiles;
      $("profiles-error").hidden = !p.error;
      $("profiles-error").textContent = p.error ? "Camera profiles unavailable: " + p.error : "";
    } catch (e) {
      $("profiles-error").hidden = false;
      $("profiles-error").textContent = "Camera profiles unavailable: " + e;
    }
    try {
      fill(await (await fetch("admin/config")).json());
    } catch (e) {
      note("Failed to load config: " + e, "err");
    }
    loadStatus();
    setInterval(loadStatus, 5000);
  }

  $("add-stream").addEventListener("click", () => addRow({ name: "", profile: "" }, false));
  $("save").addEventListener("click", save);
  document.querySelectorAll(".sidebar a").forEach((a) =>
    a.addEventListener("click", () => {
      document.querySelectorAll(".sidebar a").forEach((x) => x.classList.remove("active"));
      a.classList.add("active");
    }));
  init();
})();
