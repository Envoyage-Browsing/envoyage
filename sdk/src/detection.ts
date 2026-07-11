// Handoff detection helpers. The in-page probe (HUMAN_NEEDED_JS) is the ENGINE's
// single source of truth, imported verbatim (see src/shared.ts, generated from
// ../src/shared/human-needed.js). The engine already runs it server-side and
// emits a `browser_human_request` — the SDK does NOT re-run detection in the
// core path. These helpers exist for (a) classifying the engine's free-text
// `reason` into a typed HandoffKind, and (b) callers who want to probe a page
// client-side via browser_eval.

import { HUMAN_NEEDED_JS, AX_SNAPSHOT_JS } from "./shared.js";
import type { HandoffKind } from "./types.js";

export { HUMAN_NEEDED_JS, AX_SNAPSHOT_JS };

/**
 * Classify the engine's free-text handoff `reason` into a typed HandoffKind.
 * Priority matches the engine's human-needed.js: password and bot-checks
 * outrank a generic OAuth/login. Defaults to "oauth" (the lowest-priority,
 * most-generic sign-in bucket) when nothing more specific matches.
 */
export function classifyHandoff(reason: string): HandoffKind {
  const r = reason.toLowerCase();
  if (r.includes("password") || r.includes("one-time") || r.includes("otp")) {
    return "password";
  }
  if (r.includes("captcha") || r.includes("recaptcha") || r.includes("hcaptcha")) {
    return "captcha";
  }
  if (r.includes("cloudflare") || r.includes("turnstile") || r.includes("verify you are human")) {
    return "cloudflare";
  }
  return "oauth";
}

/**
 * Parse the JSON string the in-page probe (HUMAN_NEEDED_JS) returns into a
 * HandoffKind, or null when nothing needs a human. Use with the raw result of
 * evaluating HUMAN_NEEDED_JS in the page (via BrowserSession.evalRaw, when the
 * engine has ENVOYAGE_BROWSER_EVAL=1).
 */
export function parseHumanNeeded(probeResult: string): HandoffKind | null {
  try {
    const parsed = JSON.parse(probeResult) as { kind?: string };
    switch (parsed.kind) {
      case "password":
      case "captcha":
      case "cloudflare":
      case "oauth":
        return parsed.kind;
      default:
        return null;
    }
  } catch {
    return null;
  }
}
