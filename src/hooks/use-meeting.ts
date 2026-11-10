import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

/**
 * Auto-join: when a meeting link arrives from the phone, Rust (`src-tauri/src/meeting`) can
 * open it in a Chromium browser CamLooper controls and click through the pre-join screen.
 * This hook holds the two user preferences (on/off, guest display name) in localStorage and
 * falls back to the default browser when auto-join is off or no Chromium browser exists.
 */

export interface MeetingBrowserInfo {
  available: boolean;
  name: string | null;
}

export type MeetingPhase =
  | "launching"
  | "joining"
  | "waiting_admission"
  | "joined"
  | "blocked"
  | "timeout"
  | "failed";

export interface MeetingStatus {
  provider: string;
  phase: MeetingPhase;
  detail: string | null;
}

export interface MeetingOpenResult {
  mode: "auto" | "default" | "unavailable";
  browser: string | null;
}

const AUTO_JOIN_KEY = "camlooper.autoJoin";
const DISPLAY_NAME_KEY = "camlooper.meetingDisplayName";

function readBool(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "true";
  } catch {
    return fallback;
  }
}

function readString(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

export function useMeeting() {
  const [browser, setBrowser] = useState<MeetingBrowserInfo>({ available: false, name: null });
  const [autoJoin, setAutoJoinState] = useState(() => readBool(AUTO_JOIN_KEY, true));
  const [displayName, setDisplayNameState] = useState(() => readString(DISPLAY_NAME_KEY));

  useEffect(() => {
    let cancelled = false;
    invoke<MeetingBrowserInfo>("meeting_browser_info")
      .then((info) => {
        if (!cancelled) setBrowser(info);
      })
      .catch((error) => console.error("meeting: browser probe failed:", error));
    return () => {
      cancelled = true;
    };
  }, []);

  const setAutoJoin = useCallback((enabled: boolean) => {
    setAutoJoinState(enabled);
    try {
      localStorage.setItem(AUTO_JOIN_KEY, String(enabled));
    } catch {
      /* preference simply won't persist */
    }
  }, []);

  const setDisplayName = useCallback((name: string) => {
    setDisplayNameState(name);
    try {
      localStorage.setItem(DISPLAY_NAME_KEY, name);
    } catch {
      /* preference simply won't persist */
    }
  }, []);

  /** Open a validated meeting link: driven browser when possible, default browser otherwise. */
  const open = useCallback(
    async (url: string): Promise<MeetingOpenResult> => {
      const result = await invoke<MeetingOpenResult>("meeting_open", {
        url,
        autoJoin,
        displayName: displayName.trim() || null,
      });
      if (result.mode !== "auto") await openUrl(url);
      return result;
    },
    [autoJoin, displayName],
  );

  /** Open the dedicated profile so the user can sign in once / pick the CamLooper camera. */
  const openProfile = useCallback(() => invoke<string>("meeting_open_profile"), []);

  return { browser, autoJoin, setAutoJoin, displayName, setDisplayName, open, openProfile };
}

/** Progress of the join driven by Rust; `onStatus` always sees the latest closure. */
export function useMeetingStatus(onStatus: (status: MeetingStatus) => void) {
  const handler = useRef(onStatus);
  handler.current = onStatus;
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen<MeetingStatus>("meeting://status", (event) => handler.current(event.payload))
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((error) => console.error("meeting: could not listen for status:", error));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
