// In-page probe for a "human must take over" state. Returns JSON `{kind}` where
// `kind` is one of `password | captcha | cloudflare | oauth`, or `{}` when
// nothing needs a human. Priority: password and captcha/cloudflare bot-checks
// outrank a generic OAuth/login URL. Defensive: any error yields `{}`.
//
// SINGLE SOURCE OF TRUTH. Loaded by the Rust engine via include_str! and
// imported verbatim by the TS SDK so the CAPTCHA/login/OAuth/password heuristics
// can never diverge between the two. Keep it dependency-free browser JS: one
// self-contained IIFE returning a JSON string, no imports, no framework.
(() => {
  try {
    const host = location.hostname.toLowerCase();
    const path = location.pathname.toLowerCase();
    const q = (sel) => { try { return !!document.querySelector(sel); } catch (e) { return false; } };
    const bodyText = (document.body && document.body.innerText || '').slice(0, 4000);

    // Password entry — highest priority; passwords must never reach the AI.
    const pw = document.querySelector('input[type=password]');
    if (pw) {
      const r = pw.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) return JSON.stringify({ kind: 'password' });
    }

    // CAPTCHA widgets.
    if (q('iframe[src*="recaptcha"]') || q('iframe[src*="hcaptcha"]')) {
      return JSON.stringify({ kind: 'captcha' });
    }

    // Cloudflare / Turnstile bot-check.
    if (host === 'challenges.cloudflare.com'
        || q('iframe[src*="challenges.cloudflare.com"]')
        || q('.cf-turnstile')
        || q('#challenge-running')
        || /verify you are human|checking your browser/i.test(bodyText)) {
      return JSON.stringify({ kind: 'cloudflare' });
    }

    // OAuth / sign-in consent — generic, lowest priority.
    const oauthHosts = ['accounts.google.com', 'login.microsoftonline.com', 'appleid.apple.com'];
    const isOauthHost = oauthHosts.includes(host)
      || (host === 'github.com' && /\/login|\/session/.test(path));
    if (isOauthHost && /oauth|authorize|login|signin/i.test(path)) {
      return JSON.stringify({ kind: 'oauth' });
    }

    return '{}';
  } catch (e) {
    return '{}';
  }
})()
