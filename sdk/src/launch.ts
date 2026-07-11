// Node-only helper for the LOCAL OSS case: spawn a local `envoyage serve`
// process (which spawns a local headless Chromium) and return a BrowserSession
// pointed at it. This entry imports `node:child_process` and MUST be kept out of
// the Workers bundle — import it from "@envoyage/browser/launch", never from the
// core "@envoyage/browser". In a Worker you always use createSession({ cdpUrl })
// against a remote engine instead.

import { spawn, type ChildProcess } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { createSession, BrowserSession } from "./index.js";
import type { LaunchOptions } from "./types.js";

export interface LocalLaunchOptions extends Partial<LaunchOptions> {
  /**
   * Path/command for the engine CLI. Default: `envoyage` on PATH (or the
   * @envoyage/cli npm shim). Override for a local cargo build.
   */
  bin?: string;
  /** HTTP port for the engine's MCP + live-view surface. Default 8788. */
  httpPort?: number;
  /** Extra env for the spawned engine (e.g. ENVOYAGE_BROWSER_BIN). */
  env?: Record<string, string>;
  /** Seconds to wait for the engine's HTTP port to accept a request. Default 15. */
  readySecs?: number;
}

/** A locally-launched session plus its engine process handle. */
export interface LocalSession {
  session: BrowserSession;
  /** The spawned `envoyage serve` process. Killed by `stop()`. */
  process: ChildProcess;
  /** Stop the engine process (and close the session). */
  stop(): Promise<void>;
}

/**
 * Launch a local `envoyage serve` and return a session against it. For the OSS
 * single-machine case — the engine spawns and OWNS a local Chromium, so its
 * `close()` DOES tear that browser down (unlike a remote cdpUrl session). Node
 * only.
 */
export async function launch(opts: LocalLaunchOptions = {}): Promise<LocalSession> {
  const bin = opts.bin ?? "envoyage";
  const httpPort = opts.httpPort ?? 8788;
  const endpoint = opts.endpoint ?? `http://127.0.0.1:${httpPort}`;
  const readySecs = opts.readySecs ?? 15;

  const child = spawn(bin, ["serve", "--http-port", String(httpPort)], {
    env: { ...process.env, ...opts.env },
    stdio: ["ignore", "inherit", "inherit"],
  });

  await waitForReady(endpoint, opts.token, readySecs, child);

  const session = createSession({
    // Local engine spawns its own browser: no cdpUrl needed. The engine's
    // with_browser() launches locally when no --cdp-url is set. We still satisfy
    // the SDK's required field with a sentinel the engine ignores in local mode.
    cdpUrl: opts.env?.ENVOYAGE_CDP_URL ?? "local",
    endpoint,
    token: opts.token,
    sessionId: opts.sessionId,
    fetch: opts.fetch,
  });

  return {
    session,
    process: child,
    async stop() {
      await session.close().catch(() => {});
      if (!child.killed) child.kill("SIGTERM");
    },
  };
}

/** Poll the engine's /mcp endpoint until it answers or the timeout elapses. */
async function waitForReady(
  endpoint: string,
  token: string | undefined,
  readySecs: number,
  child: ChildProcess,
): Promise<void> {
  const deadline = Date.now() + readySecs * 1000;
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers["authorization"] = `Bearer ${token}`;
  const probe = JSON.stringify({ jsonrpc: "2.0", id: 0, method: "initialize", params: {} });
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`envoyage serve exited early (code ${child.exitCode})`);
    }
    try {
      const res = await fetch(`${endpoint}/mcp`, { method: "POST", headers, body: probe });
      if (res.ok) return;
    } catch {
      /* not up yet */
    }
    await delay(200);
  }
  throw new Error(`envoyage serve did not become ready on ${endpoint} within ${readySecs}s`);
}
