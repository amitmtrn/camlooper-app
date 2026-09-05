// Central ad configuration.
// These endpoints are served as static pages from the camlooper.com marketing site:
//   /ads/banner  — 90px iframe banner (see AdBanner.tsx)
//   /ads/popup   — full page opened in the default browser (see use-welcome-popup.ts)
// The pages ship as house ads by default and expose an #ad-slot div where an ad-network
// snippet (AdSense, Carbon, etc.) can be dropped in without shipping a new app build.
// The ads.camlooper.com subdomain is aliased to the same Pages project as a shorter URL.
export const AD_BANNER_URL = "https://camlooper.com/ads/banner";
export const AD_POPUP_URL = "https://camlooper.com/ads/popup";

/** Let the app window paint and take focus before the browser steals it. */
export const WELCOME_POPUP_DELAY_MS = 2000;

/** Floor between welcome popups, so restarting the app repeatedly doesn't reopen it. */
export const WELCOME_POPUP_MIN_INTERVAL_MS = 6 * 60 * 60 * 1000;

export const WELCOME_POPUP_STORAGE_KEY = "camlooper.lastWelcomePopupShown";
