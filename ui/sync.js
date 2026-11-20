let syncInfo = null;
let syncMessage = "";
let syncSubmitting = false;
let deviceScope = "local";
let renderedDeviceScope;

function currentDeviceScope() {
  return activeView === "day" || activeView === "history" ? deviceScope : "local";
}

function deviceName(id) {
  if (id === "local") return syncInfo?.device.label ?? t("sync.local");
  return syncInfo?.peers.find((device) => device.id === id)?.label ?? id.slice(0, 8);
}

function syncError(error) {
  const message = String(error);
  return message.startsWith("sync.") ? t(message) : t("sync.failed", { error: message });
}

function updateDeviceControls() {
  const picker = $("deviceScope");
  const loggedIn = !!syncInfo?.enabled;
  picker.hidden = !loggedIn || (activeView !== "day" && activeView !== "history");
  const scopes = [{ id: "local", label: t("sync.this_pc", { label: deviceName("local") }) }];
  if (loggedIn && syncInfo.peers.length) scopes.push({ id: "all", label: t("sync.all") }, ...syncInfo.peers);
  if (!scopes.some((scope) => scope.id === deviceScope)) deviceScope = "local";
  const options = JSON.stringify(scopes);
  if (picker.dataset.options !== options) {
    picker.dataset.options = options;
    picker.replaceChildren(...scopes.map((scope) => {
      const option = el("option", "", scope.label);
      option.value = scope.id;
      return option;
    }));
  }
  picker.value = deviceScope;
  const local = currentDeviceScope() === "local";
  for (const name of ["clips", "downloads"]) {
    document.querySelector(`[data-tab="${name}"]`).hidden = !local;
  }
  $("export").hidden = !local;
  if (!local && (tab === "clips" || tab === "downloads")) setTab("windows");
}

function setDeviceScope(scope, refresh = true) {
  const before = deviceScope;
  deviceScope = scope;
  updateDeviceControls();
  if (refresh && before !== deviceScope) {
    if (tab === "all") searchAll();
    guarded(render);
  }
}

function renderSyncProgress(progress) {
  const bar = $("syncProgress");
  bar.hidden = !progress;
  if (progress?.total !== undefined) {
    bar.max = progress.total;
    bar.value = progress.done;
  } else {
    bar.removeAttribute("value");
  }
  return progress ? t(`sync.progress_${progress.stage}`, progress) : "";
}

async function refreshSync() {
  const info = await invoke("sync_status");
  const previous = syncInfo;
  const previousScope = deviceScope;
  syncInfo = info;
  if (!previous) $("syncDeviceName").value = info.device.label;
  const needsLogin = !info.enabled || ["sync.reauth", "sync.bad_client"].includes(info.error);
  const canConnect = needsLogin && info.configured_client;
  const status = $("syncStatus");
  const last = info.synced_at ? new Date(info.synced_at * 1000).toLocaleString(I18N.locale) : "";
  const progress = renderSyncProgress(info.enabled && !info.sign_in ? info.progress : null);
  const signIn = info.sign_in === "browser" ? t("sync.browser")
    : info.sign_in === "finishing" ? t("sync.sign_in_finishing")
      : info.sign_in_error ? syncError(info.sign_in_error) : "";
  const message = signIn || progress || syncMessage || (info.enabled && info.error ? syncError(info.error)
    : info.enabled ? (last ? t("sync.last", { email: info.email, date: last }) : t("sync.pending"))
      : t(info.configured_client ? "sync.off" : "sync.unavailable"));
  if (status.textContent !== message) status.textContent = message;
  $("syncSettings").hidden = !info.enabled && !info.configured_client;
  $("syncFields").disabled = syncSubmitting || info.sign_in === "finishing";
  $("syncConnectFields").hidden = !canConnect;
  $("syncConnect").hidden = !canConnect;
  $("syncConnect").textContent = t(info.sign_in === "browser" ? "sync.retry_connect" : "sync.connect");
  $("syncRename").hidden = needsLogin;
  $("syncRename").disabled = !!info.sign_in;
  $("syncNow").hidden = needsLogin;
  $("syncNow").disabled = info.busy || !!info.sign_in;
  $("syncDisconnect").hidden = !info.enabled;
  $("syncDisconnect").disabled = !!info.sign_in;
  $("syncIndicator").hidden = !info.enabled;
  $("syncIndicator").title = status.textContent;
  updateDeviceControls();
  if (previousScope !== deviceScope) {
    if (tab === "all") searchAll();
    guarded(render);
  }

  const devices = [info.device, ...info.peers];
  const deviceKey = JSON.stringify({ devices, busy: syncSubmitting || !!info.sign_in });
  if (previous && $("syncDevices").dataset.key === deviceKey) return;
  $("syncDevices").dataset.key = deviceKey;
  $("syncDevices").replaceChildren(...devices.map((device, index) => {
    const row = el("div", "item sync-device");
    const infoBox = el("div");
    infoBox.append(el("strong", "", index ? device.label : t("sync.this_pc", { label: device.label })),
      el("code", "device-id", device.id));
    row.append(infoBox);
    if (index > 0) {
      const remove = el("button", "small danger", t("sync.remove_peer"));
      remove.type = "button";
      remove.disabled = syncSubmitting || !!info.sign_in;
      remove.onclick = () => {
        if (window.confirm(t("sync.confirm_remove_peer", { label: device.label }))) {
          syncAction(() => invoke("sync_remove_peer", { device: device.id }));
        }
      };
      row.append(remove);
    }
    return row;
  }));
}

