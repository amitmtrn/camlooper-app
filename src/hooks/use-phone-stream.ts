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

/**
 * What a session carries: the phone's live camera, one video file to loop, a take
 * ("Record a loop": the live camera, saved here and looped once the phone stops), or — the
 * other way round — a meeting: this computer's sound and a picked window, sent to the phone.
 */
export type PhoneStreamMode = "live" | "file" | "record" | "meeting";

export type PhoneRecordPhase = "recording" | "processing" | "looping" | "failed";

/** The take of a `record` session. */
export interface PhoneStreamRecord {
  /** How much of the take is saved so far. */
  elapsedMs: number;
  /** The longest a take may be (60 s). */
  capMs: number;
  phase: PhoneRecordPhase;
  /** Short human text, when `phase` is `failed`. */
  error?: string | null;
}

export type PhoneFilePhase = "pending" | "receiving" | "processing" | "looping" | "failed";

/** The video of a `file` session. `name` comes from the phone: display only. */
export interface PhoneStreamFile {
  name: string;
  /** Bytes the phone announced. */
  size: number;
  /** Bytes written so far. */
  received: number;
  phase: PhoneFilePhase;
  /** Short human text, when `phase` is `failed`. */
  error?: string | null;
}

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
  /** Absent from a backend that only knows live streams: read it with [`sessionMode`]. */
  mode?: PhoneStreamMode | null;
  /** Set for `file` sessions only. */
  file?: PhoneStreamFile | null;
  /** Set for `record` sessions, from the moment the take runs. */
  record?: PhoneStreamRecord | null;
}

export interface PhoneStreamRequest {
  sessionId: string;
  deviceName: string | null;
  expiresAt: number;
  mode: PhoneStreamMode;
  /** The video the phone wants to send, for `mode: "file"`. */
  file: PhoneStreamFile | null;
}

export interface PhoneStreamError {
  code: string | null;
  detail: string | null;
  /** What failed: the phone's camera (`live`) or the meeting shared with the phone. */
  mode: "live" | "meeting";
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
  mode: "live",
  file: null,
  record: null,
};

/**
 * `"file"` only when the status says so and names the file, `"record"` and `"meeting"` when
 * it says so; everything else is a live stream.
 */
export function sessionMode(status: Pick<PhoneStreamStatus, "mode" | "file">): PhoneStreamMode {
  if (status.mode === "record") return "record";
  if (status.mode === "meeting") return "meeting";
  return status.mode === "file" && status.file ? "file" : "live";
}

/** The take of a `record` session, else `null` (also before the take has started). */
export function sessionRecord(status: Pick<PhoneStreamStatus, "mode" | "file" | "record">): PhoneStreamRecord | null {
  return sessionMode(status) === "record" ? (status.record ?? null) : null;
}

