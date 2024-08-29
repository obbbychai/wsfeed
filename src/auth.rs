use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;
use rand::Rng;
use hex;

#[derive(Debug, Deserialize)]
pub struct DeribitConfig {
    pub api_url: String,
    pub client_id: String,
    pub client_secret: String,
}

pub async fn authenticate_with_token(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, access_token: &str) {
    let auth_message = serde_json::json!({
        "id": 5647,
        "method": "private/get_subaccounts",
        "params": {
            "access_token": access_token
        }
    });

    socket.send(Message::Text(auth_message.to_string())).await.unwrap();
}

pub async fn authenticate_with_signature(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>, client_id: &str, client_secret: &str) {
    let timestamp = get_current_timestamp();
    let nonce = generate_nonce();
    let signature = generate_signature(client_secret, timestamp, &nonce, "");

    let auth_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9929,
        "method": "public/auth",
        "params": {
            "grant_type": "client_signature",
            "client_id": client_id,
            "timestamp": timestamp,
            "nonce": nonce,
            "data": "",
            "signature": signature
        }
    });

    socket.send(Message::Text(auth_message.to_string())).await.unwrap();
}

fn get_current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    duration.as_millis() as u64
}

fn generate_nonce() -> String {
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    (0..8).map(|_| chars[rng.gen_range(0..chars.len())]).collect()
}

fn generate_signature(secret: &str, timestamp: u64, nonce: &str, data: &str) -> String {
    let string_to_sign = format!("{}\n{}\n{}", timestamp, nonce, data);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(string_to_sign.as_bytes());
    let result = mac.finalize();
    let signature = hex::encode(result.into_bytes());
    signature
}
