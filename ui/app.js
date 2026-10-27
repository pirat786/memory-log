const { invoke } = window.__TAURI__.core;

const CATEGORY_NAMES = {
  work: "Работа и документы",
  communication: "Общение",
  social: "Соцсети",
  entertainment: "Видео и развлечения",
  shopping: "Покупки",
  study: "Учёба",
  games: "Игры",
  other: "Другое",
};

let current = new Date();

const $ = (id) => document.getElementById(id);
const iso = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const time = (ts) => new Date(ts * 1000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });

function duration(secs) {
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  if (h && m) return `${h} ч ${m} мин`;
  if (h) return `${h} ч`;
  return `${m} мин`;
}

function empty(text) {
  const p = document.createElement("div");
  p.className = "muted";
  p.textContent = text;
  return p;
}

function bars(el, rows) {
  el.replaceChildren();
  if (!rows.length) return el.append(empty("Пока нет данных"));
  const max = Math.max(...rows.map((r) => r.secs));
  for (const r of rows) {
    const row = document.createElement("div");
    row.className = "bar";
    const label = document.createElement("div");
    label.textContent = r.label;
    const track = document.createElement("div");
    track.className = "track";
    const fill = document.createElement("div");
    fill.className = "fill";
    fill.style.width = `${(r.secs / max) * 100}%`;
    track.append(fill);
    const value = document.createElement("div");
    value.className = "muted";
    value.textContent = duration(r.secs);
    row.append(label, track, value);
    el.append(row);
  }
}

function list(el, rows, emptyText) {
  el.replaceChildren();
  if (!rows.length) return el.append(empty(emptyText));
  for (const text of rows) {
    const item = document.createElement("div");
    item.className = "item";
    item.textContent = text;
    el.append(item);
  }
}

async function render() {
  const isToday = iso(current) === iso(new Date());
  $("date").textContent = isToday ? "Сегодня" : current.toLocaleDateString("ru-RU", { day: "numeric", month: "long" });
  $("next").disabled = isToday;

  const s = await invoke("day_summary", { date: iso(current) });
  $("total").textContent = duration(s.active_secs);

  bars($("categories"), s.by_category.map((c) => ({ label: CATEGORY_NAMES[c.category] ?? c.category, secs: c.secs })));
  bars($("apps"), s.top_apps.map((a) => ({ label: a.app, secs: a.secs })));
  list($("focus"), s.focus_blocks.map((b) => `${b.app}: ${time(b.start)}–${time(b.end)} (${duration(b.end - b.start)})`),
    "Длинных занятий пока нет");
  list($("bookmarks"), s.bookmarks.map((b) => `${time(b.ts)} · ${b.app}${b.title ? " — " + b.title : ""}${b.note ? " · " + b.note : ""}`),
    "Нажмите «Запомнить это», чтобы сохранить важный момент");
}

async function refreshPause() {
  $("pause").textContent = (await invoke("is_paused")) ? "▶ Продолжить" : "⏸ Пауза";
}

$("prev").onclick = () => { current.setDate(current.getDate() - 1); render(); };
$("next").onclick = () => { current.setDate(current.getDate() + 1); render(); };
$("mark").onclick = async () => {
  const note = window.prompt("Короткая заметка (можно оставить пустой):", "");
  if (note === null) return;
  await invoke("add_bookmark", { note });
  render();
};
$("pause").onclick = async () => {
  await invoke("set_paused", { paused: !(await invoke("is_paused")) });
  refreshPause();
};

render();
refreshPause();
setInterval(render, 60_000);
