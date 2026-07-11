// A minimal SSE reader over a fetch streamed Response body. Workers-safe: uses
// only ReadableStream + TextDecoder (both present in Workers and Node 18+). No
// EventSource (not in Workers), no node built-ins.
//
// SSE framing: events are separated by a blank line; each event is lines of
// `field: value`. We only need `event:` and (multi-line) `data:` per the spec.

export interface SseEvent {
  event: string;
  data: string;
}

/**
 * Consume a fetch Response's body as an SSE stream, yielding one SseEvent per
 * dispatched event. Returns when the stream ends or `signal` aborts.
 */
export async function* readSse(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<SseEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buf = "";

  const onAbort = () => reader.cancel().catch(() => {});
  if (signal) {
    if (signal.aborted) {
      await reader.cancel().catch(() => {});
      return;
    }
    signal.addEventListener("abort", onAbort, { once: true });
  }

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });

      // Dispatch every complete event (terminated by a blank line). Handle both
      // \n\n and \r\n\r\n separators.
      let sep: number;
      while ((sep = indexOfBlankLine(buf)) !== -1) {
        const rawEvent = buf.slice(0, sep);
        buf = buf.slice(sep + blankLineLength(buf, sep));
        const parsed = parseEvent(rawEvent);
        if (parsed) yield parsed;
      }
    }
  } finally {
    signal?.removeEventListener("abort", onAbort);
    reader.releaseLock?.();
  }
}

/** Index of the start of a blank-line separator, or -1. */
function indexOfBlankLine(s: string): number {
  const a = s.indexOf("\n\n");
  const b = s.indexOf("\r\n\r\n");
  if (a === -1) return b;
  if (b === -1) return a;
  return Math.min(a, b);
}

function blankLineLength(s: string, at: number): number {
  return s.startsWith("\r\n\r\n", at) ? 4 : 2;
}

function parseEvent(raw: string): SseEvent | null {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of raw.split(/\r\n|\n/)) {
    if (line === "" || line.startsWith(":")) continue; // blank or comment
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    // Per spec, one optional leading space after the colon is stripped.
    let val = colon === -1 ? "" : line.slice(colon + 1);
    if (val.startsWith(" ")) val = val.slice(1);
    if (field === "event") event = val;
    else if (field === "data") dataLines.push(val);
  }
  if (dataLines.length === 0) return null;
  return { event, data: dataLines.join("\n") };
}
