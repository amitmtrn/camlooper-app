import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  AD_POPUP_URL,
  WELCOME_POPUP_DELAY_MS,
  WELCOME_POPUP_MIN_INTERVAL_MS,
  WELCOME_POPUP_STORAGE_KEY,
} from "@/lib/ads";

/**
 * Opens the welcome ad page in the user's default browser shortly after launch, at most
 * once every WELCOME_POPUP_MIN_INTERVAL_MS. The last-shown time is persisted so the
 * throttle survives restarts — someone reopening the app all afternoon sees it once.
 */
export function useWelcomePopup() {
  const { i18n } = useTranslation();
  const startedRef = useRef(false);

  useEffect(() => {
    // Opening a browser tab on every Vite restart makes the app unpleasant to develop.
    if (!import.meta.env.PROD) return;

    // Guard against React 18 StrictMode double-invoke.
    if (startedRef.current) return;
    startedRef.current = true;

    const stored = Number(localStorage.getItem(WELCOME_POPUP_STORAGE_KEY));
    const lastShown = Number.isFinite(stored) && stored > 0 ? stored : 0;
    if (Date.now() - lastShown < WELCOME_POPUP_MIN_INTERVAL_MS) return;

    const timeoutId = setTimeout(async () => {
      try {
        // Pass the active UI language so the page shows localized house-ad copy. Use the
        // resolved i18n language rather than reading localStorage directly: the stored key
        // is empty when the language came from the browser/OS instead of an explicit pick.
        const lng = i18n.resolvedLanguage || i18n.language;
        await openUrl(lng ? `${AD_POPUP_URL}?lng=${encodeURIComponent(lng)}` : AD_POPUP_URL);
      } catch (err) {
        // Don't crash the app if the browser can't be opened.
        console.error("Failed to open welcome popup:", err);
      }
      try {
        // Recorded even when openUrl failed, so a machine with no default browser doesn't
        // retry on every single launch.
        localStorage.setItem(WELCOME_POPUP_STORAGE_KEY, String(Date.now()));
      } catch {
        // Ignore storage failures (e.g. private mode / quota).
      }
    }, WELCOME_POPUP_DELAY_MS);

    return () => clearTimeout(timeoutId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
