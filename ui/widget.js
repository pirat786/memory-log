const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
const t = (key, params) => (window.I18N ? window.I18N.t(key, params) : key);

let expanded = false;
let pinned = localStorage.getItem("widgetPinned") === "1";
let pickingFiles = false;
let hoverTimer = null;
let leaveTimer = null;
let pollTimer = null;
let isAwake = false;
let isPaused = false;
let dragActive = false;
let dropBusy = false;
let dropStartedAt = 0;
let dropProgress = { done: 0, total: 0, pct: 0 };
let dropTimer = null;
let dropQueue = [];
let currentJob = null;
let nextJobId = 1;
let lastPanelHeight = 0;
let resizeRaf = 0;

function measurePanelHeight() {
  const panel = $("panel");
  const head = panel?.querySelector(".head");
  const pane = $("pane-files");
  if (!panel || panel.hidden || !head || !pane) return 245;
  const contentH = head.offsetHeight + pane.scrollHeight + 48;
  return Math.max(220, Math.min(460, Math.ceil(contentH)));
}

function syncPanelHeight() {
  if (!expanded) return;
  cancelAnimationFrame(resizeRaf);
  resizeRaf = requestAnimationFrame(() => {
    if (!expanded) return;
    const h = measurePanelHeight();
    if (Math.abs(h - lastPanelHeight) > 1) {
      lastPanelHeight = h;
      invoke("widget_resize", { expanded: true, height: h, focus: false }).catch(() => {});
    }
  });
}

function updateStatusIndicators() {
  for (const id of ["dot", "headDot"]) {
    const el = $(id);
    if (!el) continue;
    el.classList.toggle("busy", dropBusy);
    el.classList.toggle("paused", !dropBusy && isPaused);
    el.classList.toggle("awake", !dropBusy && !isPaused && isAwake);
  }
  const statusKey = isPaused
    ? "widget.status_paused"
    : isAwake
      ? "widget.status_awake"
      : "widget.status_ok";
  $("openMain").title = `${t("widget.open_main")} · ${t(statusKey)}`;
}

function updatePinButton() {
  const btn = $("pinToggle");
  btn.classList.toggle("active", pinned);
  btn.title = t(pinned ? "widget.unpin" : "widget.pin");
}

function schedulePoll() {
  clearTimeout(pollTimer);
  pollTimer = setTimeout(refreshState, expanded ? 15000 : 30000);
}

async function setExpanded(next) {
  clearTimeout(hoverTimer);
  clearTimeout(leaveTimer);
  if (expanded === next) return;
  expanded = next;
  $("edgeTab").hidden = expanded;
  $("panel").hidden = !expanded;
  if (expanded) {
    const h = lastPanelHeight || 245;
    lastPanelHeight = h;
    await invoke("widget_resize", { expanded: true, height: h, focus: true });
    syncPanelHeight();
    refreshState();
  } else {
    lastPanelHeight = 0;
    await invoke("widget_resize", { expanded: false, height: null, focus: false });
    schedulePoll();
  }
}

$("edgeTab").addEventListener("mouseenter", () => {
  hoverTimer = setTimeout(() => setExpanded(true), 120);
});
$("edgeTab").addEventListener("mouseleave", () => clearTimeout(hoverTimer));
$("edgeTab").addEventListener("click", () => setExpanded(true));

$("panel").addEventListener("mouseenter", () => clearTimeout(leaveTimer));
$("panel").addEventListener("mouseleave", () => {
  if (
    dragActive ||
    dropBusy ||
    pinned ||
    pickingFiles ||
    document.activeElement === $("dropZipMode")
  ) {
    return;
  }
  leaveTimer = setTimeout(() => setExpanded(false), 450);
});
$("collapse").onclick = () => setExpanded(false);
$("openMain").onclick = () => invoke("show_main_window");
$("pinToggle").onclick = () => {
  pinned = !pinned;
  localStorage.setItem("widgetPinned", pinned ? "1" : "0");
  updatePinButton();
};

listen("widget-toggle", () => setExpanded(!expanded));
listen("widget-expand", () => setExpanded(true));
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") setExpanded(false);
});

async function refreshState() {
  try {
    const [power, status] = await Promise.all([invoke("power_status"), invoke("status")]);
    isAwake = Boolean(power.keep_awake);
    isPaused = Boolean(status.paused);
    updateStatusIndicators();
  } catch {}
  schedulePoll();
}

