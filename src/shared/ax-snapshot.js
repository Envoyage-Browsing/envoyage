// In-page accessibility snapshot: role, accessible name, value, and CSS-pixel
// center for every relevant element. Returns JSON `{title,url,items:[…]}`.
//
// SINGLE SOURCE OF TRUTH. Loaded by the Rust engine via include_str! and
// imported verbatim by the TS SDK so the DOM-role / accessible-name / ref-stamp
// heuristics can never diverge. Keep it dependency-free browser JS: one
// self-contained IIFE returning a JSON string, no imports, no framework.
(() => {
  const ROLE = (el) => {
    const r = el.getAttribute('role');
    if (r) return r;
    const t = el.tagName.toLowerCase();
    if (t === 'a' && el.href) return 'link';
    if (t === 'button') return 'button';
    if (t === 'select') return 'combobox';
    if (t === 'textarea') return 'textbox';
    if (t === 'input') {
      const it = (el.type || 'text').toLowerCase();
      if (it === 'checkbox') return 'checkbox';
      if (it === 'radio') return 'radio';
      if (it === 'submit' || it === 'button') return 'button';
      return 'textbox';
    }
    return el.tagName.toLowerCase();
  };
  const INTERACTIVE = new Set(['link','button','textbox','checkbox','radio','combobox','listbox','select','menuitem','tab','switch']);
  const NAME = (el, role, idx) => {
    const al = el.getAttribute('aria-label'); if (al) return al.trim();
    if (el.id) { const l = document.querySelector(`label[for="${el.id}"]`); if (l && l.textContent.trim()) return l.textContent.trim(); }
    const lbl = el.closest('label'); if (lbl && lbl.textContent.trim()) return lbl.textContent.trim();
    if (el.placeholder) return el.placeholder.trim();
    if (el.value && (el.tagName === 'BUTTON' || (el.tagName==='INPUT' && (el.type==='submit'||el.type==='button')))) return String(el.value).trim();
    const txt = (el.textContent || '').trim(); if (txt) return txt.slice(0, 200);
    // Icon-only controls (no aria-label/text): title → alt → role@position.
    const title = el.getAttribute('title'); if (title) return title.trim();
    const alt = el.getAttribute('alt'); if (alt) return alt.trim();
    const img = el.querySelector && el.querySelector('img[alt]'); if (img && img.alt.trim()) return img.alt.trim();
    return `${role}@${idx}`;
  };
  const items = [];
  const sel = 'a[href],button,input,select,textarea,[role],[onclick],h1,h2,h3,p,li';
  let idx = 0;
  for (const el of document.querySelectorAll(sel)) {
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) continue;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none') continue;
    const role = ROLE(el);
    // Stamp a stable handle so click/form_input can re-query the LIVE element
    // and re-measure it after reflows.
    el.setAttribute('data-immorterm-ref', String(idx));
    let value = undefined;
    if (role === 'checkbox' || role === 'radio') value = el.checked ? 'checked' : 'unchecked';
    else if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') value = String(el.value || '');
    else if (el.tagName === 'SELECT') value = String(el.value || '');
    items.push({
      role, name: NAME(el, role, idx), value, idx,
      interactive: INTERACTIVE.has(role),
      cx: Math.round(rect.x + rect.width / 2),
      cy: Math.round(rect.y + rect.height / 2),
    });
    idx++;
  }
  return JSON.stringify({ title: document.title, url: location.href, items });
})()
