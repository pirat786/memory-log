let liveSystemData = null;

function renderLiveProcList() {
  const target = $("liveProcs");
  if (!liveSystemData) return target.replaceChildren();
  const query = $("procSearch").value.trim().toLowerCase();
  const sortBy = $("procSort").value;
  let listRows = liveSystemData.processes.filter((p) => !query || p.name.toLowerCase().includes(query));
  if (sortBy === "mem") {
    listRows = [...listRows].sort((a, b) => b.mem_mb - a.mem_mb || b.cpu_pct - a.cpu_pct);
  }
  const items = listRows.slice(0, 60).map((p) => {
    const row = el("div", "item live-proc");
    const name = p.count > 1 ? `${p.name} ×${p.count}` : p.name;
    const stats = `${pct(p.cpu_pct)} · ${p.mem_mb} ${t("unit.mb")}`;
    const kill = el("button", "small danger", t("system.kill"));
    kill.type = "button";
    kill.onclick = async () => {
      if (!window.confirm(t("system.confirm_kill", { name: p.name, count: p.count }))) return;
      try {
        const count = await invoke("kill_process", { name: p.name });
        $("liveProcMsg").textContent = t("system.killed", { name: p.name, count });
        await guarded(refreshLiveSystem);
      } catch (err) {
        const key = String(err);
        $("liveProcMsg").textContent = key.startsWith("system.") ? t(key) : key;
      }
    };
    const actions = el("div", "btns");
    if (p.exe) {
      const open = el("button", "small", t("system.open_folder"));
      open.type = "button";
      open.onclick = async () => {
        try { await invoke("reveal_path", { path: p.exe }); }
        catch (err) { $("liveProcMsg").textContent = String(err).startsWith("system.") ? t(String(err)) : String(err); }
      };
      actions.append(open);
    }
    actions.append(kill);
    row.append(el("strong", "live-proc-name", name), el("span", "muted live-proc-stats", stats), actions);
    return row;
  });
  target.replaceChildren(...(items.length ? items : [note(t("common.nodata"))]));
}

async function refreshLiveSystem() {
  if (activeView !== "system" || document.hidden) return;
  const data = await invoke("live_system");
  liveSystemData = data;
  $("sysLiveClock").textContent = `${data.os_name} · ${data.host_name}`;
  cards($("sysSpecs"), [
    [t("system.cpu_live"), pct(data.cpu_pct), `${data.cpu_brand} (${data.cpu_cores})`],
    [t("system.ram_live"), `${(data.used_mem_mb / 1024).toFixed(1)} / ${(data.total_mem_mb / 1024).toFixed(1)} ${t("unit.gb")}`,
      pct((data.used_mem_mb / Math.max(data.total_mem_mb, 1)) * 100)],
    [t("system.uptime_live"), duration(data.uptime_s), data.host_name],
  ], t("common.nodata"));

  bars($("sysDisks"), data.disks.map((d) => {
    const used = Math.max(0, d.total_gb - d.free_gb);
    const label = d.name ? `${d.mount} (${d.name})` : d.mount;
    return {
      label,
      secs: used,
      over: d.free_gb / Math.max(d.total_gb, 1) < 0.1,
      value: t("system.disk_val", { free: d.free_gb.toFixed(1), total: d.total_gb.toFixed(0), gb: t("unit.gb") }),
    };
  }));
  renderLiveProcList();
}

let junkCategories = [];
let portsData = [];

function renderJunkList() {
  const target = $("junkList");
  if (!junkCategories.length) {
    $("cleanJunk").hidden = true;
    return target.replaceChildren(note(t("system.junk_hint")));
  }
  const rows = junkCategories.map((cat) => {
    const row = el("label", "item junk-row");
    const check = el("input");
    check.type = "checkbox";
    check.value = cat.id;
    check.checked = cat.bytes > 0;
    check.disabled = cat.bytes === 0;
    const text = el("div");
    text.append(
      el("strong", "", t(`system.junk_${cat.id}`)),
      el("div", "muted small-text", t("system.junk_files", { files: cat.files })),
    );
    row.append(check, text, el("span", "junk-size", bytes(cat.bytes)));
    return row;
  });
  target.replaceChildren(...rows);
  $("cleanJunk").hidden = !junkCategories.some((c) => c.bytes > 0);
}

