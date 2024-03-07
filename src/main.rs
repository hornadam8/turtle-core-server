mod connection;

use crate::connection::Connection;
use anyhow;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{future, pin_mut, StreamExt, TryStreamExt};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{error, info, instrument, span, warn, Level};
use turtle_protocol::{
    ForwardMessage, IntoSendable, Ping, ServerUserJoined, ServerUserLeft, ServerUsers, UserId,
    WsShell,
};

type ShellMap = Arc<Mutex<HashMap<SocketAddr, Connection>>>;

lazy_static! {
    static ref SHELL_MAP: ShellMap = Arc::new(Mutex::new(HashMap::default()));
}

const ADDR: &str = "0.0.0.0:8000";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // set up logging
    tracing_subscriber::fmt::init();
    let span = span!(Level::INFO, "server");
    let _guard = span.enter();

    let try_socket = TcpListener::bind(ADDR).await;
    let listener = try_socket.expect("Failed to bind");
    info!("Listening on: {}", ADDR);

    // Let's spawn the handling of each connection in a separate task.
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(stream, addr));
    }

    Ok(())
}

#[instrument(skip(raw_stream))]
async fn handle_connection(raw_stream: TcpStream, addr: SocketAddr) {
    let (outgoing, incoming) = set_up_stream(raw_stream, &addr).await;

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded::<Message>();
    add_to_shell_map(addr, tx.clone()).await;
    let broadcast = incoming.try_for_each(|msg| {
        let tx = tx.clone();
        async move {
            if let Ok(txt) = msg.to_text() {
                if let Ok(shell) = serde_json::from_str::<WsShell>(txt) {
                    match shell.type_.as_str() {
                        "Ping" => {
                            let ping = Ping {};
                            let resp = serde_json::to_string(&ping.into_sendable())
                                .expect("couldn't serialize ping message?!?");
                            let _ = tx.unbounded_send(Message::Text(resp));
                        }
                        "ServerUsers" => {
                            if let Ok(mut s_users) =
                                serde_json::from_value::<ServerUsers>(shell.value)
                            {
                                let mut shell_map = SHELL_MAP.lock().await;
                                if let Some(connection) = shell_map.get_mut(&addr) {
                                    connection.user_ids.append(&mut s_users.user_ids);
                                }
                            }
                        }
                        "ServerUserJoined" => {
                            if let Ok(user_joined) =
                                serde_json::from_value::<ServerUserJoined>(shell.value)
                            {
                                let new_user_id = user_joined.user_id;
                                if let Some(conn) = SHELL_MAP.lock().await.get_mut(&addr) {
                                    conn.user_ids.push(new_user_id);
                                }
                            }
                        }
                        "ServerUserLeft" => {
                            if let Ok(user_left) =
                                serde_json::from_value::<ServerUserLeft>(shell.value)
                            {
                                let left_user_id = user_left.user_id;
                                if let Some(conn) = SHELL_MAP.lock().await.get_mut(&addr) {
                                    conn.user_ids.retain(|u_id| u_id != &left_user_id);
                                }
                            }
                        }
                        "ForwardMessage" => {
                            if let Ok(fwd_msg) =
                                serde_json::from_value::<ForwardMessage>(shell.value)
                            {
                                for (_, conn) in SHELL_MAP.lock().await.iter() {
                                    let recp_users: Vec<UserId> = conn
                                        .user_ids
                                        .iter()
                                        .filter(|u_id| fwd_msg.user_ids.contains(u_id))
                                        .copied()
                                        .collect();
                                    if !recp_users.is_empty() {
                                        info!("forwarding message to {recp_users:?}");
                                        let outbound = ForwardMessage {
                                            shell: fwd_msg.shell.clone(),
                                            user_ids: recp_users,
                                        }
                                        .into_sendable();
                                        if let Ok(msg) = serde_json::to_string(&outbound) {
                                            let _ = conn.tx.unbounded_send(Message::Text(msg));
                                        } else {
                                            error!("failed to serialize forward message to server")
                                        }
                                    }
                                }
                            } else {
                                error!("Failed to deserialize value from forward message shell");
                            }
                        }
                        x => warn!("Received unexpected message type: {x}"),
                    }
                }
            }
            Ok(())
        }
    });

    let receive = rx.map(Ok).forward(outgoing);
    pin_mut!(broadcast, receive);
    future::select(broadcast, receive).await;

    info!("server: {} disconnected", addr);
    SHELL_MAP.lock().await.remove(&addr);
}

async fn add_to_shell_map(addr: SocketAddr, tx: UnboundedSender<Message>) {
    let connection = Connection {
        tx,
        user_ids: vec![],
    };
    SHELL_MAP.lock().await.insert(addr, connection);
}

#[instrument(skip(raw_stream))]
async fn set_up_stream(
    raw_stream: TcpStream,
    addr: &SocketAddr,
) -> (
    SplitSink<WebSocketStream<TcpStream>, Message>,
    SplitStream<WebSocketStream<TcpStream>>,
) {
    info!("Incoming TCP connection from: {}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    info!("WebSocket connection established: {}", addr);

    // Get sender and receiver for websocket connection
    ws_stream.split()
}
