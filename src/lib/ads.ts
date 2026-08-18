// Central ad configuration.
// These endpoints are served as static pages from the camlooper.com marketing site:
//   /ads/banner  — 90px iframe banner (see AdBanner.tsx)
// The page ships as a house ad by default and exposes an #ad-slot div where an ad-network
// snippet (AdSense, Carbon, etc.) can be dropped in without shipping a new app build.
// The ads.camlooper.com subdomain is aliased to the same Pages project as a shorter URL.
export const AD_BANNER_URL = "https://camlooper.com/ads/banner";
