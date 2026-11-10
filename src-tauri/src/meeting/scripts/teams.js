// Microsoft Teams web client pre-join. `data-tid` attributes are Teams' own test hooks and do
// not change with the UI language (verified on the anonymous "light meetings" client).
const inCall = __cl.q('[data-tid="hangup-main-btn"], #hangup-button, [data-tid="call-hangup"], button[aria-label^="Leave" i]');
if (inCall && !__cl.q('button[data-tid="prejoin-join-button"]')) return __cl.result('joined');

// Dead or invalid meeting (shown in-page once the pre-join screen is gone).
const teamsError = /couldn't connect you|can't find the meeting|meeting not found|meeting has ended|invalid meeting|we couldn't join/i;
if (!__cl.q('button[data-tid="prejoin-join-button"]') && teamsError.test(__cl.bodyText())) {
  return __cl.result('blocked', __cl.bodyText().slice(0, 120));
}

// Launcher page when the Teams desktop app is registered in this profile.
const onWeb = __cl.q('button[data-tid="joinOnWeb"]') || __cl.byText(/continue on this browser|join on the web instead|use the web app instead/i);
if (onWeb) {
  __cl.click(onWeb);
  return __cl.result('clicked_web');
}

const name = __cl.q('input[data-tid="prejoin-display-name-input"]');
if (name && !name.value) __cl.setValue(name, displayName);

const video = __cl.q('input[data-tid="toggle-video"]');
if (video && !video.checked) __cl.click(video);

const join = __cl.q('button[data-tid="prejoin-join-button"]') || __cl.q('#prejoin-join-button');
if (join && !join.disabled && join.getAttribute('aria-disabled') !== 'true') {
  __cl.click(join);
  __cl.state.clickedJoin = true;
  return __cl.result('clicked_join');
}

if (__cl.state.clickedJoin) return __cl.result('waiting_admission');
return __cl.result('waiting');