/** A take's clock: `0:12`, `1:00`. */
export function formatTake(ms: number): string {
  const total = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** What the top banner shows for a `record` session, or `null` for nothing. */
export type RecordBannerView = "connecting" | "recording" | "processing" | "stopping" | null;

export function recordBannerView(status: PhoneStreamStatus): RecordBannerView {
  if (sessionMode(status) !== "record") return null;
  const phase = status.record?.phase;
  // Done, one way or the other: the toast says it, the banner goes.
  if (phase === "looping" || phase === "failed") return null;
  // The take is being turned into a loop. The session that carried it may already be over.
  if (phase === "processing" && status.state !== "error" && status.state !== "requested") return "processing";
  if (!phoneOwnsCamera(status.state)) return null;
  if (status.state === "stopping") return "stopping";
  return status.state === "live" ? "recording" : "connecting";
}

/** The file of a `file` session, else `null`. */
export function sessionFile(status: Pick<PhoneStreamStatus, "mode" | "file">): PhoneStreamFile | null {
  return sessionMode(status) === "file" ? (status.file ?? null) : null;
}

/** Whole percent received, 0–100. 100 only once every announced byte is there. */
export function fileProgressPercent(file: Pick<PhoneStreamFile, "size" | "received">): number {
  const size = Number(file.size);
  const received = Number(file.received);
  if (!(size > 0) || !(received > 0)) return 0;
  if (received >= size) return 100;
  return Math.min(99, Math.floor((received / size) * 100));
}

/** Bytes as megabytes (MiB) with one decimal: `"12.3"`; small files never read as `"0.0"`. */
export function formatMegabytes(bytes: number): string {
  const mb = Math.max(0, Number(bytes) || 0) / (1024 * 1024);
  if (mb > 0 && mb < 0.1) return "0.1";
  return mb >= 100 ? String(Math.round(mb)) : mb.toFixed(1);
}

/** What the top banner shows for a `file` session, or `null` for nothing. */
export type FileBannerView = "connecting" | "receiving" | "processing" | "stopping" | null;

export function fileBannerView(status: PhoneStreamStatus): FileBannerView {
  const file = sessionFile(status);
  if (!file || !phoneOwnsCamera(status.state)) return null;
  // Done, one way or the other: the toast says it, the banner goes.
  if (file.phase === "looping" || file.phase === "failed") return null;
  if (status.state === "stopping") return "stopping";
  if (file.phase === "processing") return "processing";
  if (file.phase === "receiving") return "receiving";
  return "connecting";
}

/** What the top banner shows for a `meeting` session, or `null` for nothing. */
export type MeetingBannerView = "connecting" | "sound" | "window" | "stopping" | null;

export function meetingBannerView(status: PhoneStreamStatus): MeetingBannerView {
  if (sessionMode(status) !== "meeting" || !phoneOwnsCamera(status.state)) return null;
  if (status.state === "stopping") return "stopping";
  if (status.state !== "live") return "connecting";
  // The backend reports a size exactly while a window is being sent.
  return status.width && status.height ? "window" : "sound";
}

/** A banner's Stop turns into a force stop this long into `stopping` (machine.rs: `FORCE_STOP_AFTER`, plus a margin). */
export const FORCE_STOP_AFTER_MS = 5_250;

/** A phone session (live or file) is up: the banner shows and Stop applies. */
export function phoneOwnsCamera(state: PhoneStreamPhase): boolean {
  return state === "connecting" || state === "live" || state === "stopping";
}

/**
 * The video and camera controls are off: the phone holds the camera, or a session that was
 * given up on has not handed it back yet (the backend refuses to start anything until then).
 */
export function cameraBusy(
  status: Pick<PhoneStreamStatus, "state" | "releasing"> & Partial<Pick<PhoneStreamStatus, "mode" | "file">>,
): boolean {
  if (status.releasing === true) return true;
  // A video on its way from the phone does not take the camera: the current loop keeps
  // playing until the new one replaces it. A take does: it is the live camera while it runs.
  // A meeting shared with the phone never touches the camera at all.
  const mode = sessionMode(status);
  return phoneOwnsCamera(status.state) && mode !== "file" && mode !== "meeting";
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

/** [`phoneErrorKey`] for a meeting session that failed. */
export function meetingErrorKey(code: string | null): string {
  if (code === "ice_failed") return phoneErrorKey(code);
  return code === "timeout" ? "phoneStream.meeting.errors.timeout" : "phoneStream.meeting.errors.generic";
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
  return {
    sessionId: s.sessionId,
    deviceName: s.deviceName,
    expiresAt: s.expiresAt,
    mode: sessionMode(s),
    file: sessionFile(s),
  };
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

/** What a new status says happened to a phone video, compared with the one before it. */
export type FileOutcome = "looping" | "failed" | null;

function fileResult(s: PhoneStreamStatus): FileOutcome {
  const file = sessionFile(s);
  if (!file) return null;
  if (file.phase === "looping") return "looping";
  // A session that broke before the transfer got anywhere (no direct connection, …) ends in
  // `error` without the file ever being marked: still a failed video.
  return file.phase === "failed" || s.state === "error" ? "failed" : null;
}

/**
 * `looping` / `failed` exactly once per transfer: when the result is first seen. A later
 * status that only repeats it (the session going idle, a re-read) is not a new result. Never
 * for the very first status after startup (`before.seq < 0`), which may describe something
 * that ended long ago.
 */
export function fileOutcome(next: PhoneStreamStatus, before: PhoneStreamStatus): FileOutcome {
  const result = fileResult(next);
  if (!result || before.seq < 0) return null;
  const sameSession = next.sessionId === before.sessionId || next.sessionId == null;
  if (fileResult(before) === result && sameSession) return null;
  return result;
}

function recordResult(s: PhoneStreamStatus): FileOutcome {
  if (sessionMode(s) !== "record") return null;
  if (s.record?.phase === "looping") return "looping";
  // A take that broke off (no direct connection, …) ends in `error` without being marked.
  return s.record?.phase === "failed" || s.state === "error" ? "failed" : null;
}

/** [`fileOutcome`] for a take: `looping` / `failed` exactly once, when it is first seen. */
export function recordOutcome(next: PhoneStreamStatus, before: PhoneStreamStatus): FileOutcome {
  const result = recordResult(next);
  if (!result || before.seq < 0) return null;
  const sameSession = next.sessionId === before.sessionId || next.sessionId == null;
  if (recordResult(before) === result && sameSession) return null;
  return result;
}

/**
 * A loaded video as the backend describes it: what `load_video_file` and
 * `stream_upload_and_load_video` return for a file picked on the desktop.
 */
export interface LoadedVideo {
  id: string;
  filename: string;
  duration: number;
  width: number;
  height: number;
  fps: number;
  format: string;
}

/** `remote://phone-video-loaded`: a video from the phone was loaded and started. */
export interface PhoneVideoLoaded {
  video: LoadedVideo;
  playing: boolean;
}

/** The payload of `remote://phone-video-loaded`, or `null` when it is not one. */
export function parsePhoneVideoLoaded(payload: unknown): PhoneVideoLoaded | null {
  const p = payload as { video?: Partial<LoadedVideo> | null; playing?: unknown } | null;
  const v = p?.video;
  if (!v || typeof v.filename !== "string") return null;
  const num = (x: unknown) => (typeof x === "number" && Number.isFinite(x) && x > 0 ? x : 0);
  return {
    video: {
      id: typeof v.id === "string" && v.id ? v.id : `phone-${Date.now()}`,
      filename: v.filename,
      duration: num(v.duration),
      width: num(v.width),
      height: num(v.height),
      fps: num(v.fps),
      format: typeof v.format === "string" ? v.format : "",
    },
    playing: p?.playing !== false,
  };
}

export interface PhoneStreamCallbacks {
  /** The phone ended a session that was up. `mode` says which kind. */
  onStoppedByPhone?: (mode: PhoneStreamMode) => void;
  onStopFailed?: () => void;
  /**
   * The backend loaded a video from the phone and started looping it. Arrives before the
   * status turns `looping` (which is what `onFileLooping` reports).
   */
  onPhoneVideoLoaded?: (video: LoadedVideo, playing: boolean) => void;
  /** The phone stopped or started the loop of the loaded video ("Stop loop" / "Play loop"). */
  onLoopChanged?: (playing: boolean) => void;
  /** A video from the phone is loaded and looping on the camera. */
  onFileLooping?: (file: PhoneStreamFile, deviceName: string | null) => void;
  /** A video from the phone could not be received or loaded. */
  onFileFailed?: (file: PhoneStreamFile, deviceName: string | null) => void;
  /** A take recorded on the phone is loaded and looping on the camera. */
  onRecordLooping?: (deviceName: string | null) => void;
  /** A take could not be turned into a loop. `error` is short human text, when there is any. */
  onRecordFailed?: (error: string | null, deviceName: string | null) => void;
}

export function usePhoneStream(options: PhoneStreamCallbacks = {}) {
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
    const outcome = fileOutcome(s, before);
    if (outcome === "looping") callbacks.current.onFileLooping?.(s.file!, s.deviceName);
    else if (outcome === "failed") callbacks.current.onFileFailed?.(s.file!, s.deviceName);
    const take = recordOutcome(s, before);
    if (take === "looping") callbacks.current.onRecordLooping?.(s.deviceName);
    else if (take === "failed") callbacks.current.onRecordFailed?.(s.record?.error ?? null, s.deviceName);
    // A video or a take that arrived, or failed with its own message, already said how it ended.
    const fileSettled =
      fileResult(s) !== null || fileResult(before) !== null || recordResult(s) !== null || recordResult(before) !== null;
    if (s.state === "idle" && s.endedByPhone && phoneOwnsCamera(before.state) && !fileSettled) {
      callbacks.current.onStoppedByPhone?.(sessionMode(before));
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
        listen<unknown>("remote://phone-video-loaded", (e) => {
          const loaded = parsePhoneVideoLoaded(e.payload);
          if (loaded) callbacks.current.onPhoneVideoLoaded?.(loaded.video, loaded.playing);
        }),
        listen<{ playing?: unknown } | null>("remote://loop-changed", (e) => {
          if (typeof e.payload?.playing === "boolean") callbacks.current.onLoopChanged?.(e.payload.playing);
        }),
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

  /**
   * The last failure, until the user dismisses it or a new stream starts. A failed video
   * transfer or take is reported through `onFileFailed` / `onRecordFailed` instead (a toast,
   * not the camera banner).
   */
  const error = useMemo<PhoneStreamError | null>(() => {
    const mode = sessionMode(status);
    return status.state === "error" && (mode === "live" || mode === "meeting") && status.since !== dismissedErrorSince
      ? { code: status.lastErrorCode, detail: status.lastError, mode }
      : null;
  }, [status, dismissedErrorSince]);

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
