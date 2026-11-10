import React, { forwardRef, useImperativeHandle, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Smartphone } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { RemoteErrorCode, RemoteState } from "@/hooks/use-remote";

export interface RemoteCardHandle {
  focusCode: () => void;
  scrollIntoView: () => void;
}

interface RemoteCardProps {
  state: RemoteState;
  busy: boolean;
  onLink: (code: string) => Promise<RemoteErrorCode | null>;
  onUnlink: () => Promise<void>;
  onToggleAutostart: (enabled: boolean) => Promise<boolean>;
  /** Auto-join (CamLooper-driven browser); `browserName` is null when none is installed. */
  meeting: { browserName: string | null; autoJoin: boolean; displayName: string };
  onToggleAutoJoin: (enabled: boolean) => void;
  onChangeDisplayName: (name: string) => void;
  onOpenMeetingBrowser: () => Promise<void>;
}

const CODE_LENGTH = 6;

/**
 * Advanced → Phone remote. The phone shows a 6-digit code; the user types it here. Nothing
 * touches the network until that happens (the card says so), and "Unlink phone" returns the
 * app to that state.
 */
export const RemoteCard = forwardRef<RemoteCardHandle, RemoteCardProps>(function RemoteCard(
  { state, busy, onLink, onUnlink, onToggleAutostart, meeting, onToggleAutoJoin, onChangeDisplayName, onOpenMeetingBrowser },
  ref,
) {
  const { t } = useTranslation();
  const [code, setCode] = useState("");
  const [error, setError] = useState<RemoteErrorCode | null>(null);
  const [relinking, setRelinking] = useState(false);
  const [pendingAutostart, setPendingAutostart] = useState<boolean | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);

  useImperativeHandle(ref, () => ({
    focusCode: () => inputRef.current?.focus(),
    scrollIntoView: () => cardRef.current?.scrollIntoView({ behavior: "smooth", block: "center" }),
  }));

  const linked = state.status === "linked" && !relinking;
  const linking = state.status === "linking" || busy;

  const submit = async (value: string) => {
    if (value.length !== CODE_LENGTH || linking) return;
    setError(null);
    const err = await onLink(value);
    setCode("");
    if (err) {
      setError(err);
      inputRef.current?.focus();
    } else {
      setRelinking(false);
    }
  };

  const onCodeChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const digits = event.target.value.replace(/\D/g, "").slice(0, CODE_LENGTH);
    setCode(digits);
    setError(null);
    if (digits.length === CODE_LENGTH) void submit(digits);
  };

  const toggleAutostart = async (enabled: boolean) => {
    setPendingAutostart(enabled);
    await onToggleAutostart(enabled);
    setPendingAutostart(null); // the status event carries the real value
  };

  const phone = state.mobileDeviceName ?? t("remote.yourPhone");
  const autostartShown = pendingAutostart ?? state.autostart;

  return (
    <Card className="p-4" ref={cardRef}>
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="font-semibold flex items-center">
            <Smartphone className="h-4 w-4 me-2" />
            {t("remote.title")}
          </h3>
          {state.status === "linked" && (
            <Badge
              variant={state.connected ? "default" : "secondary"}
              className={state.connected ? "bg-green-500/20 text-green-400 border-green-500/40" : ""}
            >
              {state.connected ? t("remote.badgeConnected") : t("remote.badgeOffline")}
            </Badge>
          )}
          {state.status === "linking" && <Badge variant="secondary">{t("remote.badgeLinking")}</Badge>}
        </div>

        {state.status === "disabled" ? (
          <p className="text-sm text-muted-foreground">{t("remote.errors.disabled")}</p>
        ) : linked ? (
          <div className="space-y-3">
            <div>
              <p className="text-sm">{t("remote.linkedWith", { phone })}</p>
              <p className="text-xs text-muted-foreground">{t("remote.linkedDesc")}</p>
            </div>

            <div className="flex items-center justify-between gap-4">
              <div>
                <span className="text-sm">{t("remote.autostartLabel")}</span>
                <p className="text-xs text-muted-foreground">{t("remote.autostartDesc")}</p>
              </div>
              <Switch
                checked={autostartShown}
                onCheckedChange={(v) => void toggleAutostart(v)}
                disabled={pendingAutostart !== null}
                aria-label={t("remote.autostartLabel")}
              />
            </div>

            <div className="space-y-2 border-t border-border/60 pt-3">
              <div className="flex items-center justify-between gap-4">
                <div>
                  <span className="text-sm">{t("remote.autoJoin.label")}</span>
                  <p className="text-xs text-muted-foreground">
                    {meeting.browserName
                      ? t("remote.autoJoin.desc", { browser: meeting.browserName })
                      : t("remote.autoJoin.noBrowser")}
                  </p>
                </div>
                <Switch
                  checked={meeting.autoJoin && meeting.browserName !== null}
                  onCheckedChange={onToggleAutoJoin}
                  disabled={meeting.browserName === null}
                  aria-label={t("remote.autoJoin.label")}
                />
              </div>
              {meeting.autoJoin && meeting.browserName !== null && (
                <>
                  <label className="block space-y-1">
                    <span className="text-xs text-muted-foreground">{t("remote.autoJoin.nameLabel")}</span>
                    <input
                      value={meeting.displayName}
                      onChange={(e) => onChangeDisplayName(e.target.value)}
                      placeholder={t("remote.autoJoin.namePlaceholder")}
                      maxLength={64}
                      className="w-full h-8 rounded-md border border-border bg-background/40 px-2 text-sm"
                    />
                  </label>
                  <div className="flex flex-wrap items-center gap-2">
                    <Button variant="outline" size="sm" onClick={() => void onOpenMeetingBrowser()}>
                      {t("remote.autoJoin.openBrowser")}
                    </Button>
                    <p className="text-xs text-muted-foreground flex-1 min-w-[12rem]">{t("remote.autoJoin.openBrowserHint")}</p>
                  </div>
                </>
              )}
            </div>

            <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">{t("remote.trayHint")}</p>

            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => setRelinking(true)} disabled={busy}>
                {t("remote.relink")}
              </Button>
              <Button variant="destructive" size="sm" onClick={() => void onUnlink()} disabled={busy}>
                {t("remote.unlink")}
              </Button>
            </div>
          </div>
        ) : (
          <div className="space-y-3">
            <p className="text-xs text-muted-foreground">{t("remote.networkNote")}</p>
            <p className="text-sm text-muted-foreground">{t("remote.unlinkedDesc")}</p>

            <label className="block space-y-2">
              <span className="text-sm">{t("remote.codeLabel")}</span>
              <div className="relative inline-flex gap-2" dir="ltr">
                {Array.from({ length: CODE_LENGTH }, (_, i) => (
                  <div
                    key={i}
                    aria-hidden="true"
                    className={cn(
                      "h-12 w-10 rounded-md border bg-background/40 text-2xl font-mono flex items-center justify-center",
                      i === code.length && !linking ? "border-primary ring-1 ring-primary" : "border-border",
                      error && "border-destructive",
                    )}
                  >
                    {code[i] ?? ""}
                  </div>
                ))}
                <input
                  ref={inputRef}
                  value={code}
                  onChange={onCodeChange}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void submit(code);
                  }}
                  inputMode="numeric"
                  pattern="[0-9]*"
                  autoComplete="one-time-code"
                  maxLength={CODE_LENGTH}
                  disabled={linking}
                  aria-label={t("remote.codeAria")}
                  className="absolute inset-0 w-full h-full opacity-0 cursor-text"
                />
              </div>
            </label>

            {error && (
              <p className="text-xs text-destructive" role="alert">
                {t(`remote.errors.${error}`)}
              </p>
            )}

            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" onClick={() => void submit(code)} disabled={code.length !== CODE_LENGTH || linking}>
                {linking && <Loader2 className="h-4 w-4 me-2 animate-spin" />}
                {linking ? t("remote.linking") : t("remote.linkButton")}
              </Button>
              {relinking && (
                <Button variant="ghost" size="sm" onClick={() => setRelinking(false)} disabled={linking}>
                  {t("remote.cancel")}
                </Button>
              )}
            </div>
          </div>
        )}
      </div>
    </Card>
  );
});
