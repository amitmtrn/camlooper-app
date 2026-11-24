import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Frontend side of the phone camera stream (remote v2). The state machine, timeouts and the
 * camera handoff live in Rust (`src-tauri/src/remote/stream`); this hook mirrors its status
 * and relays accept / decline / stop.
 *
 * The status is the single source of truth. The confirm dialog and the error banner are
 * derived from it, never from separate events: events and command replies reach the webview
 * on different channels in no guaranteed order, so every status carries a `seq` and anything
 * older than what was already applied is dropped.
 */

export type PhoneStreamPhase = "idle" | "requested" | "connecting" | "live" | "stopping" | "error";

export interface PhoneStreamStatus {
  /** Grows with every change the backend publishes. */
  seq: number;
  state: PhoneStreamPhase;
  sessionId: string | null;
  deviceName: string | null;
  codec: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  frames: number;
  /** Unix ms when `state` was entered. */
  since: number | null;
  /** While `requested`: Unix ms at which the request declines by itself. */
  expiresAt: number | null;
  /** Technical detail, for logs. */
  lastError: string | null;
  /** Stop reason behind `lastError`: `ice_failed`, `timeout`, `error`, … */
  lastErrorCode: string | null;
  /** The last session ended normally because the phone stopped it. Never an error. */
  endedByPhone: boolean;
  /**
   * A session the backend gave up on still holds the camera. `state` is `error`, but
   * playback and the camera are refused until it lets go.
   */
  releasing: boolean;
}

export interface PhoneStreamRequest {
  sessionId: string;
  deviceName: string | null;
  expiresAt: number;
}

export interface PhoneStreamError {
  code: string | null;
  detail: string | null;
}

const IDLE: PhoneStreamStatus = {
  seq: -1,
  state: "idle",
  sessionId: null,
  deviceName: null,
  codec: null,
  width: null,
  height: null,
  fps: null,
  frames: 0,
  since: null,
  expiresAt: null,
  lastError: null,
  lastErrorCode: null,
  endedByPhone: false,
  releasing: false,
};

/** A banner's Stop turns into a force stop this long into `stopping` (machine.rs: `FORCE_STOP_AFTER`, plus a margin). */
export const FORCE_STOP_AFTER_MS = 5_250;

/** The phone holds the camera: normal video start controls are off. */
export function phoneOwnsCamera(state: PhoneStreamPhase): boolean {
  return state === "connecting" || state === "live" || state === "stopping";
}

/**
 * The video and camera controls are off: the phone holds the camera, or a session that was
 * given up on has not handed it back yet (the backend refuses to start anything until then).
 */
export function cameraBusy(status: Pick<PhoneStreamStatus, "state" | "releasing">): boolean {
  return phoneOwnsCamera(status.state) || status.releasing === true;
}

/** Error codes with their own translated text; anything else uses the generic message. */
const KNOWN_ERRORS = ["ice_failed", "timeout"] as const;

export function phoneErrorKey(
  code: string | null,
  windows: boolean = navigator.userAgent.includes("Windows"),
): string {
  // On Windows a failed direct connection is often the firewall prompt having been
  // dismissed: inbound UDP on private networks is what the phone needs.
  if (code === "ice_failed" && windows) return "phoneStream.errors.ice_failed_windows";
  return code && (KNOWN_ERRORS as readonly string[]).includes(code)
    ? `phoneStream.errors.${code}`
    : "phoneStream.errors.generic";
}

/** `"H264"` → `"H.264"`; other codec names as reported. */
export function codecLabel(codec: string | null): string | null {
  if (!codec) return null;
  return codec.toUpperCase() === "H264" ? "H.264" : codec;
}

/** Elapsed time as `mm:ss`, or `h:mm:ss` from one hour on. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

/**
 * Wrap a phone-provided name in FSI … PDI so its direction cannot reorder the sentence it is
 * put into (a Hebrew name in an English sentence, a Latin name in an Arabic one).
 */
export function isolate(text: string): string {
  return `\u2068${text}\u2069`;
}

/** Whether `next` may replace `current`: never go back to an older snapshot. */
export function isNewerOrSame(next: PhoneStreamStatus, current: PhoneStreamStatus): boolean {
  return next.seq >= current.seq;
}

/** The request awaiting a decision, from the status alone. */
export function requestFromStatus(s: PhoneStreamStatus): PhoneStreamRequest | null {
  if (s.state !== "requested" || !s.sessionId || s.expiresAt == null) return null;
  return { sessionId: s.sessionId, deviceName: s.deviceName, expiresAt: s.expiresAt };
}

/**
 * Identity of one request. The phone chooses the session id and may use it again, so the id
 * alone does not tell a request that was declined here from a new one; the deadline does.
 */
export function requestKey(r: Pick<PhoneStreamRequest, "sessionId" | "expiresAt">): string {
  return `${r.sessionId}@${r.expiresAt}`;
}

/**
 * - `expired`: the request is gone.
 * - `offline` / `too_soon`: not accepted, the request is still pending and the dialog stays.
 * - `failed`: anything else (backend not ready, …); the status is re-read.
 */
export type AcceptResult = "ok" | "expired" | "offline" | "too_soon" | "failed";

function acceptResult(err: unknown): AcceptResult {
  if (err === "offline" || err === "too_soon") return err;
  return err === "no_such_request" ? "expired" : "failed";
}

/** Dismissed errors are remembered by when they happened, across re-reads and reloads. */
const DISMISSED_ERROR_KEY = "camlooper.phoneStream.dismissedErrorSince";

function readDismissedError(): number | null {
  try {
    const v = Number(sessionStorage.getItem(DISMISSED_ERROR_KEY));
    return Number.isFinite(v) && v > 0 ? v : null;
  } catch {
    return null;
  }
}

