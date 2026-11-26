const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
const t = (key, params) => (window.I18N ? window.I18N.t(key, params) : key);

let expanded = false;
let hoverTimer = null;
let leaveTimer = null;
let pollTimer = null;
let isAwake = false;
let isPaused = false;
let dragActive = false;
let dropBusy = false;
let lastDropPath = "";
let dropStartedAt = 0;
let dropProgress = { done: 0, total: 0, pct: 0 };
let dropTimer = null;

function updateEdgeDot() {
  const dot = $("dot");
  dot.classList.toggle("paused", isPaused);
  dot.classList.toggle("awake", !isPaused && isAwake);
}

function schedulePoll() {
  clearTimeout(pollTimer);
  pollTimer = setTimeout(refreshState, expanded ? 5000 : 30000);
}

async function setExpanded(next) {
  clearTimeout(hoverTimer);
  clearTimeout(leaveTimer);
  if (expanded === next) return;
  expanded = next;
  $("edgeTab").hidden = expanded;
  $("panel").hidden = !expanded;
  await invoke("widget_resize", { expanded });
  if (expanded) {
    refreshState();
  } else {
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
  if (dragActive || dropBusy) return;
  leaveTimer = setTimeout(() => setExpanded(false), 450);
});
$("collapse").onclick = () => setExpanded(false);

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
    updateEdgeDot();
  } catch {}
  schedulePoll();
}

function fmtBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
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
  if (ms < 1000) return `${ms} ms`;
  const secs = ms / 1000;
  if (secs < 60) return `${secs.toFixed(1)} s`;
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
  return mbps >= 100 ? `${Math.round(mbps)} MB/s` : `${mbps.toFixed(1)} MB/s`;
}

function fmtSpeed(bytes, ms) {
  if (ms < 300 || bytes <= 0) return "";
  return fmtSpeedMbps((bytes / 1048576) / (ms / 1000));
}

