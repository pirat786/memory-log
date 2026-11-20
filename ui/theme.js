(() => {
  const storageKey = "memory-log.theme";
  function apply(value) {
    const theme = value === "light" || value === "dark" ? value : "system";
    document.documentElement.dataset.theme = theme;
    return theme;
  }

  try { apply(localStorage.getItem(storageKey)); }
  catch { apply("system"); }

  window.addEventListener("storage", (event) => {
    if (event.key === storageKey) apply(event.newValue);
  });
  window.memoryTheme = {
    get mode() { return document.documentElement.dataset.theme; },
    set(value) {
      const theme = apply(value);
      try { localStorage.setItem(storageKey, theme); } catch {}
    },
  };
})();
