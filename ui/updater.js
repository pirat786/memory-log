let availableUpdate = null;
let updateStage = "checking";
let updateBusy = false;

function renderUpdateStatus() {
  $("updateStatus").textContent = t(`update.${updateStage}`, { version: availableUpdate });
  $("updateCheck").disabled = updateBusy;
  $("updateInstall").hidden = !availableUpdate;
  $("updateInstall").disabled = updateBusy;
  $("updateIndicator").hidden = !availableUpdate;
}

async function checkForAppUpdate() {
  if (updateBusy) return;
  updateBusy = true;
  updateStage = "checking";
  availableUpdate = null;
  renderUpdateStatus();
  try {
    availableUpdate = await invoke("check_update");
    updateStage = availableUpdate ? "available" : "current";
  } catch {
    updateStage = "check_failed";
  } finally {
    updateBusy = false;
    renderUpdateStatus();
  }
}

async function initAppUpdates() {
  try {
    if (!await invoke("updater_configured")) return;
  } catch {
    return;
  }
  $("updateSettings").hidden = false;
  $("updateIndicator").onclick = () => {
    setView("settings");
    $("updateTitle").scrollIntoView({ block: "start" });
    $("updateTitle").focus({ preventScroll: true });
  };
  $("updateCheck").onclick = checkForAppUpdate;
  $("updateInstall").onclick = async () => {
    if (updateBusy || !availableUpdate) return;
    updateBusy = true;
    updateStage = "installing";
    renderUpdateStatus();
    try {
      await invoke("install_update", { version: availableUpdate });
    } catch {
      availableUpdate = null;
      updateStage = "install_failed";
    } finally {
      updateBusy = false;
      renderUpdateStatus();
    }
  };
  await checkForAppUpdate();
}
