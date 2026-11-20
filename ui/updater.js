let availableUpdate = null;
let updateStage = "checking";
let updateBusy = false;
let lastUpdateCheck = 0;
const UPDATE_CHECK_INTERVAL = 6 * 60 * 60 * 1000;

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
  lastUpdateCheck = Date.now();
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
  const checkIfStale = () => {
    if (!document.hidden && !availableUpdate && Date.now() - lastUpdateCheck >= UPDATE_CHECK_INTERVAL) {
      void checkForAppUpdate();
    }
  };
  document.addEventListener("visibilitychange", checkIfStale);
  setInterval(checkIfStale, 60 * 60 * 1000);
}
