// Переводы: файлы лежат в ui/i18n/, список языков — в ui/i18n/languages.json.
// Как добавить язык: ui/i18n/README.md.
(function () {
  const { invoke } = window.__TAURI__.core;
  const FALLBACK = "en";
  let languages = [];
  let base = {};
  let dict = {};
  let code = FALLBACK;

  const loadJson = async (path) => (await fetch(path)).json();

  // Язык по умолчанию задаёт программа (по языку Windows); если её не спросить, берём язык браузера.
  function pick(saved) {
    const codes = languages.map((l) => l.code);
    if (saved && codes.includes(saved)) return saved;
    const nav = (navigator.language || "").slice(0, 2).toLowerCase();
    return codes.includes(nav) ? nav : FALLBACK;
  }

  // t("key", { name: value }) — строка на текущем языке; нет перевода — берётся английский.
  function t(key, params) {
    let s = dict[key] ?? base[key] ?? key;
    if (params) for (const [k, v] of Object.entries(params)) s = s.replaceAll(`{${k}}`, v);
    return s;
  }

  // Подписывает всё, что в разметке помечено data-i18n, data-i18n-placeholder, data-i18n-title.
  function applyStatic() {
    for (const e of document.querySelectorAll("[data-i18n]")) e.textContent = t(e.dataset.i18n);
    for (const e of document.querySelectorAll("[data-i18n-placeholder]")) e.placeholder = t(e.dataset.i18nPlaceholder);
    for (const e of document.querySelectorAll("[data-i18n-title]")) e.title = t(e.dataset.i18nTitle);
    document.documentElement.lang = code;
  }

  async function load(next) {
    code = next;
    dict = next === FALLBACK ? base : await loadJson(`i18n/${next}.json`);
  }

  async function init() {
    languages = await loadJson("i18n/languages.json");
    base = await loadJson(`i18n/${FALLBACK}.json`);
    let saved = null;
    try {
      saved = await invoke("get_language");
    } catch {
      // вне программы (например, при просмотре страницы в браузере) команд нет
    }
    await load(pick(saved));
    applyStatic();
  }

  async function setLanguage(next) {
    await load(next);
    try {
      await invoke("set_language", { code: next });
    } catch {
      // см. выше
    }
    applyStatic();
  }

  window.I18N = {
    init,
    t,
    applyStatic,
    setLanguage,
    get code() { return code; },
    get locale() { return (languages.find((l) => l.code === code) ?? {}).locale ?? code; },
    get languages() { return languages; },
  };
})();
