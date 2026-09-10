use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tracing::{debug, error, info};

pub struct WebSocketProxy;

impl WebSocketProxy {
    pub fn is_websocket_request(headers: &axum::http::HeaderMap) -> bool {
        let upgrade_header = headers
            .get(axum::http::header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let connection_header = headers
            .get(axum::http::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        upgrade_header.contains("websocket") && connection_header.contains("upgrade")
    }

    pub async fn tunnel(client_ws: WebSocket, upstream_base_url: &str, path_and_query: &str) {
        let ws_url = if let Some(rest) = upstream_base_url.strip_prefix("https://") {
            format!("wss://{}{}", rest.trim_end_matches('/'), path_and_query)
        } else if let Some(rest) = upstream_base_url.strip_prefix("http://") {
            format!("ws://{}{}", rest.trim_end_matches('/'), path_and_query)
        } else {
            format!(
                "ws://{}{}",
                upstream_base_url.trim_end_matches('/'),
                path_and_query
            )
        };

        info!(target = %ws_url, "Connecting to upstream WebSocket");

        let (upstream_ws, _) = match connect_async(ws_url.as_str()).await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Failed to connect to upstream WebSocket {}: {}", ws_url, e);
                return;
            }
        };

        let (mut client_tx, mut client_rx) = client_ws.split();
        let (mut upstream_tx, mut upstream_rx) = upstream_ws.split();

        // Forward Client -> Upstream
        let client_to_upstream = async {
            while let Some(Ok(msg)) = client_rx.next().await {
                let tungstenite_msg = match msg {
                    AxumMessage::Text(t) => TungsteniteMessage::Text(t.as_str().into()),
                    AxumMessage::Binary(b) => TungsteniteMessage::Binary(b),
                    AxumMessage::Ping(p) => TungsteniteMessage::Ping(p),
                    AxumMessage::Pong(p) => TungsteniteMessage::Pong(p),
                    AxumMessage::Close(c) => {
                        let frame =
                            c.map(
                                |frame| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                    code: frame.code.into(),
                                    reason: frame.reason.as_str().into(),
                                },
                            );
                        let _ = upstream_tx.send(TungsteniteMessage::Close(frame)).await;
                        break;
                    }
                };
                if upstream_tx.send(tungstenite_msg).await.is_err() {
                    break;
                }
            }
        };

        // Forward Upstream -> Client
        let upstream_to_client = async {
            while let Some(Ok(msg)) = upstream_rx.next().await {
                let axum_msg = match msg {
                    TungsteniteMessage::Text(t) => AxumMessage::Text(t.as_str().into()),
                    TungsteniteMessage::Binary(b) => AxumMessage::Binary(b),
                    TungsteniteMessage::Ping(p) => AxumMessage::Ping(p),
                    TungsteniteMessage::Pong(p) => AxumMessage::Pong(p),
                    TungsteniteMessage::Close(c) => {
                        let frame = c.map(|f| axum::extract::ws::CloseFrame {
                            code: f.code.into(),
                            reason: f.reason.as_str().into(),
                        });
                        let _ = client_tx.send(AxumMessage::Close(frame)).await;
                        break;
                    }
                    TungsteniteMessage::Frame(_) => continue,
                };
                if client_tx.send(axum_msg).await.is_err() {
                    break;
                }
            }
        };

        tokio::select! {
            _ = client_to_upstream => {
                debug!("Client closed WebSocket connection");
            }
            _ = upstream_to_client => {
                debug!("Upstream closed WebSocket connection");
            }
        }
    }
}
