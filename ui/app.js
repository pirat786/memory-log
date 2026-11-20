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
    const row = el("div", "bar");
    if (r.category) row.dataset.category = r.category;
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
const viewButtons = document.querySelectorAll("[data-view]");
// Выбранная запись буфера (Ctrl+↑/↓ двигают, Enter копирует).
let clipSel = 0;
let clipRows = [];

const tabPlaceholder = (name) => t(`hist.ph.${name}`);
const tabNoteText = (name) => (name === "windows" ? "" : t(`hist.note.${name}`));

const matches = (q, ...fields) => !q || fields.some((f) => f.toLowerCase().includes(q));

function setView(view) {
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
}

function selectRange(range) {
  selectedRange = range;
  if (range) {
    setView("history");
    $("q").value = "";
    setTab("windows");
  } else {
    renderHistory();
  }
  for (const segment of $("trackSegments").children) {
    segment.setAttribute("aria-pressed", segment.dataset.start === String(range?.start));
  }
  if (range) $("pageTitle").focus({ preventScroll: true });
  else $("q").focus();
}

function setTrackPosition(node, start, end, from, to) {
  node.style.insetInlineStart = `${(start - from) / (to - from) * 100}%`;
  if (end !== null) node.style.inlineSize = `${(end - start) / (to - from) * 100}%`;
}

function bookmarkLabel(b) {
  return [clock(b.ts), appName(b.app), b.title, b.note].filter(Boolean).join(" · ");
}

function renderDayTrack(summary, dayChanged) {
  const { day_start: from, day_end: to, timeline, bookmarks } = summary;
  const track = $("trackSegments");
  const markers = $("trackBookmarks");
  // Соседние окна одной категории образуют один отрезок, промежутки остаются видимыми.
  const ranges = [];
  for (let i = timeline.length - 1; i >= 0; i--) {
    const { start, end, category } = timeline[i];
    const previous = ranges.at(-1);
    if (previous?.end === start && previous.category === category) previous.end = end;
    else ranges.push({ start, end, category });
  }

  const axes = $("trackHours");
  axes.replaceChildren();
  for (const hour of [0, 6, 12, 18, 24]) {
    const date = new Date(from * 1000);
    date.setHours(hour, 0, 0, 0);
    const tick = el("span", "", `${String(hour).padStart(2, "0")}:00`);
    tick.dataset.edge = hour === 0 ? "start" : hour === 24 ? "end" : "";
    setTrackPosition(tick, date.getTime() / 1000, null, from, to);
    axes.append(tick);
  }

  const used = new Set(ranges.map((range) => range.category));
  const legend = $("trackLegend");
  legend.replaceChildren();
  for (const category of CATEGORIES.filter((category) => used.has(category))) {
    const item = el("span", "legend-item", categoryName(category));
    item.dataset.category = category;
    legend.append(item);
  }
  legend.append(el("span", "legend-item", t("track.gap")));
  if (bookmarks.length) legend.append(el("span", "legend-item legend-bookmark", t("sec.bookmarks")));

  if (!dayChanged && $("dayTrack").contains(document.activeElement)) return;
  track.replaceChildren();
  markers.replaceChildren();
  for (const [index, range] of ranges.entries()) {
    const { start, end, category } = range;
    const btn = el("button", "track-segment");
    btn.type = "button";
    btn.dataset.category = category;
    btn.dataset.start = start;
    btn.title = t("track.range", { start: clock(start), end: clock(end), category: categoryName(category) });
    btn.setAttribute("aria-label", btn.title);
    btn.setAttribute("aria-pressed", start === selectedRange?.start);
    btn.tabIndex = index === 0 ? 0 : -1;
    btn.onclick = () => selectRange(range);
    setTrackPosition(btn, start, end, from, to);
    track.append(btn);
  }
  for (const [index, bookmark] of bookmarks.entries()) {
    const btn = el("button", "track-bookmark");
    btn.type = "button";
    btn.title = bookmarkLabel(bookmark);
    btn.setAttribute("aria-label", t("track.bookmark", { label: btn.title }));
    btn.onclick = () => {
      setView("history");
      setTab("bookmarks");
      $("bookmarks").children[index]?.focus();
    };
    setTrackPosition(btn, bookmark.ts, null, from, to);
    markers.append(btn);
  }
}

$("trackSegments").onkeydown = (event) => {
  const track = $("trackSegments");
  const btn = event.target;
  const next = {
    ArrowLeft: btn.previousElementSibling,
    ArrowRight: btn.nextElementSibling,
    Home: track.firstElementChild,
    End: track.lastElementChild,
  }[event.key];
  if (!next) return;
  event.preventDefault();
  btn.tabIndex = -1;
  next.tabIndex = 0;
  next.focus();
};

function shortDuration(secs) {
  return secs < 60 ? `${secs} ${t("unit.sec")}` : duration(secs);
}

function historyRow(time, text, tail, button, tag = "div") {
  const row = el(tag, "item hist");
  const body = el("span", "tl-name", text.length > 300 ? text.slice(0, 300) + "…" : text);
  body.title = text;
  row.append(el("span", "tl-time", time), body);
  row.append(button ?? el("span", "muted", tail ?? ""));
  return row;
}

