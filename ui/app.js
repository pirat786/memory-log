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
// Пока открыт «Сегодня», после полуночи окно само переходит на новый день.
let followToday = true;

const $ = (id) => document.getElementById(id);
const iso = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const clock = (ts) => new Date(ts * 1000).toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
const pct = (v) => `${Math.round(v)}%`;

function duration(secs) {
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  if (h && m) return `${h} ч ${m} мин`;
  if (h) return `${h} ч`;
  return `${m} мин`;
}

function bytes(n) {
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
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
  if (!rows.length) return target.append(note("Пока нет данных"));
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
  const parts = [`с ${clock(r.first_ts)} до ${clock(r.last_ts)}`];
  parts.push(r.breaks.length ? `перерывов: ${r.breaks.length} (${duration(r.break_secs)})` : "без долгих перерывов");
  parts.push(`переключений между программами: ${r.switches}`);
  if (r.longest_focus_secs >= 60) parts.push(`самый долгий фокус: ${duration(r.longest_focus_secs)}`);
  return parts.join(" · ");
}

function renderLoad(load) {
  cards($("load"), load ? [
    ["Процессор", `${pct(load.cpu.avg)} в среднем`, `мин ${pct(load.cpu.min)} · макс ${pct(load.cpu.max)} в ${clock(load.cpu.max_ts)}`],
    ["Память (ОЗУ)", `${pct(load.mem.avg)} в среднем`, `мин ${pct(load.mem.min)} · макс ${pct(load.mem.max)} в ${clock(load.mem.max_ts)}`],
  ] : [], "Данные о нагрузке появятся через несколько секунд после запуска");
}

function renderNet(net) {
  cards($("net"), net ? [
    ["Получено", bytes(net.rx_total), "скачано из интернета"],
    ["Отправлено", bytes(net.tx_total), "загружено в интернет"],
    ["Пик трафика", net.peak_hour === null ? "—" : `${String(net.peak_hour).padStart(2, "0")}:00`, "самый активный час"],
  ] : [], "Пока нет данных");

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
    rows.push(`Компьютер работает без перезагрузки: ${duration(h.uptime_secs)}`);
    if (h.disk_free_start !== null && h.disk_free_end !== null) {
      const diff = h.disk_free_end - h.disk_free_start;
      rows.push(`Свободно на диске C: ${h.disk_free_end.toFixed(1)} ГБ (${diff >= 0 ? "+" : ""}${diff.toFixed(1)} ГБ за день)`);
    }
    if (h.battery_min !== null && h.battery_min < 100) rows.push(`Заряд батареи опускался до ${h.battery_min}%`);
    if (h.on_battery_secs > 0) rows.push(`Работал от батареи: ${duration(h.on_battery_secs)}`);
    if (h.temp_max !== null) rows.push(`Максимальная температура: ${Math.round(h.temp_max)} °C`);
  }
  list($("health"), rows, "Пока нет данных");
}

async function renderWeek() {
  const days = await invoke("week_overview");
  bars($("week"), days.map((d) => ({
    label: new Date(d.date + "T00:00:00").toLocaleDateString("ru-RU", { weekday: "short", day: "numeric" }),
    secs: d.active_secs,
    value: d.active_secs ? duration(d.active_secs) : "—",
  })));
}

