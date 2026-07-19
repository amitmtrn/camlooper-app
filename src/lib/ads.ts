// Central ad configuration.
// These endpoints are served as static pages from the camlooper.com marketing site:
//   /ads/banner  — 90px iframe banner (see AdBanner.tsx)
//   /ads/popup   — full-page popup opened in the user's default browser (see use-ad-popup.ts)
// The pages ship as house ads by default and expose an #ad-slot div where an ad-network
// snippet (AdSense, Carbon, etc.) can be dropped in without shipping a new app build.
// The ads.camlooper.com subdomain is aliased to the same Pages project as a shorter URL.
export const AD_POPUP_URL = "https://camlooper.com/ads/popup";
export const AD_BANNER_URL = "https://camlooper.com/ads/banner";

// How often the hourly browser popup fires.
export const AD_INTERVAL_MS = 60 * 60 * 1000; // 1 hour

// Delay before the first popup on launch (when it's already due), so it isn't jarring on open.
export const AD_POPUP_INITIAL_DELAY_MS = 30 * 1000; // 30 seconds

// localStorage key used to persist the last time the popup was shown (across app restarts).
export const AD_POPUP_STORAGE_KEY = "camlooper.lastAdPopupShown";
