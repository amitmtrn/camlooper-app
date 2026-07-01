// Central ad configuration. Update these URLs to point at your hosted ad endpoints.
export const AD_POPUP_URL = "https://ads.camlooper.com/popup"; // placeholder — set to your hosted popup URL
export const AD_BANNER_URL = "https://ads.camlooper.com/banner"; // placeholder — set to your hosted banner URL

// How often the hourly browser popup fires.
export const AD_INTERVAL_MS = 60 * 60 * 1000; // 1 hour

// Delay before the first popup on launch (when it's already due), so it isn't jarring on open.
export const AD_POPUP_INITIAL_DELAY_MS = 30 * 1000; // 30 seconds

// localStorage key used to persist the last time the popup was shown (across app restarts).
export const AD_POPUP_STORAGE_KEY = "camlooper.lastAdPopupShown";
