import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { FileVideo, Loader2, MonitorUp, Smartphone, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  codecLabel,
  fileBannerView,
  fileProgressPercent,
  FORCE_STOP_AFTER_MS,
  formatElapsed,
  formatMegabytes,
  formatTake,
  isolate,
  meetingBannerView,
  meetingErrorKey,
  phoneErrorKey,
  recordBannerView,
  sessionFile,
  type PhoneStreamRecord,
  type PhoneStreamError,
  type PhoneStreamRequest,
  type PhoneStreamStatus,
} from "@/hooks/use-phone-stream";

/**
 * Accept stays disabled this long after the dialog is in front of the user: after it mounts,
 * after the window gains focus, becomes visible, or is raised from the tray — whichever is
 * latest. A click or keystroke that was already on its way to another app when the window
 * appeared must not start the camera. (The backend refuses anything under 800 ms as well.)
 */
const ACCEPT_ARM_MS = 1200;

/** Re-render once a second while `active`; returns the current wall clock. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [active]);
  return now;
}

/**
 * How long the take is, counted on between the statuses that report it: the backend says
 * `elapsedMs` now and then, the banner shows every second. Never past the cap.
 */
function useTakeElapsed(record: PhoneStreamRecord | null | undefined, running: boolean): number {
  const reported = record ? Math.max(0, Number(record.elapsedMs) || 0) : null;
  const cap = record && Number(record.capMs) > 0 ? Number(record.capMs) : 60_000;
  const anchor = useRef<{ reported: number; at: number } | null>(null);
  if (!running) anchor.current = null;
  else if (!anchor.current || (reported !== null && reported !== anchor.current.reported)) {
    // No report yet: the take starts counting from the moment it is live here.
    anchor.current = { reported: reported ?? 0, at: Date.now() };
  }
  const now = useNow(running);
  const a = anchor.current;
  return a ? Math.min(cap, a.reported + Math.max(0, now - a.at)) : 0;
}

/** Whole seconds until `deadline`, re-rendered exactly when the number changes. */
function useSecondsLeft(deadline: number): number {
  const left = () => Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
  const [seconds, setSeconds] = useState(left);
  useEffect(() => {
    let id: number | undefined;
    const tick = () => {
      const remaining = deadline - Date.now();
      const s = Math.max(0, Math.ceil(remaining / 1000));
      setSeconds(s);
      // Wake just past the next second boundary of the deadline, not a second from now: a
      // free-running interval drifts against it and shows a number twice or skips one.
      if (s > 0) id = window.setTimeout(tick, remaining - (s - 1) * 1000 + 5);
    };
    tick();
    return () => window.clearTimeout(id);
  }, [deadline]);
  return seconds;
}

/** `performance.now()` from which Accept may be used; see [`ACCEPT_ARM_MS`]. */
function useAcceptArming(): { armed: boolean; isArmed: () => boolean } {
  const armAt = useRef(performance.now() + ACCEPT_ARM_MS);
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    let timer: number | undefined;
    let cancelled = false;
    const rearm = () => {
      armAt.current = performance.now() + ACCEPT_ARM_MS;
      setArmed(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setArmed(true), ACCEPT_ARM_MS);
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") rearm();
    };
    rearm();
    window.addEventListener("focus", rearm);
    document.addEventListener("visibilitychange", onVisibility);
    let unlisten: (() => void) | undefined;
    void listen("app://shown", rearm).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      window.removeEventListener("focus", rearm);
      document.removeEventListener("visibilitychange", onVisibility);
      unlisten?.();
    };
  }, []);
  // The click handler asks the clock too: the event that focuses the window and the click
  // it carries can arrive before React has re-rendered the button as disabled.
  return { armed, isArmed: () => performance.now() >= armAt.current };
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * "Use your phone as the camera?" — or, for a video the phone wants to send, "Loop a video
 * from <phone>?" with its name and size, or, for a take, "Record a loop from <phone>?", or,
 * for a meeting, "Share the meeting with <phone>?" with the reminder that nobody else in the
 * meeting is told — shown while the backend waits for a decision (60 s).
 *
 * A real modal: rendered in a portal on `<body>` with the app root made `inert`, so nothing
 * behind it can be reached by Tab or click and no other overlay can cover it. (Dropdowns
 * live in their own portals outside the root; `SelectSuppressedContext` closes those.) Deny by
 * default: focus starts on the dialog itself (never on Accept), Escape declines, and the
 * countdown running out closes it. Mount it with `key={requestKey(request)}` so every request
 * gets a fresh guard, also one that reuses a session id.
 */