async function render() {
  if (followToday) current = new Date();
  const isToday = iso(current) === iso(new Date());
  $("date").textContent = isToday ? "Сегодня" : current.toLocaleDateString("ru-RU", { day: "numeric", month: "long" });
  $("next").disabled = isToday;

  const s = await invoke("day_summary", { date: iso(current) });
  $("total").textContent = duration(s.active_secs);
  $("rhythm").textContent = renderRhythm(s.rhythm);

  bars($("categories"), s.by_category.map((c) => ({ label: CATEGORY_NAMES[c.category] ?? c.category, secs: c.secs })));
  bars($("apps"), s.top_apps.map((a) => ({ label: a.app, secs: a.secs })));
  bars($("goals"), s.goals.map((g) => ({
    label: CATEGORY_NAMES[g.category] ?? g.category,
    secs: Math.min(g.used_mins, g.limit_mins) * 60,
    over: g.used_mins > g.limit_mins,
    value: `${g.used_mins} из ${g.limit_mins} мин`,
  })));
  if (!s.goals.length) $("goals").replaceChildren(note("Например: не больше 60 минут в соцсетях"));

  list($("focus"), s.focus_blocks.map((b) => `${b.app}: ${clock(b.start)}–${clock(b.end)} (${duration(b.end - b.start)})`),
    "Длинных занятий пока нет");
  list($("bookmarks"), s.bookmarks.map((b) => `${clock(b.ts)} · ${b.app}${b.title ? " — " + b.title : ""}${b.note ? " · " + b.note : ""}`),
    "Нажмите «Запомнить это», чтобы сохранить важный момент");

  renderLoad(s.load);
  renderNet(s.net);
  list($("procs"), s.procs.map((p) =>
    `${p.name}: процессор до ${pct(p.max_cpu)} в ${clock(p.max_cpu_ts)}, память до ${p.max_mem_mb} МБ в ${clock(p.max_mem_ts)}`),
    "Пока нет данных");
  renderHealth(s.health);
  list($("downloads"), s.downloads.map((d) => `${clock(d.ts)} · ${d.name} (${bytes(d.size)})`), "За этот день ничего не скачано");
}

async function refreshStatus() {
  const st = await invoke("status");
  const e = $("status");
  const stale = st.last_sample_age_secs === null || st.last_sample_age_secs > 30;
  if (st.paused) {
    e.className = "status bad";
    e.textContent = "На паузе";
  } else if (stale) {
    e.className = "status bad";
    e.textContent = st.last_sample_age_secs === null ? "Записей ещё нет" : `Нет новых записей ${duration(st.last_sample_age_secs)}`;
  } else {
    e.className = "status ok";
    e.textContent = "Запись идёт";
  }
  $("pause").textContent = st.paused ? "▶ Продолжить" : "⏸ Пауза";
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
    toolMessage(`Сохранено: ${await invoke("export_day_csv", { date: iso(current) })}`);
  } catch (e) {
    toolMessage(`Не удалось сохранить: ${e}`);
  }
};

$("forget").onclick = async () => {
  if (!window.confirm("Удалить всё, что записано за последние 10 минут?")) return;
  await invoke("forget_minutes", { minutes: 10 });
  toolMessage("Последние 10 минут удалены");
  render();
};

$("autostart").onchange = async (e) => {
  try {
    await invoke("set_autostart", { enabled: e.target.checked });
    toolMessage(e.target.checked ? "Программа будет запускаться вместе с Windows" : "Автозапуск выключен");
  } catch (err) {
    e.target.checked = !e.target.checked;
    toolMessage(`Не удалось изменить автозапуск: ${err}`);
  }
};

let searchTimer;
$("q").oninput = (e) => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(async () => {
    const q = e.target.value.trim();
    if (!q) return $("results").replaceChildren();
    const hits = await invoke("search", { query: q });
    const day = (ts) => new Date(ts * 1000).toLocaleDateString("ru-RU", { day: "numeric", month: "short" });
    list($("results"), hits.map((h) => `${day(h.last_ts)} ${clock(h.last_ts)} · ${h.app}${h.title ? " — " + h.title : ""}`),
      "Ничего не найдено");
  }, 250);
};

for (const [value, name] of Object.entries(CATEGORY_NAMES)) {
  const o = el("option", "", name);
  o.value = value;
  $("goalCat").append(o);
}

// Любая ошибка показывается на странице, а не оставляет её пустой.
async function guarded(task) {
  const box = $("errorBox");
  try {
    await task();
    box.hidden = true;
  } catch (e) {
    box.hidden = false;
    box.textContent = `Не удалось загрузить данные: ${typeof e === "string" ? e : JSON.stringify(e)}`;
  }
}

async function start() {
  await guarded(async () => { $("autostart").checked = await invoke("get_autostart"); });
  await guarded(render);
  await guarded(renderWeek);
  await guarded(refreshStatus);
}

start();
setInterval(() => { guarded(render); guarded(refreshStatus); }, 15_000);
setInterval(() => guarded(renderWeek), 5 * 60_000);
