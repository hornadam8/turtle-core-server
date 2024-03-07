use futures_channel::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;
use turtle_protocol::UserId;

#[derive(Clone, Debug)]
pub struct Connection {
    pub tx: UnboundedSender<Message>,
    pub user_ids: Vec<UserId>,
}
