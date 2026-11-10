// Diagnostic evaluated once when auto-join gives up: the visible controls, so a selector that
// Meet/Teams/Zoom renamed can be fixed from a user's Debug log.
(() => {
  const vis = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const pick = (e) => ['data-tid', 'jsname', 'aria-label', 'id', 'placeholder', 'type'].filter((a) => e.getAttribute(a)).map((a) => a + '=' + e.getAttribute(a).slice(0, 40)).join(' ');
  const rows = [...document.querySelectorAll('button, [role=button], input')].filter(vis).slice(0, 40)
    .map((e) => e.tagName + '|' + (e.innerText || e.value || '').replace(/\s+/g, ' ').trim().slice(0, 30) + '|' + pick(e));
  return { href: location.href.slice(0, 120), title: document.title.slice(0, 60), controls: rows };
})()
