//! CDP transport abstraction: the same JSON-RPC-over-CDP conversation, over
//! either a local `--remote-debugging-pipe` (spawned Chromium) or a remote CDP
//! WebSocket (a browser envoyage did NOT spawn — e.g. Cloudflare Browser Run).
//!
//! Both transports carry the *same* CDP messages. The only difference is
//! framing:
//!   * **Pipe** — `\0`-delimited: one JSON object per NUL-terminated chunk.
//!   * **WebSocket** — one JSON object per WS text frame (NO delimiter; the WS
//!     layer already delimits messages).
//!
//! A [`CdpTransport`] exposes three transport-agnostic ops that [`BrowserSession`]
//! builds on:
//!   * [`send`](CdpTransport::send) — write one CDP message (already-serialized
//!     JSON, no NUL — the transport adds framing as needed).
//!   * [`next_frame`](CdpTransport::next_frame) — block until one complete CDP
//!     message arrives or `deadline` passes (returns `None` on timeout).
//!   * [`drain_frames`](CdpTransport::drain_frames) — non-blocking: return every
//!     complete message currently buffered, keeping any partial tail. Never
//!     sleeps. This is what the screencast pump polls.

use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

/// The two CDP transports. An enum (not a `dyn Trait`) keeps the hot
/// `send`/`next_frame`/`drain_frames` calls monomorphic and the WS half's async
/// runtime handle owned in one place.
pub enum CdpTransport {
    Pipe(PipeTransport),
    WebSocket(WsTransport),
}

impl CdpTransport {
    /// Send one CDP message. `bytes` is the serialized JSON object WITHOUT any
    /// framing — the transport adds a NUL (pipe) or wraps it in a WS frame.
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            CdpTransport::Pipe(p) => p.send(bytes),
            CdpTransport::WebSocket(w) => w.send(bytes),
        }
    }

    /// Block until one complete CDP message arrives or `deadline` passes.
    /// `Ok(None)` on timeout; `Ok(Some(bytes))` is one JSON object (no framing).
    pub fn next_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        match self {
            CdpTransport::Pipe(p) => p.next_frame(deadline),
            CdpTransport::WebSocket(w) => w.next_frame(deadline),
        }
    }

    /// Non-blocking: every complete message currently buffered, partial tail
    /// kept for the next call. Empty Vec when nothing is ready. Never sleeps.
    pub fn drain_frames(&mut self) -> Result<Vec<Vec<u8>>, String> {
        match self {
            CdpTransport::Pipe(p) => p.drain_frames(),
            CdpTransport::WebSocket(w) => w.drain_frames(),
        }
    }
}

// ── Pipe transport (local spawned Chromium) ──────────────────────────

/// `\0`-delimited CDP over the inherited fds 3/4 of a spawned Chromium.
/// The read fd is `O_NONBLOCK` so `next_frame` polls against a deadline and
/// `drain_frames` reads until `WouldBlock`.
pub struct PipeTransport {
    /// Parent write end → child's fd 3 (we send CDP here).
    write: std::fs::File,
    /// Parent read end ← child's fd 4 (we receive CDP here). O_NONBLOCK.
    read: std::fs::File,
    /// Unconsumed bytes from the read pipe (frames arrive interleaved).
    read_buf: Vec<u8>,
}

impl PipeTransport {
    pub fn new(write: std::fs::File, read: std::fs::File) -> Self {
        PipeTransport { write, read, read_buf: Vec::new() }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        // NUL-frame: one JSON object per NUL for `--remote-debugging-pipe`.
        self.write
            .write_all(bytes)
            .and_then(|()| self.write.write_all(&[0]))
            .and_then(|()| self.write.flush())
            .map_err(|e| format!("CDP send: {e}"))
    }

    fn next_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        loop {
            if let Some(frame) = pop_nul_frame(&mut self.read_buf) {
                return Ok(Some(frame));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let mut chunk = [0u8; 8192];
            match self.read.read(&mut chunk) {
                Ok(0) => return Err("CDP pipe closed (browser exited)".to_string()),
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("CDP read: {e}")),
            }
        }
    }

    fn drain_frames(&mut self) -> Result<Vec<Vec<u8>>, String> {
        loop {
            let mut chunk = [0u8; 65536];
            match self.read.read(&mut chunk) {
                Ok(0) => return Err("CDP pipe closed (browser exited)".to_string()),
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("CDP read: {e}")),
            }
        }
        let mut frames = Vec::new();
        while let Some(frame) = pop_nul_frame(&mut self.read_buf) {
            frames.push(frame);
        }
        Ok(frames)
    }
}

/// Pop one complete `\0`-delimited frame off `buf`, skipping empty frames.
/// Returns `None` when no NUL is buffered (partial tail stays in `buf`).
fn pop_nul_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    while let Some(pos) = buf.iter().position(|&b| b == 0) {
        let frame: Vec<u8> = buf.drain(..=pos).collect();
        let frame = &frame[..frame.len() - 1]; // strip NUL
        if !frame.is_empty() {
            return Some(frame.to_vec());
        }
    }
    None
}

// ── WebSocket transport (remote CDP endpoint) ────────────────────────