function fmtEta(ms) {
  let secs = Math.max(1, Math.round(ms / 1000));
  if (secs >= 15) {
    secs = Math.max(15, Math.round(secs / 5) * 5);
  }
  if (secs < 60) return `~${secs} s`;
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
    $("dropSummary").textContent = parts.join(" • ");
  } else {
    $("dropSummary").textContent = `${t("widget.files.processing")} ${elapsed}`;
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

function syncDropControls() {
  const m = $("dropMode").value;
  $("dropZipMode").hidden = m !== "zip";
  $("dropMaxDim").hidden = m === "zip" || m === "strip" || m === "locks" || m === "delete";
  $("dropDeleteAfterRow").hidden = m !== "zip" && m !== "auto";
  if (m !== "locks" && m !== "delete") $("lockList").hidden = true;
  $("dropZone").classList.toggle("danger", m === "delete");
  if (!dropBusy) {
    $("dropZone").textContent = t(m === "delete" ? "widget.files.drop_delete" : "widget.files.drop_hint");
  }
}

restoreSelect("dropMode", "widgetDropMode", "auto");
restoreSelect("dropMaxDim", "widgetDropMaxDim", "1920");
restoreSelect("dropZipMode", "widgetDropZipMode", "fast");
$("dropDeleteAfter").checked = localStorage.getItem("widgetDropDeleteAfter") === "1";

$("dropMode").onchange = () => {
  localStorage.setItem("widgetDropMode", $("dropMode").value);
  syncDropControls();
};
$("dropMaxDim").onchange = () => {
  localStorage.setItem("widgetDropMaxDim", $("dropMaxDim").value);
};
$("dropZipMode").onchange = () => {
  localStorage.setItem("widgetDropZipMode", $("dropZipMode").value);
};
$("dropDeleteAfter").onchange = () => {
  localStorage.setItem("widgetDropDeleteAfter", $("dropDeleteAfter").checked ? "1" : "0");
};
syncDropControls();

listen("quick-drop-progress", (e) => {
  if (!dropBusy || !e?.payload) return;
  dropProgress = e.payload;
  const now = Date.now();
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

async function checkLocks(paths) {
  $("lockList").innerHTML = "";
  $("lockList").hidden = true;
  $("dropBar").hidden = true;
  $("dropCancel").hidden = true;
  $("dropReveal").hidden = true;
  $("dropResult").hidden = false;
  try {
    const procs = await invoke("who_locks_file", { paths });
    if (!procs.length) {
      $("dropSummary").textContent = t("widget.files.lock_free");
      return;
    }
    $("dropSummary").textContent = `${procs.length}`;
    for (const p of procs) {
      const row = document.createElement("div");
      row.className = "lock-row";
      const label = document.createElement("span");
      label.textContent = `${p.name} (${p.pid})`;
      label.title = p.app_name || p.name;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = t("widget.files.lock_kill");
      btn.onclick = async () => {
        btn.disabled = true;
        try {
          await invoke("kill_pid", { pid: p.pid });
          await checkLocks(paths);
        } catch (err) {
          $("dropSummary").textContent = String(err);
          btn.disabled = false;
        }
      };
      row.append(label, btn);
      $("lockList").appendChild(row);
    }
    $("lockList").hidden = false;
  } catch (err) {
    $("dropSummary").textContent = String(err);
  }
}

function confirmFastDelete(paths) {
  $("lockList").innerHTML = "";
  $("dropBar").hidden = true;
  $("dropCancel").hidden = true;
  $("dropReveal").hidden = true;
  $("dropResult").hidden = false;
  const first = paths[0].split(/[/\\]/).pop() || paths[0];
  $("dropSummary").textContent = paths.length > 1
    ? `${t("widget.files.delete_confirm")} (${first} +${paths.length - 1})`
    : `${t("widget.files.delete_confirm")} (${first})`;

  const row = document.createElement("div");
  row.className = "lock-row";
  const label = document.createElement("span");
  label.textContent = paths.join(", ");
  const delBtn = document.createElement("button");
  delBtn.type = "button";
  delBtn.className = "btn-danger";
  delBtn.textContent = t("widget.files.delete_btn");
  delBtn.onclick = async () => {
    $("lockList").hidden = true;
    dropBusy = true;
    dropStartedAt = Date.now();
    dropProgress = { done: 0, total: 0, pct: 0 };
    resetDropMetrics(dropStartedAt);
    clearInterval(dropTimer);
    dropTimer = setInterval(renderDropProgress, 250);
    $("dropZone").textContent = t("widget.files.processing");
    $("dropFill").style.width = "0%";
    $("dropBar").hidden = false;
    $("dropCancel").disabled = false;
    $("dropCancel").textContent = t("widget.files.cancel");
    $("dropCancel").hidden = false;
    renderDropProgress();
    try {
      const rep = await invoke("fast_delete", { paths });
      const spd = fmtSpeed(rep.input_bytes, rep.elapsed_ms);
      $("dropSummary").textContent = `${t("widget.files.delete_done")}: ${rep.count} f (${fmtBytes(rep.input_bytes)})${spd ? ` • ${spd}` : ""} • ${fmtElapsed(rep.elapsed_ms)}`;
    } catch (err) {
      const msg = String(err);
      $("dropSummary").textContent = msg.includes("отменено")
        ? `${t("widget.files.cancelled")} (${fmtElapsed(Date.now() - dropStartedAt)})`
        : msg;
    } finally {
      dropBusy = false;
      clearInterval(dropTimer);
      $("dropBar").hidden = true;
      $("dropCancel").hidden = true;
      syncDropControls();
    }
  };
  const cancelBtn = document.createElement("button");
  cancelBtn.type = "button";
  cancelBtn.textContent = t("widget.files.cancel");
  cancelBtn.onclick = () => {
    $("lockList").hidden = true;
    $("dropResult").hidden = true;
  };
  row.append(label, delBtn, cancelBtn);
  $("lockList").appendChild(row);
  $("lockList").hidden = false;
}

listen("tauri://drag-drop", async (e) => {
  dragActive = false;
  $("dropZone").classList.remove("over");
  const paths = e?.payload?.paths;
  if (!Array.isArray(paths) || !paths.length || dropBusy) return;
  if ($("dropMode").value === "locks") {
    await checkLocks(paths);
    return;
  }
  if ($("dropMode").value === "delete") {
    confirmFastDelete(paths);
    return;
  }
  $("lockList").hidden = true;
  dropBusy = true;
  dropStartedAt = Date.now();
  dropProgress = { done: 0, total: 0, pct: 0 };
  resetDropMetrics(dropStartedAt);
  clearInterval(dropTimer);
  dropTimer = setInterval(renderDropProgress, 250);
  $("dropZone").textContent = t("widget.files.processing");
  $("dropFill").style.width = "0%";
  $("dropBar").hidden = false;
  renderDropProgress();
  $("dropReveal").hidden = true;
  $("dropCancel").disabled = false;
  $("dropCancel").textContent = t("widget.files.cancel");
  $("dropCancel").hidden = false;
  $("dropResult").hidden = false;
  try {
    const rep = await invoke("quick_drop", {
      paths,
      mode: $("dropMode").value,
      maxDim: Number($("dropMaxDim").value) || 0,
      quality: 82,
      zipMode: $("dropZipMode").value,
      deleteAfter: $("dropDeleteAfter").checked,
    });
    lastDropPath = rep.output_path;
    const pct = rep.input_bytes > 0
      ? Math.round((1 - rep.output_bytes / rep.input_bytes) * 100)
      : 0;
    const delta = rep.kind === "zip-unpack"
      ? `${rep.count} f`
      : `${fmtBytes(rep.input_bytes)} → ${fmtBytes(rep.output_bytes)} (${pct >= 0 ? `-${pct}%` : `+${-pct}%`})`;
    const extra = rep.deleted_original ? ` • ${t("widget.files.deleted_original")}` : "";
    $("dropSummary").textContent = `${delta} • ${fmtElapsed(rep.elapsed_ms)}${extra}`;
    $("dropReveal").hidden = !lastDropPath;
  } catch (err) {
    const msg = String(err);
    $("dropSummary").textContent = msg.includes("отменено")
      ? `${t("widget.files.cancelled")} (${fmtElapsed(Date.now() - dropStartedAt)})`
      : msg;
    $("dropReveal").hidden = true;
  } finally {
    dropBusy = false;
    clearInterval(dropTimer);
    $("dropBar").hidden = true;
    $("dropCancel").hidden = true;
    $("dropZone").textContent = t("widget.files.drop_hint");
  }
});
$("dropCancel").onclick = async () => {
  if (!dropBusy) return;
  $("dropCancel").disabled = true;
  $("dropCancel").textContent = t("widget.files.cancelling");
  await invoke("quick_drop_cancel");
};
$("dropReveal").onclick = () => {
  if (lastDropPath) invoke("reveal_path", { path: lastDropPath });
};

(async () => {
  try { await window.I18N?.init(); } catch {}
  syncDropControls();
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
