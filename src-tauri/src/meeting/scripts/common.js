// Shared helpers for the auto-join scripts. Rust wraps `common.js + <provider>.js` in one
// arrow IIFE and evaluates it in the meeting tab once a second, so everything here must be
// idempotent. `__DISPLAY_NAME__` is replaced with a JSON string literal before evaluation.
const displayName = __DISPLAY_NAME__;
const __cl = (() => {
  const state = (window.__camlooperJoin = window.__camlooperJoin || { clickedJoin: false, ticks: 0 });
  state.ticks++;
  const vis = (e) => {
    if (!e || !(e instanceof Element)) return false;
    const r = e.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    const cs = getComputedStyle(e);
    return cs.visibility !== 'hidden' && cs.display !== 'none';
  };
  const q = (sel, root = document) => [...root.querySelectorAll(sel)].find(vis) || null;
  const text = (e) =>
    ((e.innerText || e.textContent || '') + ' ' + (e.getAttribute('aria-label') || '')).replace(/\s+/g, ' ').trim();
  const byText = (re, root = document, sel = 'button, [role=button], a[href]') =>
    [...root.querySelectorAll(sel)].filter(vis).find((e) => re.test(text(e))) || null;
  const click = (e) => {
    if (e.focus) e.focus();
    e.click();
    return true;
  };
  // React-controlled inputs ignore plain `.value =`; go through the native setter and fire input.
  const setValue = (input, value) => {
    const proto = input.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
    setter.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const result = (status, detail) => ({ status, detail: detail ? String(detail).slice(0, 200) : null });
  const bodyText = () => (document.body ? document.body.innerText.replace(/\s+/g, ' ').trim() : '');
  return { state, vis, q, text, byText, click, setValue, result, bodyText };
})();
