function setTrackPosition(node, start, end, from, to) {
  node.style.insetInlineStart = `${(start - from) / (to - from) * 100}%`;
  if (end !== null) node.style.inlineSize = `${(end - start) / (to - from) * 100}%`;
}

function bookmarkLabel(b) {
  return [clock(b.ts), b.device_label, appName(b.app), b.title, b.note].filter(Boolean).join(" · ");
}

function renderDayTrack(summary, dayChanged) {
  const { day_start: from, day_end: to, timeline, bookmarks } = summary;
  const track = $("trackSegments");
  track.dataset.from = from;
  track.dataset.to = to;
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
    btn.dataset.end = end;
    btn.title = t("track.range", { start: clock(start), end: clock(end), category: categoryName(category) });
    btn.setAttribute("aria-label", btn.title);
    btn.tabIndex = index === 0 ? 0 : -1;
    btn.onclick = () => selectRange(range);
    setTrackPosition(btn, start, end, from, to);
    track.append(btn);
  }
  markTrackSelection();
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
  const actions = el("span", "visit-actions");
  actions.append(el("span", "muted", shortDuration(s.end - s.start)));
  const button = categoryButton(s);
  if (button) actions.append(button);
  const title = [s.device_label, s.title ? `${app} — ${s.title}` : app].filter(Boolean).join(" · ");
  const row = historyRow(`${clock(s.start)}–${clock(s.end)}`, title, "", actions);
  row.dataset.category = s.category;
  return row;
}

// Список сессий уже отсортирован от новых к старым, поэтому первая запись определяет порядок группы.
function groupWindowVisits(sessions) {
  const groups = new Map();
  for (const session of sessions) {
    const key = JSON.stringify([renderedDate, session.device, session.app, session.title]);
    let group = groups.get(key);
    if (!group) {
      group = { key, sessions: [], seconds: 0 };
      groups.set(key, group);
    }
    group.sessions.push(session);
    group.seconds += session.end - session.start;
  }
  return [...groups.values()];
}

function visitDuration(secs) {
  const remainder = secs % 60;
  if (secs < 60) return shortDuration(secs);
  const minutes = duration(secs - remainder);
  return remainder ? `${minutes} ${shortDuration(remainder)}` : minutes;
}

