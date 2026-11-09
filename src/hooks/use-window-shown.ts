import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Whether the main window has actually been shown to the user. False while the app was
 * started hidden by the "start at login" entry and is only waiting in the tray.
 *
 * The welcome popup and the ad banner are gated on this: a hidden autostart launch must not
 * open a browser tab at every login, and must not count as an app open in analytics.
 */
export function useWindowShown(): boolean {
  const [shown, setShown] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen("app://shown", () => {
        if (!cancelled) setShown(true);
      });
      if (cancelled) {
        unlisten();
        return;
      }
      try {
        const mode = await invoke<string>("get_startup_mode");
        if (!cancelled && mode !== "hidden") setShown(true);
      } catch {
        // Backend without the command: behave as before (always visible).
        if (!cancelled) setShown(true);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return shown;
}
