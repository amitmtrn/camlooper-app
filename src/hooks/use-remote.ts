import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Frontend side of the phone remote. All networking, validation and persistence live in Rust
 * (`src-tauri/src/remote`); this hook only mirrors state and relays user actions.
 *
 * Privacy: the backend makes no network connection until `link(code)` is called.
 */

export type RemoteStatus = "disabled" | "unlinked" | "linking" | "linked";
export type RemoteErrorCode = "code_invalid" | "rate_limited" | "network" | "server" | "disabled";
export type MeetingProvider = "zoom" | "teams" | "meet" | "generic";

export interface RemoteState {
  status: RemoteStatus;
  connected: boolean;
  mobileDeviceName: string | null;
  desktopName: string | null;
  autostart: boolean;
  lastError: string | null;
}

export interface OpenMeetingCommand {
  commandId: string;
  url: string;
  provider: MeetingProvider;
  mobileDeviceName: string | null;
  createdAt: string | null;
}

export interface LinkedPayload {
  mobileDeviceName: string | null;
  autostartEnabled: boolean;
}

export interface UnlinkedByPhonePayload {
  reason: string;
}

export interface CommandRejectedPayload {
  commandId: string;
  reason: string;
}

const INITIAL_STATE: RemoteState = {
  status: "unlinked",
  connected: false,
  mobileDeviceName: null,
  desktopName: null,
  autostart: false,
  lastError: null,
};

const ERROR_CODES: readonly RemoteErrorCode[] = ["code_invalid", "rate_limited", "network", "server", "disabled"];

function toErrorCode(err: unknown): RemoteErrorCode {
  return typeof err === "string" && (ERROR_CODES as readonly string[]).includes(err)
    ? (err as RemoteErrorCode)
    : "server";
}

export function useRemote() {
  const [state, setState] = useState<RemoteState>(INITIAL_STATE);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      // Subscribe first, then read, so no transition between the two is lost.
      unlisten = await listen<RemoteState>("remote://status", (event) => {
        if (!cancelled) setState(event.payload);
      });
      if (cancelled) {
        unlisten();
        return;
      }
      try {
        const current = await invoke<RemoteState>("remote_get_state");
        if (cancelled) return;
        setState(current);
        // Surfaces in Advanced → Debug logs; the manual test looks for this line.
        console.log(current.status === "unlinked" ? "remote: idle, no network" : `remote: ${current.status}`);
      } catch (err) {
        console.error("remote: could not read state:", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  /** Returns null on success, otherwise a stable error code for the UI. */
  const link = useCallback(async (code: string): Promise<RemoteErrorCode | null> => {
    setBusy(true);
    try {
      setState(await invoke<RemoteState>("remote_link", { code }));
      return null;
    } catch (err) {
      return toErrorCode(err);
    } finally {
      setBusy(false);
    }
  }, []);

  const unlink = useCallback(async () => {
    setBusy(true);
    try {
      setState(await invoke<RemoteState>("remote_unlink"));
    } catch (err) {
      console.error("remote: unlink failed:", err);
    } finally {
      setBusy(false);
    }
  }, []);

  const setAutostart = useCallback(async (enabled: boolean): Promise<boolean> => {
    try {
      await invoke<boolean>("remote_set_autostart", { enabled });
      return true;
    } catch (err) {
      console.error("remote: start-at-login toggle failed:", err);
      return false;
    }
  }, []);

  return { state, busy, link, unlink, setAutostart };
}

interface RemoteCommandHandlers {
  onOpenMeeting: (cmd: OpenMeetingCommand) => void | Promise<void>;
  onShowLinking: () => void;
  onLinked: (payload: LinkedPayload) => void;
  onUnlinkedByPhone: (payload: UnlinkedByPhonePayload) => void;
  onRejected: (payload: CommandRejectedPayload) => void;
}

/**
 * Backend → UI events. Handlers are read through a ref so the latest closure (with current
 * loop / camera state) always runs without re-subscribing.
 */
export function useRemoteCommands(handlers: RemoteCommandHandlers) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];
    const seen = new Set<string>();
    const dispatch = (cmd: OpenMeetingCommand) => {
      if (seen.has(cmd.commandId)) return;
      seen.add(cmd.commandId);
      void handlersRef.current.onOpenMeeting(cmd);
    };

    (async () => {
      const subs = await Promise.all([
        listen<OpenMeetingCommand>("remote://open-meeting", (e) => dispatch(e.payload)),
        listen("remote://show-linking", () => handlersRef.current.onShowLinking()),
        listen<LinkedPayload>("remote://linked", (e) => handlersRef.current.onLinked(e.payload)),
        listen<UnlinkedByPhonePayload>("remote://unlinked-by-phone", (e) => handlersRef.current.onUnlinkedByPhone(e.payload)),
        listen<CommandRejectedPayload>("remote://command-rejected", (e) => handlersRef.current.onRejected(e.payload)),
      ]);
      if (cancelled) {
        subs.forEach((u) => u());
        return;
      }
      unlisteners.push(...subs);
      // Startup catch-up: the server flushes a queued command as soon as the socket connects,
      // which can be before React registered the listener above.
      try {
        const pending = await invoke<OpenMeetingCommand | null>("remote_take_pending_command");
        if (pending && !cancelled) dispatch(pending);
      } catch (err) {
        console.error("remote: pending command lookup failed:", err);
      }
    })();

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
