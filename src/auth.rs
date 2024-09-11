use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::SinkExt;
use serde::Deserialize;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand::Rng;
use crate::AppError;

#[derive(Clone, Debug, Deserialize)]
pub struct DeribitConfig {
    pub url: String,
    pub client_id: String,
    pub client_secret: String,
}


pub async fn authenticate_with_signature<S>(socket: &mut S, client_id: &str, client_secret: &str) -> Result<(), AppError>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
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

    socket.send(Message::Text(auth_message.to_string())).await.map_err(|e| AppError(e.to_string()))?;
    Ok(())
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