import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Frontend side of the phone camera stream (remote v2). The state machine, timeouts and the
 * camera handoff live in Rust (`src-tauri/src/remote/stream`); this hook mirrors its status,
 * tracks the request awaiting the user's decision and relays accept / decline / stop.
 */

export type PhoneStreamPhase = "idle" | "requested" | "connecting" | "live" | "stopping" | "error";

export interface PhoneStreamStatus {
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
  /** Technical detail, for logs. */
  lastError: string | null;
  /** Stop reason behind `lastError`: `ice_failed`, `timeout`, `error`, … */
  lastErrorCode: string | null;
  /** The last session ended normally because the phone stopped it. Never an error. */
  endedByPhone: boolean;
}

export interface PhoneStreamRequest {
  sessionId: string;
  deviceName: string | null;
  receivedAt: number;
  expiresAt: number;
}

/** Mirrors `DECISION_TIMEOUT` in machine.rs; used only to rebuild a request missed at startup. */
export const DECISION_TIMEOUT_MS = 60_000;

const IDLE: PhoneStreamStatus = {
  state: "idle",
  sessionId: null,
  deviceName: null,
  codec: null,
  width: null,
  height: null,
  fps: null,
  frames: 0,
  since: null,
  lastError: null,
  lastErrorCode: null,
  endedByPhone: false,
};

/** The phone holds the camera: normal video start controls are off. */
export function phoneOwnsCamera(state: PhoneStreamPhase): boolean {
  return state === "connecting" || state === "live" || state === "stopping";
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

/** A request is current while the status still says `requested` for that session. */
function requestFromStatus(s: PhoneStreamStatus): PhoneStreamRequest | null {
  if (s.state !== "requested" || !s.sessionId) return null;
  const receivedAt = s.since ?? Date.now();
  return { sessionId: s.sessionId, deviceName: s.deviceName, receivedAt, expiresAt: receivedAt + DECISION_TIMEOUT_MS };
}

/** `expired`: the request is gone. `offline`: not sent, the request is still pending. */
export type AcceptResult = "ok" | "expired" | "offline";

export function usePhoneStream(options: { onStoppedByPhone?: () => void } = {}) {
  const onStoppedByPhone = useRef(options.onStoppedByPhone);
  onStoppedByPhone.current = options.onStoppedByPhone;
  const previous = useRef<PhoneStreamStatus>(IDLE);
  const [status, setStatus] = useState<PhoneStreamStatus>(IDLE);
  const [request, setRequest] = useState<PhoneStreamRequest | null>(null);
  /** The last failure, until the user dismisses it or a new stream starts. */
  const [error, setError] = useState<{ code: string | null; detail: string | null } | null>(null);

  const applyStatus = useCallback((s: PhoneStreamStatus) => {
    // The phone ended a session that was up a moment ago: say so, neutrally. (Not on the
    // first read after startup, where there is no "moment ago".)
    const before = previous.current;
    previous.current = s;
    if (s.state === "idle" && s.endedByPhone && phoneOwnsCamera(before.state)) {
      onStoppedByPhone.current?.();
    }
    setStatus(s);
    // Close the dialog as soon as the backend has moved on (accepted, cancelled, timed out).
    setRequest((r) => (r && (s.state !== "requested" || s.sessionId !== r.sessionId) ? null : r));
    if (s.state === "error") setError({ code: s.lastErrorCode, detail: s.lastError });
    else if (s.state === "requested" || s.state === "connecting") setError(null);
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisteners: UnlistenFn[] = [];
    (async () => {
      // Subscribe first, then read, so a request raised during startup is not lost.
      const subs = await Promise.all([
        listen<PhoneStreamStatus>("remote://stream-status", (e) => applyStatus(e.payload)),
        listen<PhoneStreamRequest>("remote://stream-request", (e) => {
          console.log("phone stream: request from", e.payload.deviceName ?? "phone");
          setRequest(e.payload);
        }),
        listen<{ sessionId: string }>("remote://stream-request-dismissed", (e) => {
          setRequest((r) => (r?.sessionId === e.payload.sessionId ? null : r));
        }),
      ]);
      if (cancelled) {
        subs.forEach((u) => u());
        return;
      }
      unlisteners = subs;
      try {
        const current = await invoke<PhoneStreamStatus>("remote_stream_status");
        if (cancelled) return;
        applyStatus(current);
        const pending = requestFromStatus(current);
        if (pending) setRequest((r) => r ?? pending);
      } catch (err) {
        console.error("phone stream: could not read status:", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, [applyStatus]);

  const accept = useCallback(async (sessionId: string): Promise<AcceptResult> => {
    try {
      await invoke("remote_stream_accept", { sessionId });
      setRequest(null);
      return "ok";
    } catch (err) {
      console.error("phone stream: accept failed:", err);
      // The relay connection is down: the accept was not sent and the request is still
      // pending in the backend, so the dialog stays for another try.
      if (err === "offline") return "offline";
      setRequest(null);
      return "expired";
    }
  }, []);

  const reject = useCallback(async (sessionId: string) => {
    setRequest(null);
    try {
      await invoke("remote_stream_reject", { sessionId });
    } catch (err) {
      // Already expired or cancelled: nothing left to decline.
      console.warn("phone stream: decline:", err);
    }
  }, []);

  const stop = useCallback(async () => {
    try {
      await invoke("remote_stream_stop");
    } catch (err) {
      console.error("phone stream: stop failed:", err);
    }
  }, []);

  const dismissError = useCallback(() => setError(null), []);

  return { status, request, error, accept, reject, stop, dismissError };
}
