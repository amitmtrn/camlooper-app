import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";

/**
 * Locale bundles, one lazy chunk each.
 *
 * These used to be eighteen static imports, so every user shipped and parsed all ~93 KB of
 * translations to read one of them. `import.meta.glob` lets Vite emit a chunk per locale and
 * we fetch only the active one (plus English as the fallback).
 */
const LOCALE_MODULES = import.meta.glob<{ default: Record<string, unknown> }>(
  "./locales/*.json",
);

const localeLoader = (code: string) => LOCALE_MODULES[`./locales/${code}.json`];

export const SUPPORTED_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "pt", label: "Português" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "it", label: "Italiano" },
  { code: "pl", label: "Polski" },
  { code: "ru", label: "Русский" },
  { code: "tr", label: "Türkçe" },
  { code: "id", label: "Bahasa Indonesia" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "th", label: "ไทย" },
  { code: "zh", label: "中文" },
  { code: "ja", label: "日本語" },
  { code: "ko", label: "한국어" },
  { code: "hi", label: "हिन्दी" },
  { code: "ar", label: "العربية" },
  { code: "he", label: "עברית" },
] as const;

export type LanguageCode = (typeof SUPPORTED_LANGUAGES)[number]["code"];

const LANGUAGE_STORAGE_KEY = "camlooper-lang";
const SUPPORTED_CODES: ReadonlySet<string> = new Set(
  SUPPORTED_LANGUAGES.map((l) => l.code),
);

// Must be read before init: the detector used to cache whatever it detected under this key,
// after which an auto-detected default was indistinguishable from a language the user chose.
const storedLanguage = localStorage.getItem(LANGUAGE_STORAGE_KEY);
const hadStoredLanguage = storedLanguage !== null;

/**
 * Resolve the startup language without i18next's detector.
 *
 * The detector is gone because it decides the language *during* init, which is too late to
 * know which bundle to load. This reproduces its configured order — localStorage, then
 * navigator — including the "en-US" → "en" narrowing that `load: "languageOnly"` did.
 */
function detectLanguage(): string {
  const candidates = [storedLanguage, ...(navigator.languages ?? [navigator.language])];
  for (const raw of candidates) {
    if (!raw) continue;
    const base = raw.toLowerCase().split("-")[0];
    if (SUPPORTED_CODES.has(base)) return base;
  }
  return "en";
}

async function loadBundle(code: string): Promise<Record<string, unknown> | null> {
  const loader = localeLoader(code);
  if (!loader) return null;
  try {
    return (await loader()).default;
  } catch {
    return null;
  }
}

/** Fetch a locale if it isn't loaded yet, then switch to it. */
export async function loadLanguage(code: string): Promise<void> {
  if (!i18n.hasResourceBundle(code, "translation")) {
    const bundle = await loadBundle(code);
    if (!bundle) return;
    i18n.addResourceBundle(code, "translation", bundle, true, true);
  }
  await i18n.changeLanguage(code);
}

// Keep <html dir/lang> in sync with the active language (RTL for ar/he).
const applyDir = (lng: string) => {
  document.documentElement.lang = lng;
  document.documentElement.dir = i18n.dir(lng);
};

/**
 * Initialise i18next with only the languages actually needed.
 *
 * Awaited by main.tsx before the first render, so there is no flash of untranslated text.
 */
export async function initI18n(): Promise<void> {
  const lng = detectLanguage();
  const [active, fallback] = await Promise.all([
    loadBundle(lng),
    lng === "en" ? Promise.resolve(null) : loadBundle("en"),
  ]);

  const resources: Record<string, { translation: Record<string, unknown> }> = {};
  if (active) resources[lng] = { translation: active };
  if (fallback) resources.en = { translation: fallback };

  await i18n.use(initReactI18next).init({
    resources,
    lng,
    fallbackLng: "en",
    supportedLngs: SUPPORTED_LANGUAGES.map((l) => l.code),
    interpolation: { escapeValue: false }, // React already escapes
  });

  // The detector used to write this; keep the key populated so a chosen language persists.
  i18n.on("languageChanged", (next) => {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, next);
    applyDir(next);
  });
  applyDir(i18n.resolvedLanguage || i18n.language || "en");

  applyInstallerLanguage();
}

// Locales the Windows installer can be shown in — the subset of SUPPORTED_LANGUAGES that
// Tauri ships NSIS translations for (see bundle.windows.nsis.languages in tauri.conf.json).
const INSTALLER_LANGUAGES: ReadonlySet<string> = new Set([
  "en", "ar", "fr", "de", "he", "it", "ja", "ko", "pt", "ru", "zh", "es", "tr",
]);

// The Windows installer asks which language to install in and records the choice, so on
// first launch prefer it over the locale detected above — the two differ whenever someone
// installs in a language other than the one their OS is set to. Anything chosen later in
// the app is cached under LANGUAGE_STORAGE_KEY and takes precedence from then on.
function applyInstallerLanguage() {
  if (hadStoredLanguage) return;
  invoke<string | null>("installer_language")
    .then((lng) => {
      if (!lng || lng === i18n.resolvedLanguage) return;
      // Someone whose language the installer can't display installs in English, which
      // must not override the OS locale — the app itself is still translated for them.
      const detected = i18n.resolvedLanguage;
      if (lng === "en" && detected && !INSTALLER_LANGUAGES.has(detected)) return;
      void loadLanguage(lng);
    })
    .catch(() => {
      // Non-Windows, or no recorded choice: the detected locale already applies.
    });
}

export default i18n;