function groupedWindowRow(group, open) {
  const [latest] = group.sessions;
  const count = group.sessions.length;
  if (count === 1) return windowRow(latest);

  const app = appName(latest.app);
  const totals = el("span", "visit-totals");
  const badge = el("span", "visit-count", `×${count}`);
  badge.title = t("hist.visits", { n: count });
  totals.append(badge, el("span", "muted", visitDuration(group.seconds)));
  const title = [latest.device_label, latest.title ? `${app} — ${latest.title}` : app].filter(Boolean).join(" · ");
  const row = historyRow(clock(latest.start), title, "", totals, "summary");
  row.dataset.category = latest.category;

  const visits = el("ul", "visit-list");
  const block = el("details", "history-block");
  block.dataset.historyKey = group.key;
  block.open = open;
  block.append(row, visits);

  const showVisits = () => {
    if (!block.open || visits.childElementCount) return;
    const clockWithSeconds = new Intl.DateTimeFormat(I18N.locale, {
      hour: "2-digit", minute: "2-digit", second: "2-digit",
    });
    for (const visit of group.sessions) {
      const start = clockWithSeconds.format(visit.start * 1000);
      const end = clockWithSeconds.format(visit.end * 1000);
      const item = el("li", "visit");
      const actions = el("span", "visit-actions");
      actions.append(el("span", "", visitDuration(visit.end - visit.start)));
      const button = categoryButton(visit);
      if (button) actions.append(button);
      item.append(el("span", "", `${start}–${end}`), actions);
      visits.append(item);
    }
  };
  block.ontoggle = showVisits;
  showVisits();
  return block;
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
  if (b.device_label) text = `${b.device_label} · ${text}`;
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
  block.dataset.historyKey = b.start;
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
  const openBlocks = new Set([...target.querySelectorAll("details[open]")].map((block) => block.dataset.historyKey));
  const focusedBlock = target.contains(document.activeElement)
    ? document.activeElement.closest("details")?.dataset.historyKey
    : null;
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
  let totalRows;
  if (tab === "windows") {
    const grouped = $("windowOrder").value === "grouped";
    if (!grouped && !range && !q && historyBlocks.length) {
      rows = timelineWithMarks(openBlocks);
    } else {
      const sessions = timelineData.filter((s) =>
        (!range || (s.start < range.end && s.end > range.start)) && matches(q, appName(s.app), s.title));
      if (grouped) {
        const groups = groupWindowVisits(sessions);
        totalRows = groups.length;
        rows = groups.slice(0, HISTORY_LIMIT).map((g) => groupedWindowRow(g, openBlocks.has(g.key)));
      } else {
        totalRows = sessions.length;
        rows = sessions.slice(0, HISTORY_LIMIT).map(windowRow);
      }
    }
    empty = q ? t("hist.empty.day_q") : t("hist.empty.windows");
  } else if (tab === "clips") {
    let pinned = [];
    try { pinned = JSON.parse(localStorage.getItem("pinnedClips") || "[]"); } catch {}
    if (!Array.isArray(pinned)) pinned = [];
    const pinnedSet = new Set(pinned.map((p) => p.text));
    const savePinned = (next) => localStorage.setItem("pinnedClips", JSON.stringify(next));
    const combined = [
      ...pinned.map((c) => ({ ...c, pinned: true })),
      ...clipData.filter((c) => !pinnedSet.has(c.text)).map((c) => ({ ...c, pinned: false })),
    ];
    clipRows = combined.filter((c) => matches(q, appName(c.app), c.text));
    clipSel = Math.min(clipSel, Math.max(clipRows.length - 1, 0));
    rows = clipRows.map((c, i) => {
      const buttons = el("span", "btns");
      const pinBtn = el("button", "small", t(c.pinned ? "clip.unpin" : "clip.pin"));
      pinBtn.type = "button";
      pinBtn.onclick = (e) => {
        e.stopPropagation();
        const next = c.pinned
          ? pinned.filter((p) => p.text !== c.text)
          : [{ text: c.text, app: c.app, ts: c.ts }, ...pinned];
        savePinned(next);
        renderHistory();
      };
      buttons.append(copyButton(c.text), pinBtn);
      if (!c.pinned) buttons.append(deleteButton(c.text));
      const meta = c.pinned ? `📌 ${t("clip.pinned")}` : `${clock(c.ts)} · ${appName(c.app)}`;
      const row = historyRow(meta, c.text, "", buttons);
      if (c.pinned) row.classList.add("clip-pinned");
      row.classList.toggle("selected", i === clipSel);
      row.onclick = () => { clipSel = i; renderHistory(); };
      return row;
    });
    $("tabNote").textContent = clipHint();
    $("clearClips").hidden = false;
    empty = q ? t("hist.empty.day_q") : t("hist.empty.clips");
  } else {
    const day = (ts) => new Date(ts * 1000).toLocaleDateString(I18N.locale, { day: "numeric", month: "short" });
    rows = allHits.map((h) => {
      const button = el("button", "small", t("search.open_day"));
      button.type = "button";
      button.onclick = () => guarded(() => openSearchDay(h));
      const title = [h.device && deviceName(h.device), h.title ? `${appName(h.app)} — ${h.title}` : appName(h.app)].filter(Boolean).join(" · ");
      return historyRow(`${day(h.last_ts)} ${clock(h.last_ts)}`, title, "", button);
    });
    empty = q ? t("hist.empty.all_q") : t("hist.empty.all");
  }

  if (!rows.length) return target.append(note(empty));
  target.append(...rows.slice(0, HISTORY_LIMIT));
  const total = totalRows ?? rows.length;
  if (total > HISTORY_LIMIT) target.append(note(t("hist.more", { n: HISTORY_LIMIT, total })));
  if (focusedBlock) {
    const block = [...target.querySelectorAll("details")].find((node) => node.dataset.historyKey === focusedBlock);
    block?.querySelector("summary").focus({ preventScroll: true });
  }
}