async function syncAction(action) {
  if (syncSubmitting) return;
  syncSubmitting = true;
  syncMessage = "";
  $("syncFields").disabled = true;
  try {
    await action();
    syncMessage = "";
  } catch (error) {
    syncMessage = syncError(error);
  } finally {
    syncSubmitting = false;
    await guarded(refreshSync);
  }
}

async function connectGoogle(label, includeHistory) {
  if (syncSubmitting || syncInfo?.sign_in === "finishing") return;
  syncMessage = "";
  try {
    await invoke("sync_connect", { label, includeHistory });
  } catch {
    // The backend stores the result of the current attempt across window reloads.
  } finally {
    await guarded(refreshSync);
  }
}

function initSyncControls() {
  $("deviceScope").onchange = (e) => setDeviceScope(e.target.value);
  $("syncIndicator").onclick = () => {
    setView("settings");
    $("syncTitle").scrollIntoView({ block: "start" });
    $("syncTitle").focus({ preventScroll: true });
  };
  $("syncSettings").onsubmit = (e) => {
    e.preventDefault();
    const label = $("syncDeviceName").value;
    if (e.submitter?.id === "syncRename") {
      syncAction(() => invoke("sync_rename", { label }));
    } else if (e.submitter?.id === "syncConnect" && syncInfo?.configured_client) {
      connectGoogle(label, $("syncHistory").checked);
    }
  };
  $("syncNow").onclick = () => syncAction(() => invoke("sync_now"));
  $("syncDisconnect").onclick = () => {
    if (window.confirm(t("sync.confirm_off"))) syncAction(() => invoke("sync_disconnect"));
  };
}

function tagDevice(summary, device) {
  const label = device === "local" ? deviceName("local") : deviceName(device);
  const tag = (item) => ({ ...item, device, device_label: label });
  return {
    ...summary,
    timeline: summary.timeline.map(tag),
    bookmarks: summary.bookmarks.map(tag),
    history_blocks: summary.history_blocks.map(tag),
  };
}

async function loadDeviceDay(date, scope) {
  if (scope === "local") return { summary: await invoke("day_summary", { date }), devices: [] };
  const request = invoke("sync_device_days", { date, device: scope === "all" ? null : scope });
  if (scope !== "all") {
    const [day] = await request;
    if (!day) throw t("sync.missing");
    return { summary: tagDevice(day.summary, day.device.id), devices: [] };
  }
  const [local, peers] = await Promise.all([invoke("day_summary", { date }), request]);
  const devices = [{ device: { id: "local", label: deviceName("local") }, summary: local }, ...peers];
  const tagged = devices.map(({ device, summary }) => tagDevice(summary, device.id));
  return {
    devices,
    summary: {
      ...local,
      timeline: tagged.flatMap((day) => day.timeline).sort((a, b) => b.start - a.start),
      bookmarks: tagged.flatMap((day) => day.bookmarks).sort((a, b) => a.ts - b.ts),
      history_blocks: [],
      clips: [],
      downloads: [],
    },
  };
}

function renderDeviceOverview(scope, days) {
  const all = scope === "all";
  const target = $("allDevicesRows");
  $("allDevicesOverview").hidden = !all;
  for (const section of document.querySelectorAll("#dayView > section:not(#allDevicesOverview), #dayView > .overview-grid")) {
    section.hidden = all;
  }
  $("week").parentElement.hidden = scope !== "local";
  $("comparison").closest("section").hidden = scope !== "local";
  $("goals").closest("section").hidden = scope !== "local";
  if (!all) return;
  target.replaceChildren();
  for (const { device, summary } of days) {
    const row = el("div", "item device-overview");
    const body = el("div");
    const stats = summary.by_category.slice(0, 3).map((item) =>
      `${categoryName(item.category)} · ${shortDuration(item.secs)}`);
    body.append(el("strong", "", `${device.label} · ${shortDuration(summary.active_secs)}`));
    if (stats.length) body.append(el("div", "muted", stats.join("  ·  ")));
    const open = el("button", "small", t("sync.open_device"));
    open.type = "button";
    open.onclick = () => setDeviceScope(device.id);
    row.append(body, open);
    target.append(row);
  }
}

async function searchForDevice(query, scope) {
  if (scope === "local") return invoke("search", { query });
  const remote = invoke("sync_search", { query, device: scope === "all" ? null : scope });
  if (scope !== "all") return (await remote).map(({ device, hit }) => ({ ...hit, device }));
  const [local, peers] = await Promise.all([invoke("search", { query }), remote]);
  return [...local.map((hit) => ({ ...hit, device: "local" })), ...peers.map(({ device, hit }) => ({ ...hit, device }))]
    .sort((a, b) => b.last_ts - a.last_ts).slice(0, 100);
}
