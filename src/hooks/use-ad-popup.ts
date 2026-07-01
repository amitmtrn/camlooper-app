import { useEffect, useRef } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  AD_POPUP_URL,
  AD_INTERVAL_MS,
  AD_POPUP_INITIAL_DELAY_MS,
  AD_POPUP_STORAGE_KEY,
} from "@/lib/ads";

/**
 * Opens the ad popup URL in the user's default browser on an hourly cadence.
 * The last-shown time is persisted in localStorage so the schedule survives
 * app restarts (fires 1h after the last show, not 1h after each launch).
 */
export function useAdPopup() {
  const startedRef = useRef(false);

  useEffect(() => {
    // Guard against React 18 StrictMode double-invoke in dev.
    if (startedRef.current) return;
    startedRef.current = true;

    let timeoutId: ReturnType<typeof setTimeout> | undefined;
    let intervalId: ReturnType<typeof setInterval> | undefined;

    const fire = async () => {
      try {
        await openUrl(AD_POPUP_URL);
      } catch (err) {
        // Don't crash the app if the browser can't be opened.
        console.error("Failed to open ad popup:", err);
      }
      try {
        localStorage.setItem(AD_POPUP_STORAGE_KEY, String(Date.now()));
      } catch {
        // Ignore storage failures (e.g. private mode / quota).
      }
    };

    const startRecurring = () => {
      // Fire now, then repeat every interval.
      void fire();
      intervalId = setInterval(() => void fire(), AD_INTERVAL_MS);
    };

    // Determine how long until the first fire based on the persisted timestamp.
    const stored = Number(localStorage.getItem(AD_POPUP_STORAGE_KEY));
    const lastShown = Number.isFinite(stored) && stored > 0 ? stored : 0;
    const elapsed = Date.now() - lastShown;

    if (!lastShown || elapsed >= AD_INTERVAL_MS) {
      // Already due — fire shortly after launch.
      timeoutId = setTimeout(startRecurring, AD_POPUP_INITIAL_DELAY_MS);
    } else {
      // Wait out the remainder of the current interval, then fire and recur.
      timeoutId = setTimeout(startRecurring, AD_INTERVAL_MS - elapsed);
    }

    return () => {
      if (timeoutId) clearTimeout(timeoutId);
      if (intervalId) clearInterval(intervalId);
    };
  }, []);
}
