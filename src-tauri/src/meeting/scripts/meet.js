// Google Meet pre-join. Primary selectors are Meet's `jsname` attributes (stable across UI
// languages); text patterns in common languages are the fallback.
const inCall =
  __cl.q('button[jsname="CQylAd"]') ||
  __cl.q('[aria-label*="Leave call" i], [data-tooltip*="Leave call" i], [aria-label*="Salir de la llamada" i]');
if (inCall) return __cl.result('joined');

// "You can't join this video call" / "Check your meeting code" → nothing we can do.
if (__cl.q('button[jsname="dqt8Pb"]') || __cl.byText(/return to home screen|volver a la pantalla principal|retour à l'écran d'accueil/i)) {
  return __cl.result('blocked', __cl.bodyText().slice(0, 120));
}

// First-visit dialog "Do you want people to see and hear you in the meeting?": click the primary
// button (the one that is not "Continue without microphone and camera").
const dialog = __cl.q('[role=dialog]');
if (dialog) {
  const without = /without|sin |sans |ohne |senza |без |なし|없이|بدون|ללא|zonder|utan|uden|bez |fără|χωρίς|tanpa|không|ไม่/i;
  const buttons = [...dialog.querySelectorAll('button')].filter(__cl.vis);
  const allow = buttons.find((b) => !without.test(__cl.text(b)) && __cl.text(b).length > 0);
  if (allow && buttons.length >= 2) {
    __cl.click(allow);
    return __cl.result('clicked_allow', __cl.text(allow));
  }
}

// Signed-out: Meet asks for a name before it offers "Ask to join".
const name = __cl.q('input[jsname="YPqjbf"]') || __cl.q('input[type="text"][aria-label]');
if (name && !name.value) __cl.setValue(name, displayName);

const joinRe =
  /^(join now|ask to join|join|unirse ahora|unirme|solicitar unirse|participer|demander à participer|jetzt teilnehmen|um teilnahme bitten|participar agora|pedir para participar|partecipa|chiedi di partecipare|присоединиться|попросить присоединиться|今すぐ参加|参加をリクエスト|立即加入|请求加入|지금 참여하기|참여 요청|הצטרפות עכשיו|בקשה להצטרף|الانضمام الآن|طلب الانضمام|अभी शामिल हों|şimdi katıl|katılmak için izin iste|dołącz teraz|poproś o dołączenie|gabung sekarang|tham gia ngay|เข้าร่วมเลย)$/i;
const join = __cl.q('button[jsname="Qx7uuf"]') || __cl.byText(joinRe, document, 'button');
if (join && !join.disabled && !join.getAttribute('aria-disabled')) {
  __cl.click(join);
  __cl.state.clickedJoin = true;
  return __cl.result('clicked_join', __cl.text(join));
}

if (__cl.state.clickedJoin) return __cl.result('waiting_admission');
return __cl.result('waiting');