export function PhoneStreamDialog({
  request,
  busy,
  onAccept,
  onDecline,
  onExpire,
}: {
  request: PhoneStreamRequest;
  /** Accept was clicked; waiting for the backend to move on. */
  busy: boolean;
  onAccept: (sessionId: string) => void;
  onDecline: (sessionId: string) => void;
  onExpire: (sessionId: string) => void;
}) {
  const { t } = useTranslation();
  const dialogRef = useRef<HTMLDivElement>(null);
  const seconds = useSecondsLeft(request.expiresAt);
  const { armed, isArmed } = useAcceptArming();
  const phone = isolate(request.deviceName || t("phoneStream.yourPhone"));
  const expired = seconds === 0;
  const { sessionId } = request;
  const file = request.mode === "file" ? request.file : null;
  const record = request.mode === "record";
  const meeting = request.mode === "meeting";

  // Modal for real: everything else inert, focus inside, focus given back afterwards.
  useEffect(() => {
    const root = document.getElementById("root");
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    root?.setAttribute("inert", "");
    dialogRef.current?.focus();
    return () => {
      root?.removeAttribute("inert");
      if (previous?.isConnected) previous.focus();
    };
  }, []);

  // A disabled button cannot keep the focus: when Accept or Decline is disabled under it
  // (busy, or Accept re-arming) the focus falls to <body>, outside the dialog, and Escape
  // and Tab would stop working. Put it back on the dialog whenever it has left.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.contains(document.activeElement)) dialog.focus();
  }, [busy, armed, expired]);

  // The backend's timeout is what counts, but do not sit at "0 s" waiting to hear of it.
  useEffect(() => {
    if (expired) onExpire(sessionId);
  }, [expired, sessionId, onExpire]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    // A key held down since before the dialog appeared decides nothing.
    if (e.repeat && (e.key === "Enter" || e.key === " " || e.key === "Escape")) {
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (!busy) onDecline(sessionId);
      return;
    }
    if (e.key === "Tab") {
      const items = Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []);
      if (items.length === 0) {
        e.preventDefault();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || active === dialogRef.current)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  // Screen readers hear the limit when the dialog opens (it is part of the description) and
  // again at 30 s and 10 s — not every second.
  const milestone = seconds <= 10 ? 10 : seconds <= 30 ? 30 : null;

  return createPortal(
    <div
      ref={dialogRef}
      tabIndex={-1}
      onKeyDownCapture={onKeyDown}
      // `pointer-events-auto`: an overlay that was open when the request arrived may have
      // set `pointer-events: none` on <body>, which this portal would inherit.
      className="pointer-events-auto fixed inset-0 z-[100] flex items-center justify-center bg-background/80 backdrop-blur-sm p-6 outline-none"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="phone-stream-dialog-title"
      aria-describedby="phone-stream-dialog-desc phone-stream-dialog-countdown"
    >
      <Card className="max-w-md w-full max-h-[90vh] overflow-y-auto p-6 space-y-4">
        <div className="flex items-center gap-3">
          {meeting ? (
            <MonitorUp className="h-6 w-6 text-primary shrink-0" />
          ) : (
            <Smartphone className="h-6 w-6 text-primary shrink-0" />
          )}
          <h2 id="phone-stream-dialog-title" className="text-lg font-semibold">
            {file
              ? t("phoneStream.file.confirmTitle", { phone })
              : record
                ? t("phoneStream.record.confirmTitle", { phone })
                : meeting
                  ? t("phoneStream.meeting.confirmTitle", { phone })
                  : t("phoneStream.confirmTitle")}
          </h2>
        </div>
        {/* Name and size are part of the description: what is being accepted is read out too. */}
        <div id="phone-stream-dialog-desc" className="space-y-3">
          <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">
            {file
              ? t("phoneStream.file.confirmBody", { phone })
              : record
                ? t("phoneStream.record.confirmBody", { phone })
                : meeting
                  ? t("phoneStream.meeting.confirmBody", { phone })
                  : t("phoneStream.confirmBody", { phone })}
          </p>
          {/* What happens right after Accept: the system's own picker, which can be cancelled. */}
          {meeting && <p className="text-sm text-muted-foreground">{t("phoneStream.meeting.confirmPicker")}</p>}
          {file && (
            <div className="flex items-center gap-3 rounded-md border bg-muted/40 p-3">
              <FileVideo className="h-5 w-5 shrink-0 text-muted-foreground" aria-hidden="true" />
              <div className="min-w-0">
                {/* The phone chose this name: its own direction, wrapped anywhere, never markup. */}
                <p className="text-sm font-medium [overflow-wrap:anywhere]">
                  <bdi>{file.name}</bdi>
                </p>
                <p className="text-xs text-muted-foreground">
                  <bdi>{t("phoneStream.file.size", { value: formatMegabytes(file.size) })}</bdi>
                </p>
              </div>
            </div>
          )}
        </div>
        <p id="phone-stream-dialog-countdown" className="text-xs text-muted-foreground" aria-live="off">
          {t("phoneStream.countdown", { seconds })}
        </p>
        <p className="sr-only" role="status" aria-live="polite">
          {milestone !== null && !expired ? t("phoneStream.countdown", { seconds: milestone }) : ""}
        </p>
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="outline" disabled={busy} onClick={() => onDecline(sessionId)}>
            {t("phoneStream.decline")}
          </Button>
          <Button
            disabled={!armed || busy || expired}
            aria-busy={busy}
            onClick={() => {
              if (isArmed() && !busy && !expired) onAccept(sessionId);
            }}
          >
            {busy && <Loader2 className="h-4 w-4 animate-spin" aria-hidden="true" />}
            {t("phoneStream.accept")}
          </Button>
        </div>
      </Card>
    </div>,
    document.body,
  );
}