export function usePhoneStream(options: { onStoppedByPhone?: () => void; onStopFailed?: () => void } = {}) {
  const callbacks = useRef(options);
  callbacks.current = options;
  const latest = useRef<PhoneStreamStatus>(IDLE);
  const [status, setStatus] = useState<PhoneStreamStatus>(IDLE);
  /** The request ([`requestKey`]) whose dialog was closed here (declined, or its countdown ran out). */
  const [closedRequest, setClosedRequest] = useState<string | null>(null);
  /** The request whose accept is on its way, or done and waiting for the status to move on. */
  const [acceptingRequest, setAcceptingRequest] = useState<string | null>(null);
  const [dismissedErrorSince, setDismissedErrorSince] = useState<number | null>(readDismissedError);

  const applyStatus = useCallback((s: PhoneStreamStatus) => {
    const before = latest.current;
    // An older snapshot arriving late (a command reply overtaken by an event) must not
    // bring back a banner or a dialog that is already gone.
    if (!isNewerOrSame(s, before)) return;
    latest.current = s;
    // The phone ended a session that was up a moment ago: say so, neutrally. (Not on the
    // first read after startup, where there is no "moment ago".)
    if (s.state === "idle" && s.endedByPhone && phoneOwnsCamera(before.state)) {
      callbacks.current.onStoppedByPhone?.();
    }
    // Both marks are about one pending request. Once the status has moved on, drop them,
    // so they cannot apply to a later request that uses the same session id.
    if (s.state !== "requested") {
      setClosedRequest(null);
      setAcceptingRequest(null);
    }
    setStatus(s);
  }, []);

  /** The key of the pending request `sessionId` names, as far as the latest status knows. */
  const pendingKey = useCallback((sessionId: string): string | null => {
    const r = requestFromStatus(latest.current);
    return r && r.sessionId === sessionId ? requestKey(r) : null;
  }, []);

  /** Re-read the status: at startup, and as a self-heal for an event that never arrived. */
  const refresh = useCallback(async () => {
    try {
      applyStatus(await invoke<PhoneStreamStatus>("remote_stream_status"));
    } catch (err) {
      console.error("phone stream: could not read status:", err);
    }
  }, [applyStatus]);

  useEffect(() => {
    let cancelled = false;
    let unlisteners: UnlistenFn[] = [];
    const onFocus = () => void refresh();
    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    (async () => {
      // Subscribe first, then read, so a change during startup is not lost.
      const subs = await Promise.all([
        listen<PhoneStreamStatus>("remote://stream-status", (e) => applyStatus(e.payload)),
        listen("app://shown", () => void refresh()),
        listen("remote://stream-stop-failed", () => callbacks.current.onStopFailed?.()),
      ]);
      if (cancelled) {
        subs.forEach((u) => u());
        return;
      }
      unlisteners = subs;
      window.addEventListener("focus", onFocus);
      document.addEventListener("visibilitychange", onVisible);
      await refresh();
    })();
    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [applyStatus, refresh]);

  const request = useMemo(() => {
    const r = requestFromStatus(status);
    return r && requestKey(r) !== closedRequest ? r : null;
  }, [status, closedRequest]);
  const accepting = request !== null && requestKey(request) === acceptingRequest;

  /** The last failure, until the user dismisses it or a new stream starts. */
  const error = useMemo<PhoneStreamError | null>(
    () =>
      status.state === "error" && status.since !== dismissedErrorSince
        ? { code: status.lastErrorCode, detail: status.lastError }
        : null,
    [status, dismissedErrorSince],
  );

  const accept = useCallback(
    async (sessionId: string): Promise<AcceptResult> => {
      // The dialog stays, busy, until the status leaves `requested` — accepting can sit
      // behind a system prompt for a while, and a failure must not leave nothing on screen.
      setAcceptingRequest(pendingKey(sessionId));
      let result: AcceptResult = "ok";
      try {
        await invoke("remote_stream_accept", { sessionId });
      } catch (err) {
        console.error("phone stream: accept failed:", err);
        result = acceptResult(err);
        setAcceptingRequest(null);
      }
      // Whatever happened, show what the backend now says (the dialog again if the request
      // is still pending, nothing if it is gone).
      await refresh();
      return result;
    },
    [refresh, pendingKey],
  );

  const reject = useCallback(
    async (sessionId: string) => {
      setClosedRequest(pendingKey(sessionId));
      try {
        await invoke("remote_stream_reject", { sessionId });
      } catch (err) {
        // Already expired or cancelled: nothing left to decline.
        console.warn("phone stream: decline:", err);
        void refresh();
      }
    },
    [refresh, pendingKey],
  );

  /** The dialog's own countdown ran out: close it now; the backend's timeout is what counts. */
  const expire = useCallback(
    (sessionId: string) => {
      setClosedRequest(pendingKey(sessionId));
      void refresh();
    },
    [refresh, pendingKey],
  );

  /** Also the way out of a stop that hangs: a repeat after ~5 s forces it. */
  const stop = useCallback(async () => {
    try {
      await invoke("remote_stream_stop");
    } catch (err) {
      console.error("phone stream: stop failed:", err);
      callbacks.current.onStopFailed?.();
    }
    void refresh();
  }, [refresh]);

  const dismissError = useCallback(() => {
    const since = latest.current.since;
    setDismissedErrorSince(since);
    try {
      if (since != null) sessionStorage.setItem(DISMISSED_ERROR_KEY, String(since));
    } catch {
      // Storage unavailable: dismissed for this page load only.
    }
  }, []);

  return { status, request, accepting, error, accept, reject, expire, stop, dismissError };
}