function fmtBytes(n) {
  const units = t("unit.bytes").split(",");
  const u = units.length >= 5 ? units : ["B", "KB", "MB", "GB", "TB"];
  if (n < 1024) return `${n} ${u[0]}`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} ${u[1]}`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} ${u[2]}`;
  return `${(n / 1073741824).toFixed(2)} ${u[3]}`;
}

listen("tauri://drag-enter", () => {
  dragActive = true;
  setExpanded(true);
  $("dropZone").classList.add("over");
});
listen("tauri://drag-leave", () => {
  dragActive = false;
  $("dropZone").classList.remove("over");
});

function fmtElapsed(ms) {
  if (ms < 1000) {
    const msUnit = window.I18N?.code === "ru" ? "мс" : "ms";
    return `${ms} ${msUnit}`;
  }
  const secs = ms / 1000;
  if (secs < 60) return `${secs.toFixed(1)} ${t("unit.sec")}`;
  const m = Math.floor(secs / 60);
  const s = String(Math.floor(secs % 60)).padStart(2, "0");
  return `${m}:${s}`;
}

let dropEmaBytesPerMs = 0;
let dropLastSample = null;
let dropDisplayedEtaMs = null;
let dropLastEtaAt = 0;

function resetDropMetrics(now) {
  dropEmaBytesPerMs = 0;
  dropLastSample = { t: now, done: 0 };
  dropDisplayedEtaMs = null;
  dropLastEtaAt = 0;
}

function fmtSpeedMbps(mbps) {
  if (!Number.isFinite(mbps) || mbps <= 0) return "";
  const unit = `${t("unit.mb")}/${t("unit.sec")}`;
  return mbps >= 100 ? `${Math.round(mbps)} ${unit}` : `${mbps.toFixed(1)} ${unit}`;
}

function fmtEta(ms) {
  let secs = Math.max(1, Math.round(ms / 1000));
  if (secs >= 15) {
    secs = Math.max(15, Math.round(secs / 5) * 5);
  }
  if (secs < 60) return `~${secs} ${t("unit.sec")}`;
  const m = Math.floor(secs / 60);
  const s = String(secs % 60).padStart(2, "0");
  return `~${m}:${s}`;
}

function renderDropProgress() {
  if (!dropBusy) return;
  const now = Date.now();
  const elapsedMs = now - dropStartedAt;
  const elapsed = fmtElapsed(elapsedMs);
  const { done, total, pct } = dropProgress;
  $("dropFill").style.width = `${pct}%`;
  if (total > 0) {
    const bytesPerMs =
      dropEmaBytesPerMs > 0
        ? dropEmaBytesPerMs
        : elapsedMs >= 400 && done > 0
          ? done / elapsedMs
          : 0;
    const spd = bytesPerMs > 0 ? fmtSpeedMbps((bytesPerMs * 1000) / 1048576) : "";
    let eta = "";
    if (elapsedMs >= 2000 && done > 0 && done < total && bytesPerMs > 0) {
      if (dropDisplayedEtaMs === null || now - dropLastEtaAt >= 1000) {
        const rawEtaMs = (total - done) / bytesPerMs;
        dropDisplayedEtaMs =
          dropDisplayedEtaMs === null ? rawEtaMs : dropDisplayedEtaMs * 0.65 + rawEtaMs * 0.35;
        dropLastEtaAt = now;
      }
      eta = fmtEta(dropDisplayedEtaMs);
    }
    const parts = [`${pct}%`];
    if (spd) parts.push(spd);
    if (eta) parts.push(eta);
    parts.push(`${fmtBytes(done)}/${fmtBytes(total)}`);
    const text = parts.join(" · ");
    $("dropSummary").textContent = currentJob ? `${currentJob.label} · ${text}` : text;
  } else {
    const text = `${t("widget.files.processing")} ${elapsed}`;
    $("dropSummary").textContent = currentJob ? `${currentJob.label} · ${text}` : text;
  }
}

function restoreSelect(id, key, fallback) {
  const el = $(id);
  const saved = localStorage.getItem(key);
  if (saved && [...el.options].some((o) => o.value === saved)) {
    el.value = saved;
  } else {
    el.value = fallback;
  }
}

restoreSelect("dropZipMode", "widgetZipMode", "fast");
$("dropZipMode").onchange = () => {
  localStorage.setItem("widgetZipMode", $("dropZipMode").value);
};

// Не запоминаем удаление оригиналов между сессиями, чтобы не удалить файлы случайно.
$("dropDeleteAfter").checked = false;
$("dropDeleteAfter").onchange = () => {
  $("dropZone").classList.toggle("danger-mode", $("dropDeleteAfter").checked);
};

