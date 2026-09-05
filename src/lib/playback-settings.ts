// Playback preferences that survive a restart.
//
// The Rust side keeps playback settings in memory only, like every other setting in the
// app. Natural motion needs more than that: it defaults to on, so a user who deliberately
// turns it off would find it back on at the next launch. Storing the off state here — and
// letting the existing push effect ship it to Rust once a video is loaded — keeps the
// backend unchanged while making the switch stick.

export const NATURAL_MOTION_STORAGE_KEY = "camlooper.naturalMotion";

/** Natural motion is on unless the user has explicitly turned it off. */
export function loadNaturalMotion(): boolean {
  try {
    return localStorage.getItem(NATURAL_MOTION_STORAGE_KEY) !== "off";
  } catch {
    // Ignore storage failures (e.g. private mode / quota) and take the default.
    return true;
  }
}

export function saveNaturalMotion(enabled: boolean): void {
  try {
    localStorage.setItem(NATURAL_MOTION_STORAGE_KEY, enabled ? "on" : "off");
  } catch {
    // Ignore storage failures (e.g. private mode / quota).
  }
}