/**
 * Top-of-window banner while the phone owns the camera, plus the last failure. For a video
 * the phone is sending it shows the transfer instead: "Receiving video from <phone> — 45 %"
 * with a progress bar and Cancel, then "Preparing loop…". For a take: "Recording a loop from
 * <phone> — 0:12" with Cancel, then "Preparing loop…". For a meeting shared with the phone:
 * "Sharing the meeting with <phone> · sound and a window" with Stop sharing, for as long as it
 * is shared — the reminder that this computer's sound is leaving it.
 */
export function PhoneStreamBanner({
  status,
  error,
  onStop,
  onDismissError,
}: {
  status: PhoneStreamStatus;
  error: PhoneStreamError | null;
  onStop: () => void;
  onDismissError: () => void;
}) {
  const { t } = useTranslation();
  const active = status.state === "connecting" || status.state === "live" || status.state === "stopping";
  const now = useNow(status.state === "live");
  const phone = status.deviceName || t("phoneStream.yourPhone");

  // A stop that has not finished after ~5 s: Stop comes back as the way out (the backend
  // then gives up on the session instead of waiting for it).
  const stoppingSince = status.state === "stopping" ? (status.since ?? 0) : null;
  const [canForceStop, setCanForceStop] = useState(false);
  useEffect(() => {
    setCanForceStop(false);
    if (stoppingSince === null) return;
    const wait = Math.max(0, stoppingSince + FORCE_STOP_AFTER_MS - Date.now());
    const id = window.setTimeout(() => setCanForceStop(true), wait);
    return () => window.clearTimeout(id);
  }, [stoppingSince]);

  const recordView = recordBannerView(status);
  const takeMs = useTakeElapsed(status.record, recordView === "recording");

  const meetingView = meetingBannerView(status);
  if (meetingView) {
    // The camera is not involved, so this is not the camera-in-use red: it is the app's own
    // accent, and it stays up for the whole time something is being sent to the phone.
    const sharing = meetingView === "sound" || meetingView === "window";
    return (
      <div className="sticky top-0 z-40 border-b border-primary/40 bg-primary/15 backdrop-blur-sm">
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-center gap-3">
          {sharing ? (
            <MonitorUp className="h-4 w-4 text-primary shrink-0" aria-hidden="true" />
          ) : (
            <Loader2 className="h-4 w-4 animate-spin text-primary shrink-0" aria-hidden="true" />
          )}
          <div className="flex-1 min-w-0">
            <p className="text-sm [overflow-wrap:anywhere]">
              <span className="font-semibold" role="status" aria-live="polite">
                {sharing
                  ? t("phoneStream.meeting.live", { phone: isolate(phone) })
                  : meetingView === "stopping"
                    ? t("phoneStream.meeting.stopping")
                    : t("phoneStream.connecting", { phone: isolate(phone) })}
                {sharing && (
                  <>
                    {" · "}
                    {t(meetingView === "window" ? "phoneStream.meeting.soundAndWindow" : "phoneStream.meeting.soundOnly")}
                  </>
                )}
              </span>
              {sharing && status.since ? (
                <span className="text-muted-foreground" aria-hidden="true">
                  {" · "}
                  <span dir="ltr">{formatElapsed(now - status.since)}</span>
                </span>
              ) : null}
            </p>
            {sharing && <p className="text-xs text-muted-foreground">{t("phoneStream.meeting.notice")}</p>}
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={onStop}
            disabled={status.state === "stopping" && !canForceStop}
            className="shrink-0"
          >
            <Square className="h-3 w-3 fill-current" /> {t("phoneStream.meeting.stop")}
          </Button>
        </div>
      </div>
    );
  }

  const fileView = fileBannerView(status);
  const file = sessionFile(status);
  if (file) {
    // A video on its way. Once it loops or has failed the banner is gone: a toast says so.
    if (!fileView) return null;
    const percent = fileProgressPercent(file);
    return (
      <div className="sticky top-0 z-40 border-b border-primary/40 bg-primary/15 backdrop-blur-sm">
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-center gap-3">
          <Loader2 className="h-4 w-4 animate-spin text-primary shrink-0" aria-hidden="true" />
          <div className="flex-1 min-w-0 space-y-1">
            <p className="text-sm [overflow-wrap:anywhere]">
              {fileView === "receiving" ? (
                // The number changes several times a second: not a live region. The progress
                // bar below carries the value for assistive tech.
                <span className="font-semibold">
                  {t("phoneStream.file.receiving", { phone: isolate(phone), percent })}
                </span>
              ) : (
                <span className="font-semibold" role="status" aria-live="polite">
                  {fileView === "processing"
                    ? t("phoneStream.file.preparing")
                    : fileView === "stopping"
                      ? t("phoneStream.file.cancelling")
                      : t("phoneStream.connecting", { phone: isolate(phone) })}
                </span>
              )}
              {fileView !== "connecting" && fileView !== "stopping" && (
                <span className="text-muted-foreground">
                  {" · "}
                  <bdi>{file.name}</bdi>
                </span>
              )}
            </p>
            {fileView === "receiving" && (
              <div
                className="h-1.5 w-full overflow-hidden rounded-full bg-primary/20"
                role="progressbar"
                aria-label={t("phoneStream.file.receiving", { phone: isolate(phone), percent })}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={percent}
              >
                <div className="h-full rounded-full bg-primary transition-[width] duration-200" style={{ width: `${percent}%` }} />
              </div>
            )}
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={onStop}
            disabled={status.state === "stopping" && !canForceStop}
            className="shrink-0"
          >
            <X className="h-3 w-3" /> {t("phoneStream.file.cancel")}
          </Button>
        </div>
      </div>
    );
  }

  if (recordView) {
    // A take: the phone's camera is live on the virtual camera while it is saved, so the
    // banner is the camera-in-use red. Once it loops or has failed, a toast says so.
    const recording = recordView === "recording";
    return (
      <div className="sticky top-0 z-40 border-b border-red-500/40 bg-red-500/15 backdrop-blur-sm">
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-center gap-3">
          {recording ? (
            <span className="relative flex h-3 w-3 shrink-0" aria-hidden="true">
              <span className="absolute inline-flex h-full w-full rounded-full bg-red-500 opacity-75 animate-ping" />
              <span className="relative inline-flex h-3 w-3 rounded-full bg-red-500" />
            </span>
          ) : (
            <Loader2 className="h-4 w-4 animate-spin text-red-400 shrink-0" aria-hidden="true" />
          )}
          <p className="text-sm flex-1 min-w-0 [overflow-wrap:anywhere]">
            {recording ? (
              // The clock ticks every second: not a live region.
              <span className="font-semibold">
                {t("phoneStream.record.recording", { phone: isolate(phone), time: isolate(formatTake(takeMs)) })}
              </span>
            ) : (
              <span className="font-semibold" role="status" aria-live="polite">
                {recordView === "processing"
                  ? t("phoneStream.record.preparing")
                  : recordView === "stopping"
                    ? t("phoneStream.record.cancelling")
                    : t("phoneStream.connecting", { phone: isolate(phone) })}
              </span>
            )}
          </p>
          {active && (
            <Button
              variant="outline"
              size="sm"
              onClick={onStop}
              disabled={status.state === "stopping" && !canForceStop}
              className="shrink-0"
            >
              <X className="h-3 w-3" /> {t("phoneStream.record.cancel")}
            </Button>
          )}
        </div>
      </div>
    );
  }
  // A take that is over (looping, failed): nothing here, whatever the session state says.
  if (status.mode === "record" && status.record && active && status.record.phase !== "recording") return null;

  if (active) {
    const codec = status.state === "live" ? codecLabel(status.codec) : null;
    return (
      <div className="sticky top-0 z-40 border-b border-red-500/40 bg-red-500/15 backdrop-blur-sm">
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-center gap-3">
          {status.state === "live" ? (
            <span className="relative flex h-3 w-3 shrink-0" aria-hidden="true">
              <span className="absolute inline-flex h-full w-full rounded-full bg-red-500 opacity-75 animate-ping" />
              <span className="relative inline-flex h-3 w-3 rounded-full bg-red-500" />
            </span>
          ) : (
            <Loader2 className="h-4 w-4 animate-spin text-red-400 shrink-0" aria-hidden="true" />
          )}
          <p className="text-sm flex-1 min-w-0 [overflow-wrap:anywhere]">
            {/* Only the state is a live region: the clock next to it ticks every second and
                must not be read out each time. */}
            <span className="font-semibold" role="status" aria-live="polite">
              {status.state === "live"
                ? t("phoneStream.live")
                : status.state === "connecting"
                  ? t("phoneStream.connecting", { phone: isolate(phone) })
                  : t("phoneStream.stopping")}
            </span>
            {status.state === "live" && (
              <span className="text-muted-foreground">
                {/* The name takes its own direction; sizes, codec and clock are always
                    left-to-right, whatever the name or the UI language. */}
                {" · "}
                <bdi>{phone}</bdi>
                {status.width && status.height ? (
                  <>
                    {" · "}
                    <span dir="ltr">{`${status.width}×${status.height}`}</span>
                  </>
                ) : null}
                {status.fps ? (
                  <>
                    {" · "}
                    <bdi>{t("phoneStream.fps", { value: Math.round(status.fps) })}</bdi>
                  </>
                ) : null}
                {codec && (
                  <>
                    {" · "}
                    <span dir="ltr">{codec}</span>
                  </>
                )}
                {status.since ? (
                  <span aria-hidden="true">
                    {" · "}
                    <span dir="ltr">{formatElapsed(now - status.since)}</span>
                  </span>
                ) : null}
              </span>
            )}
          </p>
          <Button
            variant="destructive"
            size="sm"
            onClick={onStop}
            disabled={status.state === "stopping" && !canForceStop}
            className="shrink-0"
          >
            <Square className="h-3 w-3 fill-current" /> {t("phoneStream.stop")}
          </Button>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="border-b border-destructive/40 bg-destructive/15" role="alert">
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-start gap-3">
          <p className="text-sm flex-1 min-w-0" title={error.detail ?? undefined}>
            <span className="font-semibold">
              {t(error.mode === "meeting" ? "phoneStream.meeting.errorTitle" : "phoneStream.errorTitle")}
            </span>{" "}
            {t(error.mode === "meeting" ? meetingErrorKey(error.code) : phoneErrorKey(error.code))}
          </p>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7 shrink-0"
            onClick={onDismissError}
            aria-label={t("phoneStream.dismiss")}
          >
            <X className="h-4 w-4" />
          </Button>
        </div>
      </div>
    );
  }

  return null;
}
