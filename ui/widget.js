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

let timerSecs = 25 * 60;
let timerInit = 25 * 60;
let timerRunning = false;
let timerEndAt = null;
let audioCtx = null;

function updateEdgeDot() {
  const dot = $("dot");
  dot.classList.toggle("paused", isPaused);
  dot.classList.toggle("awake", !isPaused && isAwake);
  dot.classList.toggle("timer", !isPaused && !isAwake && timerRunning);
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
  if (dragActive || dropBusy || document.activeElement === $("timerMins")) return;
  leaveTimer = setTimeout(() => setExpanded(false), 450);
});
$("collapse").onclick = () => setExpanded(false);

listen("widget-toggle", () => setExpanded(!expanded));
listen("widget-expand", () => setExpanded(true));
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") setExpanded(false);
});

function switchTab(id) {
  for (const b of document.querySelectorAll(".seg button")) b.classList.toggle("active", b.dataset.wt === id);
  for (const paneId of ["timer", "files", "ctrl"]) $("pane-" + paneId).hidden = paneId !== id;
}

for (const btn of document.querySelectorAll(".seg button")) {
  btn.onclick = () => switchTab(btn.dataset.wt);
}

$("timerSound").value = localStorage.getItem("timerSound") || "bell";

function getAudioContext() {
  const Ctx = window.AudioContext || window.webkitAudioContext;
  if (!Ctx) return null;
  if (!audioCtx || audioCtx.state === "closed") audioCtx = new Ctx();
  if (audioCtx.state === "suspended") audioCtx.resume().catch(() => {});
  return audioCtx;
}

function playTimerSound(kind = $("timerSound").value) {
  if (kind === "none") return;
  try {
    const ctx = getAudioContext();
    if (!ctx) return;
    const tone = (freq, start, dur, type = "sine", gainVal = 0.22) => {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = type;
      osc.frequency.setValueAtTime(freq, ctx.currentTime + start);
      gain.gain.setValueAtTime(0.001, ctx.currentTime + start);
      gain.gain.exponentialRampToValueAtTime(gainVal, ctx.currentTime + start + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + start + dur);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(ctx.currentTime + start);
      osc.stop(ctx.currentTime + start + dur + 0.02);
    };
    if (kind === "bell") {
      tone(880, 0, 0.35, "sine", 0.2);
      tone(1320, 0.14, 0.45, "sine", 0.18);
    } else if (kind === "beep") {
      tone(740, 0, 0.14, "triangle", 0.22);
      tone(740, 0.2, 0.14, "triangle", 0.22);
    } else if (kind === "soft") {
      tone(528, 0, 0.65, "sine", 0.18);
    }
  } catch {}
}

$("timerSound").onchange = () => {
  localStorage.setItem("timerSound", $("timerSound").value);
  playTimerSound($("timerSound").value);
};

function drawTimer() {
  const m = String(Math.floor(timerSecs / 60)).padStart(2, "0");
  const s = String(timerSecs % 60).padStart(2, "0");
  $("timerClock").textContent = `${m}:${s}`;
  $("timerToggle").textContent = t(timerRunning ? "widget.timer.pause" : "widget.timer.start");
  $("timerToggle").classList.toggle("on", timerRunning);
  updateEdgeDot();
}

setInterval(() => {
  if (!timerRunning || timerEndAt === null) return;
  const left = Math.max(0, Math.ceil((timerEndAt - Date.now()) / 1000));
  if (left > 0) {
    if (left !== timerSecs) {
      timerSecs = left;
      drawTimer();
    }
  } else {
    timerSecs = 0;
    timerRunning = false;
    timerEndAt = null;
    drawTimer();
    playTimerSound();
    setExpanded(true);
    document.querySelector('[data-wt="timer"]').click();
    $("timerClock").textContent = t("widget.timer.done");
  }
}, 250);

for (const btn of document.querySelectorAll("[data-mins]")) {
  btn.onclick = () => {
    const mins = Number(btn.dataset.mins);
    $("timerMins").value = String(mins);
    timerInit = mins * 60;
    timerSecs = timerInit;
    timerRunning = true;
    timerEndAt = Date.now() + timerSecs * 1000;
    drawTimer();
  };
}
$("timerMins").onchange = () => {
  const mins = Math.max(1, Math.min(180, Math.round(Number($("timerMins").value) || 1)));
  $("timerMins").value = String(mins);
  timerInit = mins * 60;
  timerSecs = timerInit;
  timerRunning = false;
  timerEndAt = null;
  drawTimer();
};
$("timerToggle").onclick = () => {
  if (timerSecs === 0) timerSecs = timerInit;
  timerRunning = !timerRunning;
  timerEndAt = timerRunning ? Date.now() + timerSecs * 1000 : null;
  drawTimer();
};
$("timerReset").onclick = () => {
  timerRunning = false;
  timerEndAt = null;
  timerSecs = timerInit;
  drawTimer();
};

