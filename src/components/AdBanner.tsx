import { useState } from "react";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { AD_BANNER_URL } from "@/lib/ads";

/**
 * Full-width ad banner shown at the very top of the app. Loads a remote ad
 * creative (image or HTML) in an iframe so ads can change without shipping an
 * app update. Dismissible for the current session.
 */
export function AdBanner() {
  const { t, i18n } = useTranslation();
  const [dismissed, setDismissed] = useState(false);

  if (dismissed) return null;

  const lng = i18n.resolvedLanguage || i18n.language;
  const bannerSrc = lng ? `${AD_BANNER_URL}?lng=${encodeURIComponent(lng)}` : AD_BANNER_URL;

  return (
    <div className="w-full bg-muted border-b border-border relative">
      <iframe
        src={bannerSrc}
        title={t("ad.title")}
        scrolling="no"
        className="w-full h-[90px] border-0 block"
      />
      <button
        type="button"
        onClick={() => setDismissed(true)}
        aria-label={t("ad.dismiss")}
        className="absolute top-1 end-1 p-1 rounded-sm text-muted-foreground hover:text-foreground hover:bg-background/60 transition-colors"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