listen("quick-drop-progress", (e) => {
  if (!dropBusy || !e?.payload) return;
  const now = Date.now();
  if (e.payload.total !== dropProgress.total || e.payload.done < dropProgress.done) {
    resetDropMetrics(now);
  }
  dropProgress = e.payload;
  if (!dropLastSample) {
    dropLastSample = { t: now, done: dropProgress.done };
  } else {
    const dt = now - dropLastSample.t;
    const dBytes = dropProgress.done - dropLastSample.done;
    if (dt >= 400 && dBytes > 0) {
      const inst = dBytes / dt;
      dropEmaBytesPerMs = dropEmaBytesPerMs === 0 ? inst : dropEmaBytesPerMs * 0.8 + inst * 0.2;
      dropLastSample = { t: now, done: dropProgress.done };
    }
  }
  renderDropProgress();
});

function pathBaseName(p) {
  if (!p) return "";
  const parts = String(p).replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function formatJobLabel(paths) {
  const first = pathBaseName(paths[0]);
  return paths.length > 1 ? `${first} +${paths.length - 1}` : first;
}

function updateOutputsHeader() {
  const hasAny = $("dropOutputs").childElementCount > 0 || $("dropErrors").textContent.length > 0;
  $("dropOutputsHead").hidden = !hasAny;
}

function makeBadge(text, cls = "") {
  const b = document.createElement("span");
  b.className = cls ? `drop-res-badge ${cls}` : "drop-res-badge";
  b.textContent = text;
  return b;
}

function appendDropReportRow(rep) {
  const saved = rep.input_bytes - rep.output_bytes;
  const pct = rep.input_bytes > 0 ? Math.round((saved / rep.input_bytes) * 100) : 0;
  const kind = {
    "zip-unpack": "result_unpacked",
    "zip-pack": "result_packed",
  }[rep.kind];

  const row = document.createElement("div");
  row.className = "drop-result";
  row.title = rep.output_path;

  const body = document.createElement("div");
  body.className = "drop-res-body";

  const top = document.createElement("div");
  top.className = "drop-res-top";
  const nameEl = document.createElement("span");
  nameEl.className = "drop-res-name";
  nameEl.textContent = pathBaseName(rep.output_path) || t(`widget.files.${kind}`);
  top.append(nameEl, makeBadge(t(`widget.files.${kind}`)));
  if (rep.kind !== "zip-unpack" && pct !== 0) {
    top.append(makeBadge(`${pct > 0 ? "−" : "+"}${Math.abs(pct)}%`, pct > 0 ? "good" : ""));
  }
  if (rep.deleted_original) {
    top.append(makeBadge(t("widget.files.deleted_original"), "warn"));
  }

  const meta = document.createElement("div");
  meta.className = "drop-res-meta";
  meta.textContent = `${t("widget.files.items_count", { n: rep.count })} · ${fmtBytes(rep.input_bytes)} → ${fmtBytes(rep.output_bytes)} · ${fmtElapsed(rep.elapsed_ms)}`;

  body.append(top, meta);
  row.append(body);

  if (rep.output_path) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = t("widget.files.reveal");
    button.onclick = () => invoke("reveal_path", { path: rep.output_path });
    row.append(button);
  }
  $("dropOutputs").append(row);
  updateOutputsHeader();
  syncPanelHeight();
}

function appendDropErrors(errors) {
  if (!errors.length) return;
  const lines = errors.map((error) => (error === "отменено" ? t("widget.files.cancelled") : error));
  const prev = $("dropErrors").textContent;
  $("dropErrors").textContent = prev ? `${prev}\n${lines.join("\n")}` : lines.join("\n");
  updateOutputsHeader();
  syncPanelHeight();
}

function renderDropQueue() {
  const list = $("dropQueueList");
  list.replaceChildren();
  if (!dropQueue.length) {
    $("dropQueue").hidden = true;
    syncPanelHeight();
    return;
  }
  $("dropQueueTitle").textContent = t("widget.files.queue_title", { n: dropQueue.length });
  for (const job of dropQueue) {
    const row = document.createElement("div");
    row.className = "drop-queue-item";
    const label = document.createElement("span");
    label.textContent = job.label;
    label.title = job.paths.join("\n");
    const rm = document.createElement("button");
    rm.type = "button";
    rm.textContent = "×";
    rm.onclick = () => {
      dropQueue = dropQueue.filter((item) => item.id !== job.id);
      renderDropQueue();
    };
    row.append(label, rm);
    list.append(row);
  }
  $("dropQueue").hidden = false;
  syncPanelHeight();
}

