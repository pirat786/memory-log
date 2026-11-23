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
  const minutes = Math.round(secs / 60);
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
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
    const row = el("div", r.onClick ? "bar bar-link" : "bar");
    if (r.category) row.dataset.category = r.category;
    if (r.onClick) {
      row.tabIndex = 0;
      row.setAttribute("role", "button");
      row.onclick = r.onClick;
      row.onkeydown = (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          r.onClick();
        }
      };
    }
    const track = el("div", "track");
    const fill = el("div", r.over ? "fill over" : "fill");
    fill.style.width = `${(r.secs / max) * 100}%`;
    track.append(fill);
    row.append(el("div", "", r.label), track, el("div", "muted", r.value ?? duration(r.secs)));
    target.append(row);
  }
}

function openHistoryQuery(query) {
  $("q").value = query;
  setView("history");
  setTab("windows");
  renderHistory();
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

function renderRhythm(r, awaySecs) {
  if (!r) return awaySecs >= 60 ? t("rhythm.away", { v: duration(awaySecs) }) : "";
  const parts = [t("rhythm.range", { from: clock(r.first_ts), to: clock(r.last_ts) })];
  if (awaySecs >= 60) parts.push(t("rhythm.away", { v: duration(awaySecs) }));
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

  const chart = $("loadHours");
  chart.replaceChildren();
  if (!load?.by_hour) return;
  const max = Math.max(...load.by_hour.map((h) => h.cpu), 1);
  for (const h of load.by_hour) {
    const col = el("div", "col");
    col.style.height = `${(h.cpu / max) * 100}%`;
    col.title = `${String(h.hour).padStart(2, "0")}:00 — CPU ${pct(h.cpu)}, RAM ${pct(h.mem)}`;
    chart.append(col);
  }
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
let historyBlocks = [];
let clipData = [];
let allHits = [];
let tab = "windows";
let activeView = "day";
let selectedRange = null;
let trackHit = null;
const viewButtons = document.querySelectorAll("[data-view]");
// Выбранная запись буфера (Ctrl+↑/↓ двигают, Enter копирует).
let clipSel = 0;
let clipRows = [];

const tabPlaceholder = (name) => t(`hist.ph.${name}`);
const tabNoteText = (name) => (name === "windows" ? "" : t(`hist.note.${name}`));

const matches = (q, ...fields) => !q || fields.some((f) => f.toLowerCase().includes(q));

function setView(view) {
  const previousScope = currentDeviceScope();
  if (activeView !== view) window.scrollTo(0, 0);
  activeView = view;
  for (const button of viewButtons) {
    const active = button.dataset.view === view;
    $(button.getAttribute("aria-controls")).hidden = !active;
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  const title = $("pageTitle");
  title.dataset.i18n = `view.${view}`;
  title.textContent = t(title.dataset.i18n);
  $("dateNav").hidden = view === "settings";
  if (syncInfo) updateDeviceControls();
  if (previousScope !== currentDeviceScope()) guarded(render);
  if (view === "system") {
    guarded(refreshLiveSystem);
    if (!$("startupList").childElementCount) guarded(refreshStartup);
    if (!portsData.length) guarded(refreshPorts);
    if (!junkCategories.length) renderJunkList();
    guarded(refreshPowerStatus);
  }
}

function selectRange(range) {
  trackHit = null;
  $("trackSelection").hidden = true;
  selectedRange = range;
  if (range) {
    setView("history");
    $("q").value = "";
    setTab("windows");
  } else {
    renderHistory();
  }
  markTrackSelection();
  if (range) $("pageTitle").focus({ preventScroll: true });
  else $("q").focus();
}

function markTrackSelection() {
  const selection = trackHit ?? selectedRange;
  const track = $("trackSegments");
  for (const segment of track.children) {
    const selected = selection && segment.dataset.category === selection.category &&
      Number(segment.dataset.start) <= selection.start && selection.start < Number(segment.dataset.end);
    segment.setAttribute("aria-pressed", !!selected);
  }
  track.classList.toggle("has-hit", !!trackHit);
  if (trackHit) {
    const from = Number(track.dataset.from);
    const length = Number(track.dataset.to) - from;
    track.style.setProperty("--hit-start", `${(trackHit.start - from) / length * 100}%`);
    track.style.setProperty("--hit-width", `${(trackHit.end - trackHit.start) / length * 100}%`);
  }
}


function setTab(name) {
  tab = name;
  clipSel = 0;
  for (const b of document.querySelectorAll(".tab")) {
    const active = b.dataset.tab === name;
    b.classList.toggle("active", active);
    b.setAttribute("aria-pressed", active);
  }
  const hasFilter = name !== "bookmarks" && name !== "downloads";
  $("historyFilter").hidden = !hasFilter;
  $("windowOrder").hidden = name !== "windows";
  $("bookmarks").hidden = name !== "bookmarks";
  $("downloads").hidden = name !== "downloads";
  if (hasFilter) {
    $("q").placeholder = tabPlaceholder(name);
    $("tabNote").textContent = tabNoteText(name);
  }
  searchAll();
  renderHistory();
}

let weekAvgSecs = 0;
let lastDaySummary = null;
let breakDismissedAt = 0;

function updateDayDelta(activeSecs, scope) {
  const target = $("dayDelta");
  if (!weekAvgSecs || !activeSecs || scope !== "local") {
    target.textContent = "";
    return;
  }
  const diff = Math.round((activeSecs - weekAvgSecs) / 60);
  if (Math.abs(diff) < 5) {
    target.textContent = t("hero.delta_even");
  } else {
    target.textContent = t(diff > 0 ? "hero.delta_up" : "hero.delta_down", { v: duration(Math.abs(diff) * 60) });
  }
}

function updateBreakReminder(summary, isToday, scope) {
  const box = $("breakReminder");
  const limitMins = Number(localStorage.getItem("breakMinutes") || "0");
  if (!limitMins || !isToday || scope !== "local" || !summary?.rhythm) {
    box.hidden = true;
    return;
  }
  const lastBreakEnd = summary.rhythm.breaks.length
    ? summary.rhythm.breaks[summary.rhythm.breaks.length - 1].end
    : summary.rhythm.first_ts;
  const streakSecs = summary.rhythm.last_ts - lastBreakEnd;
  const now = Math.floor(Date.now() / 1000);
  if (streakSecs >= limitMins * 60 && now - breakDismissedAt >= limitMins * 60 && now - summary.rhythm.last_ts < 120) {
    $("breakReminderText").textContent = t("break.prompt", { v: shortDuration(streakSecs) });
    box.hidden = false;
  } else {
    box.hidden = true;
  }
}

async function renderWeek() {
  const days = await invoke("week_overview");
  const recorded = days.filter((d) => d.active_secs > 0);
  weekAvgSecs = recorded.length ? Math.round(recorded.reduce((sum, d) => sum + d.active_secs, 0) / recorded.length) : 0;
  if (lastDaySummary) updateDayDelta(lastDaySummary.active_secs, currentDeviceScope());
  const target = $("week");
  const peak = Math.max(...days.map((d) => d.active_secs), 1);
  const chart = el("div", "week-chart");
  for (const day of days) {
    const date = new Date(day.date + "T12:00:00");
    const button = el("button", "week-day");
    button.type = "button";
    button.dataset.weekDate = day.date;
    if (day.top_category) button.dataset.category = day.top_category;
    button.title = `${date.toLocaleDateString(I18N.locale, { weekday: "long", day: "numeric", month: "long" })} · ${shortDuration(day.active_secs)}`;
    button.setAttribute("aria-label", button.title);
    if (day.date === iso(current)) button.setAttribute("aria-current", "date");
    const track = el("span", "week-track");
    const fill = el("span", "week-fill");
    fill.style.height = `${day.active_secs / peak * 100}%`;
    track.append(fill);
    button.append(track, el("span", "week-label", date.toLocaleDateString(I18N.locale, { weekday: "short" })));
    button.onclick = () => goToDate(date);
    chart.append(button);
  }
  target.replaceChildren(days.length ? chart : note(t("common.nodata")));
}

let renderRequest = 0;
let renderedDate;

async function render() {
  const request = ++renderRequest;
  if (followToday) current = new Date();
  const date = iso(current);
  const isToday = date === iso(new Date());
  $("date").textContent = isToday ? t("nav.today") : current.toLocaleDateString(I18N.locale, { day: "numeric", month: "long" });
  $("next").disabled = isToday;
  for (const button of $("week").querySelectorAll("[data-week-date]")) {
    if (button.dataset.weekDate === date) button.setAttribute("aria-current", "date");
    else button.removeAttribute("aria-current");
  }

  const scope = currentDeviceScope();
  const { summary: s, devices } = await loadDeviceDay(date, scope);
  if (request !== renderRequest) return;
  const dayChanged = renderedDate !== date || renderedDeviceScope !== scope;
  if (dayChanged) {
    selectedRange = null;
    trackHit = null;
    $("trackSelection").hidden = true;
  }
  renderedDate = date;
  renderedDeviceScope = scope;
  lastDaySummary = s;
  $("total").textContent = s.active_secs ? shortDuration(s.active_secs) : duration(0);
  $("rhythm").textContent = renderRhythm(s.rhythm, s.away_secs);
  updateDayDelta(s.active_secs, scope);
  updateBreakReminder(s, isToday, scope);

  timelineData = s.timeline;
  historyBlocks = s.history_blocks;
  clipData = s.clips;
  renderDeviceOverview(scope, devices);
  if (scope !== "all") renderDayTrack(s, dayChanged);
  renderHistory();

  bars($("categories"), s.by_category.map((c) => ({ category: c.category, label: categoryName(c.category), secs: c.secs })));
  bars($("apps"), s.top_apps.map((a) => ({ label: appName(a.app), secs: a.secs, onClick: () => openHistoryQuery(appName(a.app)) })));
  bars($("sites"), s.sites.map((x) => ({
    label: x.site || t("sites.other"),
    secs: x.secs,
    onClick: x.site ? () => openHistoryQuery(x.site) : null,
  })));
  bars($("goals"), s.goals.map((g) => ({
    label: categoryName(g.category),
    secs: Math.min(g.used_mins, g.limit_mins) * 60,
    over: g.used_mins > g.limit_mins,
    value: t("goal.value", { used: g.used_mins, limit: g.limit_mins }),
  })));
  if (!s.goals.length) $("goals").replaceChildren(note(t("goal.example")));

  list($("focus"), s.focus_blocks.map((b) => `${appName(b.app)}: ${clock(b.start)}–${clock(b.end)} (${duration(b.end - b.start)})`),
    t("focus.empty"));
  const bookmarks = $("bookmarks");
  const bookmarkRows = s.bookmarks.map((b) => {
    const row = el("div", "item", bookmarkLabel(b));
    row.tabIndex = -1;
    return row;
  });
  bookmarks.replaceChildren(...(bookmarkRows.length ? bookmarkRows : [note(t("bookmarks.empty"))]));

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
  } else if (st.inactive) {
    e.className = "status inactive";
    e.textContent = t("status.inactive");
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

function goToDate(date) {
  current = new Date(date);
  followToday = iso(current) === iso(new Date());
  return guarded(render);
}

async function openSearchDay(hit) {
  const date = new Date(hit.last_ts * 1000);
  if (hit.device) setDeviceScope(hit.device, false);
  setView("day");
  await goToDate(date);
  if (renderedDate !== iso(date) || renderedDeviceScope !== currentDeviceScope() || activeView !== "day") return;
  const matching = timelineData.filter((s) => s.app === hit.app && s.title === hit.title);
  trackHit = matching.find((s) => s.start <= hit.last_ts && hit.last_ts < s.end) ?? matching[0] ?? null;
  selectedRange = null;
  markTrackSelection();
  const label = [hit.device && deviceName(hit.device), appName(hit.app), hit.title].filter(Boolean).join(" — ");
  $("trackSelectionLabel").textContent = trackHit
    ? `${clock(trackHit.start)}–${clock(trackHit.end)} · ${label}`
    : t("search.no_window", { title: label });
  $("trackSelection").hidden = false;
  $("openTrackWindow").hidden = !trackHit;
  $("pageTitle").focus({ preventScroll: true });
}

function shiftDate(days) {
  const date = new Date(current);
  date.setDate(date.getDate() + days);
  goToDate(date);
}

$("prev").onclick = () => shiftDate(-1);
$("next").onclick = () => shiftDate(1);

$("mark").onclick = async () => {
  $("noteText").value = "";
  if ((await ask($("noteDlg"))) !== "ok") return;
  await invoke("add_bookmark", { note: $("noteText").value });
  render();
};

$("pause").onclick = () => guarded(async () => {
  const st = await invoke("status");
  await invoke("set_paused", { paused: !st.paused });
  await refreshStatus();
  await render();
});

$("clearRange").onclick = () => selectRange(null);
$("openTrackWindow").onclick = () => { if (trackHit) selectRange(trackHit); };
for (const button of viewButtons) {
  button.onclick = () => setView(button.dataset.view);
}

$("addGoal").onclick = async () => {
  $("goalMin").value = "";
  if ((await ask($("goalDlg"))) !== "ok") return;
  const mins = parseInt($("goalMin").value, 10);
  if (Number.isNaN(mins) || mins < 0) return;
  await invoke("set_goal", { category: $("goalCat").value, limitMins: mins });
  await Promise.all([render(), renderComparison()]);
};

$("export").onclick = async () => {
  const message = $("exportMsg");
  try {
    message.textContent = t("tools.saved", { path: await invoke("export_day_csv", { date: iso(current) }) });
  } catch (e) {
    message.textContent = t("tools.save_failed", { err: e });
  }
};

$("forget").onclick = async () => {
  if (!window.confirm(t("tools.forget_confirm"))) return;
  await invoke("forget_minutes", { minutes: 10 });
  toolMessage(t("tools.forgot"));
  await Promise.all([render(), renderWeek(), renderComparison()]);
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

$("idleSettings").onsubmit = async (event) => {
  event.preventDefault();
  const save = $("saveIdle");
  save.disabled = true;
  try {
    await invoke("set_idle_minutes", { minutes: $("idleMinutes").valueAsNumber });
    toolMessage(t("idle.saved"));
  } catch (err) {
    toolMessage(t("idle.error", { err }));
  } finally {
    save.disabled = false;
  }
};

let searchTimer;
let searchRequest = 0;
// Поиск по всем дням идёт в базе, остальные вкладки фильтруют уже загруженный день сразу.
function searchAll() {
  clearTimeout(searchTimer);
  const request = ++searchRequest;
  if (tab !== "all") return;
  const q = $("q").value.trim();
  if (!q) {
    allHits = [];
    return renderHistory();
  }
  searchTimer = setTimeout(() => guarded(async () => {
    const hits = await searchForDevice(q, currentDeviceScope());
    if (request !== searchRequest || tab !== "all") return;
    allHits = hits;
    renderHistory();
  }), 250);
}

$("q").oninput = () => {
  renderHistory();
  searchAll();
};
$("windowOrder").onchange = renderHistory;
$("clearClips").onclick = async () => {
  if (!window.confirm(t("clip.clear_confirm"))) return;
  await invoke("clear_clips");
  clipData = [];
  renderHistory();
};

document.addEventListener("keydown", (e) => {
  if (activeView !== "history" || tab !== "clips" || !clipRows.length) return;
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
  syncTheme();
  renderUpdateStatus();
  if (syncInfo) await guarded(refreshSync);
  setTab(tab);
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
  await Promise.all([renderComparison(), guarded(renderCategoryRules)]);
};

const TOGGLEABLE_SECTIONS = [
  ["focus", "sec.focus"],
  ["sites", "sec.sites"],
  ["goals", "sec.goals"],
  ["comparison", "compare.title"],
  ["live_procs", "system.live_procs"],
  ["junk", "system.junk_title"],
  ["startup", "system.startup_title"],
  ["ports", "system.ports_title"],
  ["power", "system.power_title"],
];

function applyHiddenSections(hiddenSet) {
  for (const [id] of TOGGLEABLE_SECTIONS) {
    const elSection = document.querySelector(`[data-section="${id}"]`);
    if (elSection) elSection.classList.toggle("section-hidden", hiddenSet.has(id));
  }
}

function initSectionToggles() {
  let saved = [];
  try { saved = JSON.parse(localStorage.getItem("hiddenSections") || "[]"); } catch {}
  const hiddenSet = new Set(Array.isArray(saved) ? saved : []);
  applyHiddenSections(hiddenSet);
  $("sectionToggles").replaceChildren(...TOGGLEABLE_SECTIONS.map(([id, key]) => {
    const row = el("label", "section-toggle");
    const check = el("input");
    check.type = "checkbox";
    check.checked = !hiddenSet.has(id);
    check.onchange = () => {
      if (check.checked) hiddenSet.delete(id);
      else hiddenSet.add(id);
      localStorage.setItem("hiddenSections", JSON.stringify([...hiddenSet]));
      applyHiddenSections(hiddenSet);
    };
    const label = el("span", "", t(key));
    label.dataset.i18n = key;
    row.append(check, label);
    return row;
  }));
}

async function start() {
  await I18N.init();
  fillLanguageControls();
  initExtraControls();
  initSectionToggles();
  initSyncControls();
  const breakSelect = $("breakMinutes");
  breakSelect.value = localStorage.getItem("breakMinutes") || "0";
  breakSelect.onchange = () => {
    localStorage.setItem("breakMinutes", breakSelect.value);
    breakDismissedAt = 0;
    if (lastDaySummary) updateBreakReminder(lastDaySummary, iso(current) === iso(new Date()), currentDeviceScope());
  };
  $("dismissBreak").onclick = () => {
    breakDismissedAt = Math.floor(Date.now() / 1000);
    $("breakReminder").hidden = true;
  };
  $("procSearch").oninput = renderLiveProcList;
  $("procSort").onchange = renderLiveProcList;
  $("procRefresh").onclick = () => guarded(refreshLiveSystem);
  $("scanJunk").onclick = () => guarded(runJunkScan);
  $("cleanJunk").onclick = () => guarded(runJunkClean);
  $("refreshStartup").onclick = () => guarded(refreshStartup);
  $("refreshPorts").onclick = () => guarded(refreshPorts);
  $("portSearch").oninput = renderPortsList;
  $("keepAwake").onchange = async () => {
    await invoke("set_keep_awake", { enabled: $("keepAwake").checked });
    await guarded(refreshPowerStatus);
  };
  $("powerApply").onclick = async () => {
    await invoke("set_power_rule", {
      condition: $("powerCondition").value,
      action: $("powerAction").value,
      delayMins: Number($("powerDelay").value),
    });
    await guarded(refreshPowerStatus);
  };
  $("powerCancel").onclick = async () => {
    await invoke("cancel_power_rule");
    await guarded(refreshPowerStatus);
  };
  guarded(async () => {
    const open = await invoke("widget_open");
    if (!open && localStorage.getItem("widgetEnabled") === "1") {
      localStorage.removeItem("widgetEnabled");
      $("widgetToggle").checked = await invoke("toggle_widget");
    } else {
      $("widgetToggle").checked = open;
    }
  });
  $("widgetToggle").onchange = async () => {
    const open = await invoke("toggle_widget");
    $("widgetToggle").checked = open;
    localStorage.removeItem("widgetEnabled");
  };
  window.__TAURI__?.event?.listen("widget-state", (e) => {
    $("widgetToggle").checked = Boolean(e.payload);
  });
  setInterval(() => {
    if (activeView === "system" && !document.hidden) {
      guarded(refreshLiveSystem);
      guarded(refreshPowerStatus);
    }
  }, 3000);
  void initAppUpdates();
  setTab("windows");
  await guarded(async () => { $("autostart").checked = await invoke("get_autostart"); });
  await guarded(async () => {
    $("idleMinutes").value = await invoke("get_idle_minutes");
    $("idleMinutes").disabled = false;
    $("saveIdle").disabled = false;
  });
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
  await guarded(refreshSync);
  void Promise.all([guarded(loadRecordingSettings), guarded(renderCategoryRules), renderComparison()]);
}

start();
function refreshVisible() {
  if (document.hidden) return;
  guarded(render);
  guarded(refreshStatus);
  if (activeView !== "settings") guarded(refreshSync);
}
document.addEventListener("visibilitychange", refreshVisible);
setInterval(refreshVisible, 15_000);
setInterval(() => {
  if (activeView === "settings" && !document.hidden && (syncInfo?.busy || syncInfo?.sign_in)) {
    guarded(refreshSync);
  }
}, 1000);
setInterval(() => {
  if (!document.hidden) {
    guarded(renderWeek);
    renderComparison();
  }
}, 5 * 60_000);

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    for (const dlg of document.querySelectorAll("dialog[open]")) dlg.close();
    return;
  }
  const inInput = ["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement?.tagName);
  if (e.ctrlKey && !e.altKey && !e.shiftKey) {
    const viewByNum = { "1": "day", "2": "history", "3": "system", "4": "settings" };
    if (viewByNum[e.key]) {
      e.preventDefault();
      setView(viewByNum[e.key]);
      return;
    }
    if (e.key.toLowerCase() === "f") {
      e.preventDefault();
      setView("history");
      setTimeout(() => $("q").focus(), 20);
      return;
    }
  }
  if (e.altKey && !e.ctrlKey && !inInput) {
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      $("prev").click();
    } else if (e.key === "ArrowRight" && !$("next").disabled) {
      e.preventDefault();
      $("next").click();
    }
  }
});
