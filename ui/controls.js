function syncTheme() {
  const dark = memoryTheme.mode === "dark" ||
    (memoryTheme.mode === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  $("themeMode").value = memoryTheme.mode;
  const toggle = $("themeToggle");
  const label = t(dark ? "theme.to_light" : "theme.to_dark");
  toggle.title = label;
  toggle.setAttribute("aria-label", label);
  toggle.setAttribute("aria-pressed", dark);
}

let calendarDate;
let calendarRequest = 0;

async function renderCalendar() {
  const request = ++calendarRequest;
  const month = calendarDate;
  const today = new Date();
  const days = $("calendarDays");
  $("calendarMonth").textContent = month.toLocaleDateString(I18N.locale, { month: "long", year: "numeric" });
  $("calendarNext").disabled = iso(month).slice(0, 7) === iso(today).slice(0, 7);
  days.replaceChildren();
  const scope = currentDeviceScope();
  const heatmap = new Map((await invoke("calendar_heatmap", { month: iso(month), device: scope === "local" ? null : scope }))
    .map((d) => [d.date, d.active_secs]));
  if (request !== calendarRequest || !$("calendarDlg").open) return;

  for (let day = 1; day <= 7; day++) {
    const weekday = new Date(2024, 0, day).toLocaleDateString(I18N.locale, { weekday: "short" });
    days.append(el("span", "weekday", weekday));
  }
  const offset = (month.getDay() + 6) % 7;
  for (let i = 0; i < offset; i++) days.append(el("span"));
  const end = new Date(month.getFullYear(), month.getMonth() + 1, 0).getDate();
  for (let day = 1; day <= end; day++) {
    const date = new Date(month.getFullYear(), month.getMonth(), day, 12);
    const key = iso(date);
    const button = el("button", "", String(day));
    button.type = "button";
    button.disabled = key > iso(today);
    button.setAttribute("aria-pressed", key === iso(current));
    if (key === iso(today)) button.setAttribute("aria-current", "date");
    const label = date.toLocaleDateString(I18N.locale, { dateStyle: "full" });
    const secs = heatmap.get(key) ?? 0;
    if (secs > 0) {
      button.dataset.recorded = "";
      button.dataset.heat = secs >= 8 * 3600 ? "4" : secs >= 5 * 3600 ? "3" : secs >= 2 * 3600 ? "2" : "1";
      button.title = `${label} · ${shortDuration(secs)}`;
      button.setAttribute("aria-label", `${t("calendar.recorded", { date: label })} · ${shortDuration(secs)}`);
    } else {
      button.setAttribute("aria-label", label);
    }
    button.onclick = () => {
      $("calendarDlg").close();
      if (activeView === "history" && tab === "all") {
        $("q").value = "";
        setTab("windows");
      }
      goToDate(date);
    };
    days.append(button);
  }
}

function changeMonth(step) {
  calendarDate = new Date(calendarDate.getFullYear(), calendarDate.getMonth() + step, 1, 12);
  guarded(renderCalendar);
}

let recordingSettings;

async function loadRecordingSettings() {
  recordingSettings = await invoke("recording_settings");
  $("recordClipboard").checked = recordingSettings.clipboard_enabled;
  $("retentionDays").value = recordingSettings.retention_days;
  $("excludedApps").value = recordingSettings.excluded_apps;
  $("recordingFields").disabled = false;
}

function settingsAction(form, task) {
  const submit = form.querySelector("button[type=submit]");
  submit.disabled = true;
  return guarded(task).finally(() => { submit.disabled = false; });
}

async function saveRecording(form) {
  const settings = {
    clipboard_enabled: $("recordClipboard").checked,
    retention_days: $("retentionDays").valueAsNumber,
    excluded_apps: $("excludedApps").value,
  };
  const previous = recordingSettings.retention_days;
  if (settings.retention_days && (!previous || settings.retention_days < previous) &&
    !window.confirm(t("record.shorten", { days: settings.retention_days }))) return;
  return settingsAction(form, async () => {
    await invoke("set_recording_settings", { settings });
    await loadRecordingSettings();
    toolMessage(t("record.saved"));
  });
}

async function deletePeriod(form) {
  const from = $("deleteFrom").value;
  const through = $("deleteThrough").value;
  if (!from || !through || through < from || through > iso(new Date())) {
    toolMessage(t("record.bad_range"));
    return;
  }
  if (!window.confirm(t("record.confirm", { from, through }))) return;
  return settingsAction(form, async () => {
    await invoke("delete_days", { from, through });
    toolMessage(t("record.deleted"));
    await Promise.all([render(), renderWeek(), renderComparison()]);
  });
}

function categoryButton(session) {
  if (!session.app || session.app === HIDDEN_APP || (session.device && session.device !== "local")) return null;
  const button = el("button", "small category-button", t("category.label"));
  button.type = "button";
  button.title = t("category.edit");
  button.setAttribute("aria-label", t("category.edit"));
  button.onclick = () => editCategory(session);
  return button;
}

function editCategory(session) {
  const dialog = $("categoryDlg");
  $("categoryTarget").textContent = [appName(session.app), session.title].filter(Boolean).join(" — ");
  const value = $("categoryValue");
  value.replaceChildren();
  for (const category of [null, ...CATEGORIES]) {
    const option = el("option", "", category ? categoryName(category) : t("category.auto"));
    option.value = category ?? "";
    value.append(option);
  }
  value.value = session.category;
  $("categoryScope").value = "visit";
  $("categoryScope").querySelector('[value="window"]').disabled = !session.title;
  dialog.querySelector("form").onsubmit = (event) => {
    if (event.submitter?.value !== "ok") return;
    const change = {
      scope: $("categoryScope").value, ts: session.start, app: session.app,
      title: session.title, category: value.value || null,
    };
    guarded(async () => {
      await invoke("set_category_override", { change });
      await Promise.all([render(), renderWeek(), renderComparison(), renderCategoryRules()]);
    });
  };
  dialog.showModal();
}

async function renderCategoryRules() {
  const rules = await invoke("category_rules");
  const target = $("categoryRules");
  target.replaceChildren();
  if (!rules.length) target.append(note(t("category.empty")));
  for (const rule of rules) {
    const row = el("div", "setting-row");
    const label = [appName(rule.app), rule.title].filter(Boolean).join(" — ");
    const remove = el("button", "small danger", t("category.remove"));
    remove.type = "button";
    remove.onclick = () => guarded(async () => {
      await invoke("set_category_override", { change: { ...rule, category: null } });
      await Promise.all([render(), renderWeek(), renderComparison(), renderCategoryRules()]);
    });
    row.append(el("span", "", `${label} → ${categoryName(rule.category)}`), remove);
    target.append(row);
  }
}

let comparisonRequest = 0;
let workHours = null;
function readTime(value) {
  const [hours, minutes] = value.split(":").map(Number);
  return hours * 60 + minutes;
}

function saveWorkHours() {
  const start = $("workStart").value;
  const end = $("workEnd").value;
  const from = readTime(start);
  const through = readTime(end);
  if ($("workOnly").checked && !(from < through)) {
    $("workEnd").setCustomValidity(t("compare.invalid_hours"));
    $("workEnd").reportValidity();
    return;
  }
  workHours = $("workOnly").checked ? [from, through] : null;
  try {
    localStorage.setItem("memory-log.work-hours", JSON.stringify({ enabled: !!workHours, start, end }));
  } catch {}
  renderComparison();
}

function comparisonTable(title, entries, delta = true) {
  const section = el("div");
  const table = el("table", "compare-table");
  const head = table.createTHead().insertRow();
  const headers = [title, t("compare.current"), t("compare.previous")];
  if (delta) headers.push(t("compare.delta"));
  for (const heading of headers) {
    const cell = el("th", "", heading);
    cell.scope = "col";
    head.append(cell);
  }
  const body = table.createTBody();
  for (const [label, current, previous, difference] of entries) {
    const row = body.insertRow();
    const name = el("th", "", label);
    name.scope = "row";
    row.append(name);
    row.append(el("td", "compare-time", current), el("td", "compare-time", previous));
    if (delta) row.append(el("td", "compare-time compare-delta", difference));
  }
  section.append(table);
  return section;
}

async function renderComparison() {
  const request = ++comparisonRequest;
  const target = $("comparison");
  try {
    const { current: week, previous, goals } = await invoke("week_comparison", { workHours });
    if (request !== comparisonRequest) return;
    const format = (seconds) => seconds ? shortDuration(seconds) : "—";
    const row = (label, now, before) => {
      const difference = now - before;
      const delta = difference ? `${difference > 0 ? "+" : "−"}${shortDuration(Math.abs(difference))}` : "—";
      return [label, format(now), format(before), delta];
    };
    const entries = (a, b, label) => [...new Set([...Object.keys(a), ...Object.keys(b)])]
      .map((key) => [key, a[key] ?? 0, b[key] ?? 0])
      .sort((x, y) => Math.max(y[1], y[2]) - Math.max(x[1], x[2]))
      .slice(0, 8)
      .map(([key, now, before]) => row(label(key), now, before));
    const dates = el("div", "comparison-dates muted small-text");
    const shortDate = (date) => new Date(date + "T12:00:00").toLocaleDateString(I18N.locale, { day: "numeric", month: "short" });
    for (const period of [week, previous]) {
      dates.append(el("span", "", `${shortDate(period.from)} – ${shortDate(period.through)}`));
    }
    target.replaceChildren(dates, comparisonTable(t("compare.total"), [
      row(t("hero.caption"), week.active_secs, previous.active_secs),
    ]));
    const categories = entries(week.categories, previous.categories, categoryName);
    const apps = entries(week.apps, previous.apps, appName);
    if (categories.length) target.append(comparisonTable(t("compare.categories"), categories));
    if (apps.length) target.append(comparisonTable(t("compare.apps"), apps));
    if (goals.length) {
      const met = (period, category) => period.recorded_days ? `${period.goals_met[category] ?? 0} / ${period.recorded_days}` : "—";
      target.append(comparisonTable(t("compare.goals"), goals.map((goal) => [
        t("compare.goal", { category: categoryName(goal.category), limit: goal.limit_mins }),
        met(week, goal.category), met(previous, goal.category),
      ]), false));
    }
  } catch (err) {
    if (request === comparisonRequest) target.replaceChildren(note(t("compare.error", { err })));
  }
}

function initExtraControls() {
  syncTheme();
  $("themeToggle").onclick = () => {
    memoryTheme.set($("themeToggle").getAttribute("aria-pressed") === "true" ? "light" : "dark");
    syncTheme();
  };
  $("themeMode").onchange = (event) => {
    memoryTheme.set(event.target.value);
    syncTheme();
  };
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", syncTheme);
  $("date").onclick = () => {
    calendarDate = new Date(current.getFullYear(), current.getMonth(), 1, 12);
    $("calendarDlg").showModal();
    guarded(renderCalendar);
  };
  $("calendarPrev").onclick = () => changeMonth(-1);
  $("calendarNext").onclick = () => changeMonth(1);
  $("openComparison").onclick = () => $("comparisonTitle").focus();
  $("recordingSettings").onsubmit = (event) => {
    event.preventDefault();
    saveRecording(event.currentTarget);
  };
  $("deletePeriod").onsubmit = (event) => {
    event.preventDefault();
    deletePeriod(event.currentTarget);
  };
  $("deleteFrom").max = $("deleteThrough").max = iso(new Date());
  try {
    const saved = JSON.parse(localStorage.getItem("memory-log.work-hours"));
    if (saved) {
      $("workStart").value = saved.start || "09:00";
      $("workEnd").value = saved.end || "18:00";
      const start = readTime($("workStart").value);
      const end = readTime($("workEnd").value);
      if (saved.enabled && start < end) workHours = [start, end];
    }
  } catch {}
  $("workOnly").checked = !!workHours;
  $("workTimes").hidden = !workHours;
  $("workOnly").onchange = () => {
    $("workTimes").hidden = !$("workOnly").checked;
    $("workEnd").setCustomValidity("");
    saveWorkHours();
  };
  for (const id of ["workStart", "workEnd"]) {
    $(id).oninput = () => $("workEnd").setCustomValidity("");
  }
  $("workHours").onsubmit = (event) => {
    event.preventDefault();
    saveWorkHours();
  };
}