use futures_util::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// CDP over a remote WebSocket. Each WS text frame is exactly one CDP JSON
/// object (no NUL framing). A background tokio task owns the socket; this
/// (blocking) side talks to it over two channels, so `send`/`next_frame`/
/// `drain_frames` stay synchronous like the pipe side and slot straight into
/// `BrowserSession`'s existing blocking loop.
pub struct WsTransport {
    /// Kept alive for the lifetime of the transport; dropping it aborts the
    /// reader/writer task and closes the socket.
    _rt: Runtime,
    /// Outbound CDP messages (JSON, no framing) → WS writer task.
    out_tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Inbound CDP messages (one JSON object per item) ← WS reader task.
    in_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl WsTransport {
    /// Connect to a remote CDP WebSocket URL (`ws://` / `wss://`) and spawn the
    /// reader/writer task on a dedicated single-thread runtime. Blocks until the
    /// socket is connected (or errors).
    pub fn connect(url: &str) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| format!("build ws runtime: {e}"))?;

        let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let url = url.to_string();
        // Connect on the runtime and hand the split socket to a pump task.
        let connect = rt.block_on(async move {
            let (ws, _resp) = tokio_tungstenite::connect_async(&url)
                .await
                .map_err(|e| format!("connect CDP WS {url}: {e}"))?;
            Ok::<_, String>(ws)
        });
        let ws = connect?;

        rt.spawn(async move {
            let (mut sink, mut stream) = ws.split();
            let mut out_rx = out_rx;
            loop {
                tokio::select! {
                    // Blocking side → WS: send each outbound CDP message as text.
                    msg = out_rx.recv() => match msg {
                        Some(bytes) => {
                            let text = String::from_utf8_lossy(&bytes).into_owned();
                            if sink.send(Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                        None => break, // transport dropped
                    },
                    // WS → blocking side: forward each inbound CDP JSON object.
                    incoming = stream.next() => match incoming {
                        Some(Ok(Message::Text(t))) => {
                            if in_tx.send(t.into_bytes()).is_err() {
                                break;
                            }
                        }
                        // CDP normally uses text; accept binary defensively.
                        Some(Ok(Message::Binary(b))) => {
                            if in_tx.send(b).is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {} // ping/pong handled by tungstenite
                        Some(Err(_)) => break,
                    },
                }
            }
        });

        Ok(WsTransport { _rt: rt, out_tx, in_rx })
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.out_tx
            .send(bytes.to_vec())
            .map_err(|_| "CDP WS closed (send)".to_string())
    }

    fn next_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        loop {
            match self.in_rx.try_recv() {
                Ok(frame) => return Ok(Some(frame)),
                Err(mpsc::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err("CDP WS closed (browser exited)".to_string());
                }
            }
        }
    }

    fn drain_frames(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let mut frames = Vec::new();
        loop {
            match self.in_rx.try_recv() {
                Ok(frame) => frames.push(frame),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Surface the close the same way the pipe does, but only if
                    // we have nothing buffered to hand back first.
                    if frames.is_empty() {
                        return Err("CDP WS closed (browser exited)".to_string());
                    }
                    break;
                }
            }
        }
        Ok(frames)
    }
}

/// Set `O_NONBLOCK` on a fd so reads return `WouldBlock` instead of hanging.
/// Lives here so the pipe transport owns its own fd setup.
pub fn set_nonblocking(fd: RawFd) -> Result<(), String> {
    // SAFETY: standard fcntl on a fd we own.
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags < 0 {
        return Err("fcntl F_GETFL failed".to_string());
    }
    let rc = unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) };
    if rc < 0 {
        return Err("fcntl F_SETFL O_NONBLOCK failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Framing parity: a pipe frame (`{json}\0`) and a WS frame (`{json}`) both
    /// parse to the same CDP reply. This is the core invariant of the abstraction
    /// — the transport differs only in delimiting, not in payload.
    #[test]
    fn pipe_and_ws_framing_yield_same_object() {
        let json = br#"{"id":7,"result":{"value":2}}"#;

        // Pipe side: bytes carry a trailing NUL; pop_nul_frame strips it.
        let mut buf = Vec::new();
        buf.extend_from_slice(json);
        buf.push(0);
        let pipe_frame = pop_nul_frame(&mut buf).expect("one pipe frame");
        assert!(buf.is_empty(), "no partial tail expected");

        // WS side: the frame IS the JSON object, no delimiter.
        let ws_frame = json.to_vec();

        assert_eq!(pipe_frame, ws_frame, "same bytes after de-framing");
        let a: serde_json::Value = serde_json::from_slice(&pipe_frame).unwrap();
        let b: serde_json::Value = serde_json::from_slice(&ws_frame).unwrap();
        assert_eq!(a, b);
        assert_eq!(a["id"], 7);
    }

    /// Pipe framing: two complete frames + a partial tail. Both come out whole,
    /// empty frames are skipped, and the partial stays buffered.
    #[test]
    fn pop_nul_frame_splits_and_keeps_partial() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"{\"a\":1}\0\0{\"b\":2}\0{\"parti");
        assert_eq!(pop_nul_frame(&mut buf).unwrap(), b"{\"a\":1}");
        // The doubled NUL yields an empty frame that is skipped, not returned.
        assert_eq!(pop_nul_frame(&mut buf).unwrap(), b"{\"b\":2}");
        assert!(pop_nul_frame(&mut buf).is_none());
        assert_eq!(buf, b"{\"parti"); // partial tail preserved
    }
}
