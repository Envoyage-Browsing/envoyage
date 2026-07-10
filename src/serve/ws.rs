//! WS frame-stream server. Each connected client receives the protocol event
//! envelopes (`browser_frame`/`browser_cursor`/`browser_narration`/
//! `browser_human_request`/`browser_state`) as JSON text, and may send
//! [`protocol::Input`] events back (a click on the live view, a key, a scroll,
//! or a pause/continue control) which the pump dispatches to the browser.

use super::state;
use crate::protocol::Input;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Bind `127.0.0.1:port` and serve the frame stream until the process exits.
pub async fn run(port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    eprintln!("rudder: WS frame stream on ws://127.0.0.1:{port}");
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };
        tokio::spawn(async move {
            if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                handle_client(ws).await;
            }
        });
    }
}

async fn handle_client<S>(ws: tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut incoming) = ws.split();
    let mut rx = state::subscribe();
    loop {
        tokio::select! {
            // Pump -> client: forward each broadcast envelope.
            env = rx.recv() => match env {
                Ok(text) => {
                    if sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                // Lagged (slow client): skip missed frames, keep streaming.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break, // channel closed
            },
            // Client -> pump: parse human input and queue it.
            msg = incoming.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(ev) = serde_json::from_str::<Input>(&t) {
                        state::push_input(ev);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ignore binary/ping/pong
                Some(Err(_)) => break,
            },
        }
    }
}
