// Zoom web client (app.zoom.us/wc/<id>/join) pre-join, plus the zoom.us launch page as a
// fallback for link shapes we did not rewrite.
const inCall = __cl.q('.footer__leave-btn, button[aria-label="Leave"], button[aria-label="End"]');
if (inCall) return __cl.result('joined');

// Invalid id / ended meeting pages ("Error - Zoom").
const zoomError = /invalid meeting id|this meeting id is not valid|meeting has ended|has been removed|could not be found|not found/i;
if (!__cl.q('button.preview-join-button, button#joinBtn') && (/^error\b/i.test(document.title) || zoomError.test(__cl.bodyText()))) {
  return __cl.result('blocked', __cl.bodyText().slice(0, 120));
}

// Launch page: prefer the web client over "Launch Meeting" (which needs the native app).
const webLink = __cl.q('a[web_client], a[href*="/wc/join"], a[href*="/wc/"][href*="join"]') || __cl.byText(/join from your browser/i);
if (webLink) {
  __cl.click(webLink);
  return __cl.result('clicked_web');
}

const name = __cl.q('#input-for-name, #inputname, input[placeholder*="name" i]');
if (name && !name.value) __cl.setValue(name, displayName);

const join = __cl.q('button.preview-join-button, button#joinBtn') || __cl.byText(/^join$/i, document, 'button');
if (join && !join.disabled) {
  __cl.click(join);
  __cl.state.clickedJoin = true;
  return __cl.result('clicked_join');
}

// Inside the client: computer audio and camera.
const audio = __cl.q('.join-audio-by-voip__join-btn') || __cl.byText(/join audio by computer|join with computer audio/i, document, 'button');
if (audio) {
  __cl.click(audio);
  return __cl.result('clicked_audio');
}
const startVideo = __cl.q('button[aria-label^="start my video" i], button[aria-label^="start video" i]');
if (startVideo) {
  __cl.click(startVideo);
  return __cl.result('clicked_video');
}

if (__cl.state.clickedJoin) return __cl.result('waiting_admission');
return __cl.result('waiting');
