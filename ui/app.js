const { invoke } = window.__TAURI__.core;
const t = (key, params) => I18N.t(key, params);

const CATEGORIES = ["work", "communication", "social", "entertainment", "shopping", "study", "games", "other"];
const categoryName = (c) => (CATEGORIES.includes(c) ? t(`cat.${c}`) : c);

// Так программа хранит название скрытого окна (см. ml-core/privacy.rs); показываем его на языке интерфейса.
const HIDDEN_APP = "(скрыто)";
const appName = (a) => (a === HIDDEN_APP ? t("hidden") : a);

let current = new Date();
// Пока открыт «Сегодня», после полуночи окно само переходит на новый день.
let followToday = true;

const $ = (id) => document.getElementById(id);
const iso = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const clock = (ts) => new Date(ts * 1000).toLocaleTimeString(I18N.locale, { hour: "2-digit", minute: "2-digit" });
const pct = (v) => `${Math.round(v)}%`;

function duration(secs) {
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  if (h && m) return `${h} ${t("unit.h")} ${m} ${t("unit.min")}`;
  if (h) return `${h} ${t("unit.h")}`;
  return `${m} ${t("unit.min")}`;
}

function bytes(n) {
  const units = t("unit.bytes").split(",");
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)} ${units[i]}`;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function note(text) {
  return el("div", "muted", text);
}

function bars(target, rows) {
  target.replaceChildren();
  if (!rows.length) return target.append(note(t("common.nodata")));
  const max = Math.max(...rows.map((r) => r.secs), 1);
  for (const r of rows) {
    const row = el("div", "bar");
    const track = el("div", "track");
    const fill = el("div", r.over ? "fill over" : "fill");
    fill.style.width = `${(r.secs / max) * 100}%`;
    track.append(fill);
    row.append(el("div", "", r.label), track, el("div", "muted", r.value ?? duration(r.secs)));
    target.append(row);
  }
}

function list(target, rows, emptyText) {
  target.replaceChildren();
  if (!rows.length) return target.append(note(emptyText));
  for (const text of rows) target.append(el("div", "item", text));
}

function cards(target, items, emptyText) {
  target.replaceChildren();
  if (!items.length) return target.append(note(emptyText));
  for (const [title, value, sub] of items) {
    const c = el("div", "card");
    c.append(el("div", "muted", title), el("div", "v", value), el("div", "muted", sub));
    target.append(c);
  }
}

function renderRhythm(r) {
  if (!r) return "";
  const parts = [t("rhythm.range", { from: clock(r.first_ts), to: clock(r.last_ts) })];
  parts.push(r.breaks.length
    ? t("rhythm.breaks", { n: r.breaks.length, v: duration(r.break_secs) })
    : t("rhythm.nobreaks"));
  parts.push(t("rhythm.switches", { n: r.switches }));
  if (r.longest_focus_secs >= 60) parts.push(t("rhythm.focus", { v: duration(r.longest_focus_secs) }));
  return parts.join(" · ");
}

function loadRange(r) {
  return t("load.range", { min: pct(r.min), max: pct(r.max), time: clock(r.max_ts) });
}

function renderLoad(load) {
  cards($("load"), load ? [
    [t("load.cpu"), t("load.avg", { v: pct(load.cpu.avg) }), loadRange(load.cpu)],
    [t("load.mem"), t("load.avg", { v: pct(load.mem.avg) }), loadRange(load.mem)],
  ] : [], t("load.empty"));
}

function renderNet(net) {
  cards($("net"), net ? [
    [t("net.rx"), bytes(net.rx_total), t("net.rx_sub")],
    [t("net.tx"), bytes(net.tx_total), t("net.tx_sub")],
    [t("net.peak"), net.peak_hour === null ? "—" : `${String(net.peak_hour).padStart(2, "0")}:00`, t("net.peak_sub")],
  ] : [], t("common.nodata"));

  const chart = $("hours");
  chart.replaceChildren();
  if (!net) return;
  const max = Math.max(...net.by_hour.map((h) => h.rx + h.tx), 1);
  for (const h of net.by_hour) {
    const col = el("div", "col");
    col.style.height = `${((h.rx + h.tx) / max) * 100}%`;
    col.title = `${String(h.hour).padStart(2, "0")}:00 — ↓ ${bytes(h.rx)}, ↑ ${bytes(h.tx)}`;
    chart.append(col);
  }
}

function renderHealth(h) {
  const rows = [];
  if (h) {
    rows.push(t("health.uptime", { v: duration(h.uptime_secs) }));
    if (h.disk_free_start !== null && h.disk_free_end !== null) {
      const diff = h.disk_free_end - h.disk_free_start;
      rows.push(t("health.disk", {
        free: h.disk_free_end.toFixed(1),
        diff: `${diff >= 0 ? "+" : ""}${diff.toFixed(1)}`,
        gb: t("unit.gb"),
      }));
    }
    if (h.battery_min !== null && h.battery_min < 100) rows.push(t("health.battery_min", { v: h.battery_min }));
    if (h.on_battery_secs > 0) rows.push(t("health.on_battery", { v: duration(h.on_battery_secs) }));
    if (h.temp_max !== null) rows.push(t("health.temp", { v: Math.round(h.temp_max) }));
  }
  list($("health"), rows, t("common.nodata"));
}

const HISTORY_LIMIT = 300;
let timelineData = [];
let clipData = [];
let allHits = [];
let tab = "windows";
// Выбранная запись буфера (Ctrl+↑/↓ двигают, Enter копирует).
let clipSel = 0;
let clipRows = [];

const tabPlaceholder = (name) => t(`hist.ph.${name}`);
const tabNoteText = (name) => (name === "windows" ? "" : t(`hist.note.${name}`));

const matches = (q, ...fields) => !q || fields.some((f) => f.toLowerCase().includes(q));

function shortDuration(secs) {
  return secs < 60 ? `${secs} ${t("unit.sec")}` : duration(secs);
}

function historyRow(time, text, tail, button) {
  const row = el("div", "item hist");
  const body = el("span", "tl-name", text.length > 300 ? text.slice(0, 300) + "…" : text);
  body.title = text;
  row.append(el("span", "tl-time", time), body);
  row.append(button ?? el("span", "muted", tail ?? ""));
  return row;
}

async function copyText(text, btn) {
  let ok = true;
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    ok = false;
  }
  if (!btn) {
    $("tabNote").textContent = ok ? t("clip.copied_note") : t("clip.copy_failed");
    return;
  }
  btn.textContent = ok ? t("clip.copied") : t("clip.failed");
  setTimeout(() => (btn.textContent = t("clip.copy")), 1500);
}

function deleteButton(text) {
  const btn = el("button", "small", t("clip.delete"));
  btn.title = t("clip.delete_title");
  btn.onclick = async (e) => {
    e.stopPropagation();
    await invoke("delete_clip", { text });
    clipData = clipData.filter((c) => c.text !== text);
    renderHistory();
  };
  return btn;
}

function copyButton(text) {
  const btn = el("button", "small", t("clip.copy"));
  btn.onclick = (e) => { e.stopPropagation(); copyText(text, btn); };
  return btn;
}

function clipHint() {
  return clipRows.length ? t("clip.hint", { n: clipSel + 1, total: clipRows.length }) : tabNoteText("clips");
}

// Одна лента на три режима: окна дня, буфер обмена и поиск по всем дням.
function renderHistory() {
  const q = $("q").value.trim().toLowerCase();
  const target = $("list");
  target.replaceChildren();
  $("clearClips").hidden = true;

  let rows;
  let empty;
  if (tab === "windows") {
    rows = timelineData
      .filter((s) => matches(q, appName(s.app), s.title))
      .map((s) => historyRow(`${clock(s.start)}–${clock(s.end)}`, s.title ? `${appName(s.app)} — ${s.title}` : appName(s.app), shortDuration(s.end - s.start)));
    empty = q ? t("hist.empty.day_q") : t("hist.empty.windows");
  } else if (tab === "clips") {
    clipRows = clipData.filter((c) => matches(q, appName(c.app), c.text));
    clipSel = Math.min(clipSel, Math.max(clipRows.length - 1, 0));
    rows = clipRows.map((c, i) => {
      const buttons = el("span", "btns");
      buttons.append(copyButton(c.text), deleteButton(c.text));
      const row = historyRow(`${clock(c.ts)} · ${appName(c.app)}`, c.text, "", buttons);
      row.classList.toggle("selected", i === clipSel);
      row.onclick = () => { clipSel = i; renderHistory(); };
      return row;
    });
    $("tabNote").textContent = clipHint();
    $("clearClips").hidden = false;
    empty = q ? t("hist.empty.day_q") : t("hist.empty.clips");
  } else {
    const day = (ts) => new Date(ts * 1000).toLocaleDateString(I18N.locale, { day: "numeric", month: "short" });
    rows = allHits.map((h) => historyRow(`${day(h.last_ts)} ${clock(h.last_ts)}`, h.title ? `${appName(h.app)} — ${h.title}` : appName(h.app), ""));
    empty = q ? t("hist.empty.all_q") : t("hist.empty.all");
  }

  if (!rows.length) return target.append(note(empty));
  target.append(...rows.slice(0, HISTORY_LIMIT));
  if (rows.length > HISTORY_LIMIT) target.append(note(t("hist.more", { n: HISTORY_LIMIT, total: rows.length })));
}

function setTab(name) {
  tab = name;
  clipSel = 0;
  for (const b of document.querySelectorAll(".tab")) b.classList.toggle("active", b.dataset.tab === name);
  $("q").placeholder = tabPlaceholder(name);
  $("tabNote").textContent = tabNoteText(name);
  searchAll();
  renderHistory();
}

async function renderWeek() {
  const days = await invoke("week_overview");
  bars($("week"), days.map((d) => ({
    label: new Date(d.date + "T00:00:00").toLocaleDateString(I18N.locale, { weekday: "short", day: "numeric" }),
    secs: d.active_secs,
    value: d.active_secs ? duration(d.active_secs) : "—",
  })));
}

async function render() {
  if (followToday) current = new Date();
  const isToday = iso(current) === iso(new Date());
  $("date").textContent = isToday ? t("nav.today") : current.toLocaleDateString(I18N.locale, { day: "numeric", month: "long" });
  $("next").disabled = isToday;

  const s = await invoke("day_summary", { date: iso(current) });
  $("total").textContent = duration(s.active_secs);
  $("rhythm").textContent = renderRhythm(s.rhythm);

  timelineData = s.timeline;
  clipData = s.clips;
  renderHistory();

  bars($("categories"), s.by_category.map((c) => ({ label: categoryName(c.category), secs: c.secs })));
  bars($("apps"), s.top_apps.map((a) => ({ label: appName(a.app), secs: a.secs })));
  bars($("goals"), s.goals.map((g) => ({
    label: categoryName(g.category),
    secs: Math.min(g.used_mins, g.limit_mins) * 60,
    over: g.used_mins > g.limit_mins,
    value: t("goal.value", { used: g.used_mins, limit: g.limit_mins }),
  })));
  if (!s.goals.length) $("goals").replaceChildren(note(t("goal.example")));

  list($("focus"), s.focus_blocks.map((b) => `${appName(b.app)}: ${clock(b.start)}–${clock(b.end)} (${duration(b.end - b.start)})`),
    t("focus.empty"));
  list($("bookmarks"), s.bookmarks.map((b) => `${clock(b.ts)} · ${appName(b.app)}${b.title ? " — " + b.title : ""}${b.note ? " · " + b.note : ""}`),
    t("bookmarks.empty"));

  renderLoad(s.load);
  renderNet(s.net);
  list($("procs"), s.procs.map((p) => t("proc.line", {
    name: p.name,
    cpu: pct(p.max_cpu),
    cpu_time: clock(p.max_cpu_ts),
    mem: p.max_mem_mb,
    mb: t("unit.mb"),
    mem_time: clock(p.max_mem_ts),
  })), t("common.nodata"));
  renderHealth(s.health);
  list($("downloads"), s.downloads.map((d) => `${clock(d.ts)} · ${d.name} (${bytes(d.size)})`), t("downloads.empty"));
}

async function refreshStatus() {
  const st = await invoke("status");
  const e = $("status");
  const stale = st.last_sample_age_secs === null || st.last_sample_age_secs > 30;
  if (st.paused) {
    e.className = "status bad";
    e.textContent = t("status.paused");
  } else if (stale) {
    e.className = "status bad";
    e.textContent = st.last_sample_age_secs === null ? t("status.none") : t("status.stale", { age: duration(st.last_sample_age_secs) });
  } else {
    e.className = "status ok";
    e.textContent = t("status.ok");
  }
  $("pause").textContent = st.paused ? t("btn.resume") : t("btn.pause");
}

function ask(dialog) {
  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue), { once: true });
    dialog.returnValue = "";
    dialog.showModal();
  });
}

function toolMessage(text) {
  $("toolMsg").textContent = text;
}

$("prev").onclick = () => { followToday = false; current.setDate(current.getDate() - 1); render(); };
$("next").onclick = () => {
  current.setDate(current.getDate() + 1);
  followToday = iso(current) === iso(new Date());
  render();
};

$("mark").onclick = async () => {
  $("noteText").value = "";
  if ((await ask($("noteDlg"))) !== "ok") return;
  await invoke("add_bookmark", { note: $("noteText").value });
  render();
};

$("pause").onclick = async () => {
  const st = await invoke("status");
  await invoke("set_paused", { paused: !st.paused });
  refreshStatus();
};

$("addGoal").onclick = async () => {
  $("goalMin").value = "";
  if ((await ask($("goalDlg"))) !== "ok") return;
  const mins = parseInt($("goalMin").value, 10);
  if (Number.isNaN(mins) || mins < 0) return;
  await invoke("set_goal", { category: $("goalCat").value, limitMins: mins });
  render();
};

$("export").onclick = async () => {
  try {
    toolMessage(t("tools.saved", { path: await invoke("export_day_csv", { date: iso(current) }) }));
  } catch (e) {
    toolMessage(t("tools.save_failed", { err: e }));
  }
};

$("forget").onclick = async () => {
  if (!window.confirm(t("tools.forget_confirm"))) return;
  await invoke("forget_minutes", { minutes: 10 });
  toolMessage(t("tools.forgot"));
  render();
};

$("autostart").onchange = async (e) => {
  try {
    await invoke("set_autostart", { enabled: e.target.checked });
    toolMessage(e.target.checked ? t("tools.autostart_on") : t("tools.autostart_off"));
  } catch (err) {
    e.target.checked = !e.target.checked;
    toolMessage(t("tools.autostart_failed", { err }));
  }
};

let searchTimer;
// Поиск по всем дням идёт в базе, остальные вкладки фильтруют уже загруженный день сразу.
function searchAll() {
  clearTimeout(searchTimer);
  if (tab !== "all") return;
  const q = $("q").value.trim();
  if (!q) {
    allHits = [];
    return renderHistory();
  }
  searchTimer = setTimeout(async () => {
    allHits = await invoke("search", { query: q });
    renderHistory();
  }, 250);
}

$("q").oninput = () => {
  renderHistory();
  searchAll();
};
$("clearClips").onclick = async () => {
  if (!window.confirm(t("clip.clear_confirm"))) return;
  await invoke("clear_clips");
  clipData = [];
  renderHistory();
};

document.addEventListener("keydown", (e) => {
  if (tab !== "clips" || !clipRows.length) return;
  if (e.ctrlKey && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
    e.preventDefault();
    clipSel = Math.max(0, Math.min(clipRows.length - 1, clipSel + (e.key === "ArrowDown" ? 1 : -1)));
    renderHistory();
    document.querySelector("#list .selected")?.scrollIntoView({ block: "nearest" });
  } else if (e.key === "Enter" && e.target.tagName !== "BUTTON" && !document.querySelector("dialog[open]")) {
    e.preventDefault();
    copyText(clipRows[clipSel].text);
  }
});
for (const b of document.querySelectorAll(".tab")) b.onclick = () => setTab(b.dataset.tab);

// Любая ошибка показывается на странице, а не оставляет её пустой.
async function guarded(task) {
  const box = $("errorBox");
  try {
    await task();
    box.hidden = true;
  } catch (e) {
    box.hidden = false;
    box.textContent = t("err.load", { err: typeof e === "string" ? e : JSON.stringify(e) });
  }
}

// Выпадающие списки, текст которых зависит от языка.
function fillLanguageControls() {
  const goalCat = $("goalCat");
  const selected = goalCat.value;
  goalCat.replaceChildren(...CATEGORIES.map((c) => {
    const o = el("option", "", categoryName(c));
    o.value = c;
    return o;
  }));
  if (selected) goalCat.value = selected;

  const lang = $("lang");
  lang.replaceChildren(...I18N.languages.map((l) => {
    const o = el("option", "", l.name);
    o.value = l.code;
    return o;
  }));
  lang.value = I18N.code;
}

$("lang").onchange = async (e) => {
  await I18N.setLanguage(e.target.value);
  fillLanguageControls();
  setTab(tab);
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
};

async function start() {
  await I18N.init();
  fillLanguageControls();
  setTab("windows");
  await guarded(async () => { $("autostart").checked = await invoke("get_autostart"); });
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
}

start();
setInterval(() => { guarded(render); guarded(refreshStatus); }, 15_000);
setInterval(() => guarded(renderWeek), 5 * 60_000);
