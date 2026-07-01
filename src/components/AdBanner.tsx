import { useState } from "react";
import { X } from "lucide-react";
import { AD_BANNER_URL } from "@/lib/ads";

/**
 * Full-width ad banner shown at the very top of the app. Loads a remote ad
 * creative (image or HTML) in an iframe so ads can change without shipping an
 * app update. Dismissible for the current session.
 */
export function AdBanner() {
  const [dismissed, setDismissed] = useState(false);

  if (dismissed) return null;

  return (
    <div className="w-full bg-muted border-b border-border relative">
      <iframe
        src={AD_BANNER_URL}
        title="Advertisement"
        scrolling="no"
        className="w-full h-[90px] border-0 block"
      />
      <button
        type="button"
        onClick={() => setDismissed(true)}
        aria-label="Dismiss advertisement"
        className="absolute top-1 right-1 p-1 rounded-sm text-muted-foreground hover:text-foreground hover:bg-background/60 transition-colors"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