function windowRow(s) {
  const app = appName(s.app);
  const row = historyRow(`${clock(s.start)}–${clock(s.end)}`, s.title ? `${app} — ${s.title}` : app, shortDuration(s.end - s.start));
  row.dataset.category = s.category;
  return row;
}

const BREAK_ROW_SECS = 5 * 60;

function timelineWithMarks(openBlocks) {
  const out = [];
  let lastHour = null;
  historyBlocks.forEach((b, i) => {
    const hour = new Date(b.start * 1000).getHours();
    if (hour !== lastHour) {
      out.push(el("div", "hour-mark", `${String(hour).padStart(2, "0")}:00`));
      lastHour = hour;
    }
    out.push(blockRow(b, openBlocks.has(String(b.start))));
    const older = historyBlocks[i + 1];
    if (older && b.start - older.end >= BREAK_ROW_SECS) {
      out.push(el("div", "break-row", t("hist.break", { v: duration(b.start - older.end) })));
    }
  });
  return out;
}

// Один блок ленты: программа, главное окно и «ещё N»; по щелчку раскрывается список всех окон блока.
function blockRow(b, open) {
  const main = b.titles[0]?.title ?? "";
  let text = main ? `${appName(b.app)} — ${main}` : appName(b.app);
  if (b.title_count > 1) text += ` ${t("hist.more_titles", { n: b.title_count - 1 })}`;
  const expandable = b.title_count > 1;
  const row = historyRow(`${clock(b.start)}–${clock(b.end)}`, text, shortDuration(b.active_secs), null, expandable ? "summary" : "div");
  row.dataset.category = b.category;
  row.title = categoryName(b.category);
  if (!expandable) return row;

  const titles = el("div", "block-titles");
  for (const x of b.titles) titles.append(el("div", "detail", `${x.title || appName(b.app)} · ${shortDuration(x.secs)}`));
  if (b.title_count > b.titles.length) titles.append(note(t("hist.more_hidden", { n: b.title_count - b.titles.length })));
  const block = el("details", "history-block");
  block.dataset.start = b.start;
  block.open = open;
  block.append(row, titles);
  return block;
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
  if (tab === "bookmarks" || tab === "downloads") return;
  const q = $("q").value.trim().toLowerCase();
  const target = $("list");
  const openBlocks = new Set([...target.querySelectorAll("details[open]")].map((block) => block.dataset.start));
  target.replaceChildren();
  $("clearClips").hidden = true;
  const range = tab === "windows" ? selectedRange : null;
  $("historyRange").hidden = !range;
  if (range) {
    $("rangeLabel").textContent = t("track.range", {
      start: clock(range.start), end: clock(range.end), category: categoryName(range.category),
    });
  }

  let rows;
  let empty;
  if (tab === "windows") {
    rows = range || q || !historyBlocks.length
      ? timelineData
        .filter((s) => !range || (s.start < range.end && s.end > range.start))
        .filter((s) => matches(q, appName(s.app), s.title))
        .map(windowRow)
      : timelineWithMarks(openBlocks);
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
  for (const b of document.querySelectorAll(".tab")) {
    const active = b.dataset.tab === name;
    b.classList.toggle("active", active);
    b.setAttribute("aria-pressed", active);
  }
  const hasFilter = name !== "bookmarks" && name !== "downloads";
  $("historyFilter").hidden = !hasFilter;
  $("bookmarks").hidden = name !== "bookmarks";
  $("downloads").hidden = name !== "downloads";
  if (hasFilter) {
    $("q").placeholder = tabPlaceholder(name);
    $("tabNote").textContent = tabNoteText(name);
  }
  searchAll();
  renderHistory();
}

async function renderWeek() {
  const days = await invoke("week_overview");
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

  const s = await invoke("day_summary", { date });
  if (request !== renderRequest) return;
  const dayChanged = renderedDate !== date;
  if (dayChanged) selectedRange = null;
  renderedDate = date;
  $("total").textContent = s.active_secs ? shortDuration(s.active_secs) : duration(0);
  $("rhythm").textContent = renderRhythm(s.rhythm, s.away_secs);

  timelineData = s.timeline;
  historyBlocks = s.history_blocks;
  clipData = s.clips;
  renderDayTrack(s, dayChanged);
  renderHistory();

  bars($("categories"), s.by_category.map((c) => ({ category: c.category, label: categoryName(c.category), secs: c.secs })));
  bars($("apps"), s.top_apps.map((a) => ({ label: appName(a.app), secs: a.secs })));
  bars($("sites"), s.sites.map((x) => ({ label: x.site || t("sites.other"), secs: x.secs })));
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
  guarded(render);
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
for (const button of viewButtons) {
  button.onclick = () => setView(button.dataset.view);
}

$("addGoal").onclick = async () => {
  $("goalMin").value = "";
  if ((await ask($("goalDlg"))) !== "ok") return;
  const mins = parseInt($("goalMin").value, 10);
  if (Number.isNaN(mins) || mins < 0) return;
  await invoke("set_goal", { category: $("goalCat").value, limitMins: mins });
  render();
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
  await guarded(async () => {
    $("idleMinutes").value = await invoke("get_idle_minutes");
    $("idleMinutes").disabled = false;
    $("saveIdle").disabled = false;
  });
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
}

start();
setInterval(() => { guarded(render); guarded(refreshStatus); }, 15_000);
setInterval(() => guarded(renderWeek), 5 * 60_000);
