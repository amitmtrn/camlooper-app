import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Smartphone, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  codecLabel,
  formatElapsed,
  phoneErrorKey,
  type PhoneStreamRequest,
  type PhoneStreamStatus,
} from "@/hooks/use-phone-stream";

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

/** "Use your phone as the camera?" — shown while the backend waits for a decision (60 s). */
export function PhoneStreamDialog({
  request,
  onAccept,
  onDecline,
}: {
  request: PhoneStreamRequest;
  onAccept: (sessionId: string) => void;
  onDecline: (sessionId: string) => void;
}) {
  const { t } = useTranslation();
  const now = useNow(true);
  const acceptRef = useRef<HTMLButtonElement>(null);
  const seconds = Math.max(0, Math.ceil((request.expiresAt - now) / 1000));
  const phone = request.deviceName || t("phoneStream.yourPhone");

  useEffect(() => {
    acceptRef.current?.focus();
  }, [request.sessionId]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm p-6"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="phone-stream-dialog-title"
      aria-describedby="phone-stream-dialog-desc"
    >
      <Card className="max-w-md w-full p-6 space-y-4">
        <div className="flex items-center gap-3">
          <Smartphone className="h-6 w-6 text-primary shrink-0" />
          <h2 id="phone-stream-dialog-title" className="text-lg font-semibold">
            {t("phoneStream.confirmTitle")}
          </h2>
        </div>
        <p id="phone-stream-dialog-desc" className="text-sm text-muted-foreground">
          {t("phoneStream.confirmBody", { phone })}
        </p>
        <p className="text-xs text-muted-foreground" aria-live="off">
          {t("phoneStream.countdown", { seconds })}
        </p>
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="outline" onClick={() => onDecline(request.sessionId)}>
            {t("phoneStream.decline")}
          </Button>
          <Button ref={acceptRef} onClick={() => onAccept(request.sessionId)}>
            {t("phoneStream.accept")}
          </Button>
        </div>
      </Card>
    </div>
  );
}

/** Top-of-window banner while the phone owns the camera, plus the last failure. */
export function PhoneStreamBanner({
  status,
  error,
  onStop,
  onDismissError,
}: {
  status: PhoneStreamStatus;
  error: { code: string | null; detail: string | null } | null;
  onStop: () => void;
  onDismissError: () => void;
}) {
  const { t } = useTranslation();
  const active = status.state === "connecting" || status.state === "live" || status.state === "stopping";
  const now = useNow(status.state === "live");
  const phone = status.deviceName || t("phoneStream.yourPhone");

  if (active) {
    const details: string[] = [phone];
    if (status.state === "live") {
      if (status.width && status.height) details.push(`${status.width}×${status.height}`);
      if (status.fps) details.push(t("phoneStream.fps", { value: Math.round(status.fps) }));
      const codec = codecLabel(status.codec);
      if (codec) details.push(codec);
      if (status.since) details.push(formatElapsed(now - status.since));
    }
    return (
      <div
        className="sticky top-0 z-40 border-b border-red-500/40 bg-red-500/15 backdrop-blur-sm"
        role="status"
        aria-live="polite"
      >
        <div className="max-w-6xl mx-auto px-4 py-2 flex items-center gap-3">
          {status.state === "live" ? (
            <span className="relative flex h-3 w-3 shrink-0" aria-hidden="true">
              <span className="absolute inline-flex h-full w-full rounded-full bg-red-500 opacity-75 animate-ping" />
              <span className="relative inline-flex h-3 w-3 rounded-full bg-red-500" />
            </span>
          ) : (
            <Loader2 className="h-4 w-4 animate-spin text-red-400 shrink-0" aria-hidden="true" />
          )}
          <p className="text-sm flex-1 min-w-0">
            {status.state === "live" ? (
              <>
                <span className="font-semibold">{t("phoneStream.live")}</span>
                {/* Numbers and codec stay left-to-right inside RTL text. */}
                <bdi className="text-muted-foreground"> · {details.join(" · ")}</bdi>
              </>
            ) : status.state === "connecting" ? (
              <span className="font-semibold">{t("phoneStream.connecting", { phone })}</span>
            ) : (
              <span className="font-semibold">{t("phoneStream.stopping")}</span>
            )}
          </p>
          <Button
            variant="destructive"
            size="sm"
            onClick={onStop}
            disabled={status.state === "stopping"}
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
            <span className="font-semibold">{t("phoneStream.errorTitle")}</span>{" "}
            {t(phoneErrorKey(error.code))}
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
