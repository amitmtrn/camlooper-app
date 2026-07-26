import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import en from "./locales/en.json";
import es from "./locales/es.json";
import pt from "./locales/pt.json";
import hi from "./locales/hi.json";
import ar from "./locales/ar.json";
import he from "./locales/he.json";
import fr from "./locales/fr.json";
import de from "./locales/de.json";
import it from "./locales/it.json";
import zh from "./locales/zh.json";
import ja from "./locales/ja.json";
import ko from "./locales/ko.json";
import ru from "./locales/ru.json";
import tr from "./locales/tr.json";
import id from "./locales/id.json";
import vi from "./locales/vi.json";
import pl from "./locales/pl.json";
import th from "./locales/th.json";

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

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      en: { translation: en },
      es: { translation: es },
      pt: { translation: pt },
      hi: { translation: hi },
      ar: { translation: ar },
      he: { translation: he },
      fr: { translation: fr },
      de: { translation: de },
      it: { translation: it },
      zh: { translation: zh },
      ja: { translation: ja },
      ko: { translation: ko },
      ru: { translation: ru },
      tr: { translation: tr },
      id: { translation: id },
      vi: { translation: vi },
      pl: { translation: pl },
      th: { translation: th },
    },
    fallbackLng: "en",
    supportedLngs: SUPPORTED_LANGUAGES.map((l) => l.code),
    // Match "en-US" → "en", "pt-BR" → "pt", etc.
    load: "languageOnly",
    nonExplicitSupportedLngs: true,
    detection: {
      order: ["localStorage", "navigator"],
      lookupLocalStorage: "camlooper-lang",
      caches: ["localStorage"],
    },
    interpolation: { escapeValue: false }, // React already escapes
  });

// Keep <html dir/lang> in sync with the active language (RTL for ar/he).
const applyDir = (lng: string) => {
  document.documentElement.lang = lng;
  document.documentElement.dir = i18n.dir(lng);
};
applyDir(i18n.resolvedLanguage || i18n.language || "en");
i18n.on("languageChanged", applyDir);

export default i18n;