async function runJunkScan() {
  $("scanJunk").disabled = true;
  $("junkAge").disabled = true;
  $("junkMsg").textContent = t("system.junk_scanning");
  try {
    junkCategories = await invoke("scan_junk", { minAgeHours: Number($("junkAge").value) });
    const total = junkCategories.reduce((sum, c) => sum + c.bytes, 0);
    $("junkMsg").textContent = t("system.junk_found", { size: bytes(total) });
    renderJunkList();
  } finally {
    $("scanJunk").disabled = false;
    $("junkAge").disabled = false;
  }
}

async function runJunkClean() {
  const ids = [...$("junkList").querySelectorAll("input:checked")].map((i) => i.value);
  if (!ids.length) return;
  if (!window.confirm(t("system.junk_confirm"))) return;
  $("cleanJunk").disabled = true;
  $("scanJunk").disabled = true;
  $("junkAge").disabled = true;
  $("junkMsg").textContent = t("system.junk_cleaning");
  try {
    const minAgeHours = Number($("junkAge").value);
    const res = await invoke("clean_junk", { ids, minAgeHours });
    $("junkMsg").textContent = t("system.junk_done", {
      size: bytes(res.freed_bytes),
      deleted: res.deleted_files,
      skipped: res.skipped_files,
    });
    junkCategories = await invoke("scan_junk", { minAgeHours });
    renderJunkList();
  } finally {
    $("cleanJunk").disabled = false;
    $("scanJunk").disabled = false;
    $("junkAge").disabled = false;
  }
}

async function refreshStartup() {
  const items = await invoke("startup_items");
  const target = $("startupList");
  if (!items.length) return target.replaceChildren(note(t("common.nodata")));
  target.replaceChildren(...items.map((item) => {
    const row = el("label", "item startup-row");
    const info = el("div");
    const title = el("strong", "", item.name);
    title.append(el("span", "startup-badge", item.source));
    info.append(title, el("div", "muted small-text startup-cmd", item.command));
    const toggle = el("input");
    toggle.type = "checkbox";
    toggle.checked = item.enabled;
    toggle.onchange = async () => {
      toggle.disabled = true;
      try {
        await invoke("set_startup_item", { id: item.id, enabled: toggle.checked });
        $("startupMsg").textContent = t(toggle.checked ? "system.startup_on" : "system.startup_off", { name: item.name });
      } catch (err) {
        toggle.checked = !toggle.checked;
        const key = String(err);
        $("startupMsg").textContent = key.startsWith("system.") ? t(key) : key;
      } finally {
        toggle.disabled = false;
      }
    };
    const controls = el("div", "setting-actions");
    if (item.source !== "Store") {
      const open = el("button", "small", t("system.open_folder"));
      open.type = "button";
      open.onclick = async (e) => {
        e.preventDefault();
        e.stopPropagation();
        try { await invoke("reveal_path", { path: item.command }); }
        catch (err) { $("startupMsg").textContent = String(err).startsWith("system.") ? t(String(err)) : String(err); }
      };
      controls.append(open);
    }
    controls.append(toggle);
    row.append(info, controls);
    return row;
  }));
}

function renderPortsList() {
  const target = $("portsList");
  const query = $("portSearch").value.trim().toLowerCase();
  const rows = portsData.filter((p) =>
    !query || p.process.toLowerCase().includes(query) || String(p.port).includes(query) || p.proto.toLowerCase().includes(query));
  if (!rows.length) return target.replaceChildren(note(t("common.nodata")));
  target.replaceChildren(...rows.slice(0, 100).map((p) => {
    const row = el("div", "item port-row");
    row.append(
      el("code", "port-badge", `${p.proto}:${p.port}`),
      el("strong", "live-proc-name", p.process),
      el("span", "muted small-text", `${p.address} · PID ${p.pid}`),
    );
    return row;
  }));
}

async function refreshPorts() {
  portsData = await invoke("listening_ports");
  renderPortsList();
}

async function refreshPowerStatus() {
  const status = await invoke("power_status");
  $("keepAwake").checked = status.keep_awake;
  const active = status.rule;
  $("powerCancel").hidden = !active;
  $("powerApply").hidden = !!active;
  $("powerCondition").disabled = !!active;
  $("powerDelay").disabled = !!active;
  $("powerAction").disabled = !!active;
  if (active) {
    $("powerStatus").textContent = t("system.power_active", {
      cond: t(`system.cond_${active.condition === "net_idle" ? "net" : active.condition === "cpu_idle" ? "cpu" : "timer"}`),
      act: t(`system.act_${active.action}`),
      elapsed: duration(active.elapsed_secs),
      total: duration(active.delay_secs),
    });
  } else {
    $("powerStatus").textContent = status.keep_awake ? t("system.keep_awake_on") : "";
  }
}