async function processDropQueue() {
  if (dropBusy) return;
  dropBusy = true;
  updateStatusIndicators();
  try {
    while (dropQueue.length > 0) {
      currentJob = dropQueue.shift();
      renderDropQueue();
      dropStartedAt = Date.now();
      dropProgress = { done: 0, total: 0, pct: 0 };
      resetDropMetrics(dropStartedAt);
      clearInterval(dropTimer);
      dropTimer = setInterval(renderDropProgress, 250);
      $("dropHint").textContent = t("widget.files.drop_more");
      $("dropFill").style.width = "0%";
      $("dropBar").hidden = false;
      renderDropProgress();
      $("dropCancel").disabled = false;
      $("dropCancel").textContent = t("widget.files.cancel");
      $("dropCancel").hidden = false;
      $("dropResult").hidden = false;
      syncPanelHeight();
      try {
        const result = await invoke("quick_drop", {
          paths: currentJob.paths,
          zipMode: currentJob.zipMode,
          deleteAfter: currentJob.deleteAfter,
        });
        for (const rep of result.reports) {
          appendDropReportRow(rep);
        }
        appendDropErrors(result.errors);
      } catch (err) {
        const msg = String(err);
        const text = msg.includes("отменено")
          ? `${currentJob.label}: ${t("widget.files.cancelled")} (${fmtElapsed(Date.now() - dropStartedAt)})`
          : `${currentJob.label}: ${msg}`;
        appendDropErrors([text]);
      }
    }
  } finally {
    currentJob = null;
    dropBusy = false;
    updateStatusIndicators();
    clearInterval(dropTimer);
    $("dropBar").hidden = true;
    $("dropCancel").hidden = true;
    $("dropResult").hidden = true;
    $("dropHint").textContent = t("widget.files.drop_hint");
    renderDropQueue();
    syncPanelHeight();
  }
}

function enqueuePaths(paths) {
  if (!Array.isArray(paths) || !paths.length) return;
  if (!dropBusy && dropQueue.length === 0) {
    $("dropOutputs").replaceChildren();
    $("dropErrors").textContent = "";
    updateOutputsHeader();
  }
  dropQueue.push({
    id: nextJobId++,
    paths,
    zipMode: $("dropZipMode").value,
    deleteAfter: $("dropDeleteAfter").checked,
    label: formatJobLabel(paths),
  });
  renderDropQueue();
  if (!dropBusy) {
    processDropQueue();
  }
}

async function pickAndEnqueue(folders) {
  if (pickingFiles) return;
  pickingFiles = true;
  try {
    const paths = await invoke("pick_quick_paths", { folders });
    enqueuePaths(paths);
  } catch (err) {
    appendDropErrors([String(err)]);
  } finally {
    pickingFiles = false;
  }
}

$("dropQueueClear").onclick = () => {
  dropQueue = [];
  renderDropQueue();
};

$("dropOutputsClear").onclick = () => {
  $("dropOutputs").replaceChildren();
  $("dropErrors").textContent = "";
  updateOutputsHeader();
  syncPanelHeight();
};

$("pickFiles").onclick = (e) => {
  e.stopPropagation();
  pickAndEnqueue(false);
};
$("pickFolders").onclick = (e) => {
  e.stopPropagation();
  pickAndEnqueue(true);
};
$("dropZone").onclick = (e) => {
  if (e.target.closest("button")) return;
  pickAndEnqueue(false);
};

listen("tauri://drag-drop", (e) => {
  dragActive = false;
  $("dropZone").classList.remove("over");
  enqueuePaths(e?.payload?.paths);
});

$("dropCancel").onclick = async () => {
  if (!dropBusy) return;
  $("dropCancel").disabled = true;
  $("dropCancel").textContent = t("widget.files.cancelling");
  await invoke("quick_drop_cancel");
};

(async () => {
  try { await window.I18N?.init(); } catch {}
  $("dropHint").textContent = t("widget.files.drop_hint");
  updatePinButton();
  updateStatusIndicators();
  refreshState();
  try {
    const open = await invoke("pet_open");
    $("petToggle").style.opacity = open ? "1" : "0.45";
  } catch {}
})();

$("petToggle").onclick = async () => {
  const open = await invoke("toggle_pet");
  $("petToggle").style.opacity = open ? "1" : "0.45";
};

listen("pet-state", (e) => {
  $("petToggle").style.opacity = e.payload ? "1" : "0.45";
});