async function refreshState() {
  try {
    const [power, status] = await Promise.all([invoke("power_status"), invoke("status")]);
    isAwake = Boolean(power.keep_awake);
    isPaused = Boolean(status.paused);
    $("btnAwake").classList.toggle("on", isAwake);
    $("btnPause").classList.toggle("on", isPaused);
    $("btnPause").textContent = t(isPaused ? "widget.ctrl.resume" : "widget.ctrl.pause");
    updateEdgeDot();
  } catch {}
  schedulePoll();
}

$("btnAwake").onclick = async () => {
  await invoke("set_keep_awake", { enabled: !$("btnAwake").classList.contains("on") });
  await refreshState();
};
$("btnPause").onclick = async () => {
  await invoke("set_paused", { paused: !$("btnPause").classList.contains("on") });
  await refreshState();
};
$("btnPass").onclick = async () => {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%&*";
  const arr = new Uint32Array(16);
  crypto.getRandomValues(arr);
  const pwd = Array.from(arr, (n) => chars[n % chars.length]).join("");
  await navigator.clipboard.writeText(pwd);
  showCtrlStatus(t("widget.ctrl.copied"));
};
$("btnOpen").onclick = () => invoke("open_main_window");

let ctrlStatusTimer = null;
function showCtrlStatus(msg) {
  $("ctrlStatus").textContent = msg;
  clearTimeout(ctrlStatusTimer);
  ctrlStatusTimer = setTimeout(() => { $("ctrlStatus").textContent = ""; }, 2200);
}

async function runCtrlAction(cmd, okKey, args) {
  try {
    await invoke(cmd, args);
    if (okKey) showCtrlStatus(t(okKey));
  } catch (err) {
    showCtrlStatus(String(err));
  }
}
$("btnLayout").onclick = () => runCtrlAction("clipboard_fix_layout", "widget.ctrl.layout_done");
$("btnExplorer").onclick = () => runCtrlAction("restart_explorer", "widget.ctrl.explorer_done");
$("btnDns").onclick = () => runCtrlAction("flush_dns", "widget.ctrl.dns_done");
for (const btn of document.querySelectorAll("[data-qpath]")) {
  btn.onclick = () => runCtrlAction("open_quick_path", null, { kind: btn.dataset.qpath });
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
  switchTab("files");
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

function fmtSpeed(bytes, ms) {
  if (ms < 300 || bytes <= 0) return "";
  const mbps = (bytes / 1048576) / (ms / 1000);
  return mbps >= 100 ? `${Math.round(mbps)} MB/s` : `${mbps.toFixed(1)} MB/s`;
}

function renderDropProgress() {
  if (!dropBusy) return;
  const elapsedMs = Date.now() - dropStartedAt;
  const elapsed = fmtElapsed(elapsedMs);
  const { done, total, pct } = dropProgress;
  $("dropFill").style.width = `${pct}%`;
  if (total > 0) {
    const spd = fmtSpeed(done, elapsedMs);
    let eta = "";
    if (done > 0 && done < total && elapsedMs >= 500) {
      const etaMs = Math.round(((total - done) / done) * elapsedMs);
      eta = `~${fmtElapsed(etaMs)}`;
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
  if (m !== "locks" && m !== "delete") $("lockList").hidden = true;
  $("dropZone").classList.toggle("danger", m === "delete");
  if (!dropBusy) {
    $("dropZone").textContent = t(m === "delete" ? "widget.files.drop_delete" : "widget.files.drop_hint");
  }
}

restoreSelect("dropMode", "widgetDropMode", "auto");
restoreSelect("dropMaxDim", "widgetDropMaxDim", "1920");
restoreSelect("dropZipMode", "widgetDropZipMode", "fast");

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
syncDropControls();

listen("quick-drop-progress", (e) => {
  if (!dropBusy || !e?.payload) return;
  dropProgress = e.payload;
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
    });
    lastDropPath = rep.output_path;
    const pct = rep.input_bytes > 0
      ? Math.round((1 - rep.output_bytes / rep.input_bytes) * 100)
      : 0;
    const delta = rep.kind === "zip-unpack"
      ? `${rep.count} f`
      : `${fmtBytes(rep.input_bytes)} → ${fmtBytes(rep.output_bytes)} (${pct >= 0 ? `-${pct}%` : `+${-pct}%`})`;
    $("dropSummary").textContent = `${delta} • ${fmtElapsed(rep.elapsed_ms)}`;
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
  drawTimer();
  refreshState();
})();
