use tokio_tungstenite::tungstenite::{self, protocol::Message};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;  // Removed unused Serialize
use hmac::{Hmac, Mac};
use sha2::Sha256;
use rand::Rng;
use std::fmt::Debug;
use anyhow::{Result, anyhow};  // Added anyhow directly since we use it

#[derive(Clone, Debug, Deserialize)]
pub struct DeribitConfig {
    pub url: String,
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

pub async fn authenticate_with_signature<S>(socket: &mut S, client_id: &str, client_secret: &str) -> Result<AuthResponse>
where
    S: SinkExt<Message> + StreamExt<Item = Result<Message, tungstenite::Error>> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,  // Add this line
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

    socket.send(Message::Text(auth_message.to_string()))
        .await
        .map_err(|e| anyhow!("Failed to send authentication message: {}", e))?;

    // Wait for and process the auth response
    if let Some(msg) = socket.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let response: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| anyhow!("Failed to parse auth response: {}", e))?;
                
                if let Some(result) = response.get("result") {
                    let auth: AuthResponse = serde_json::from_value(result.clone())
                        .map_err(|e| anyhow!("Failed to parse auth response: {}", e))?;
                    return Ok(auth);
                }
            }
            _ => return Err(anyhow!("Unexpected message type during authentication")),
        }
    }

    Err(anyhow!("No response received for authentication"))
}

pub async fn refresh_token<S>(socket: &mut S, refresh_token: Option<&str>) -> Result<AuthResponse>
where
    S: SinkExt<Message> + StreamExt<Item = Result<Message, tungstenite::Error>> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,  // Add this line
{
    let refresh_token = refresh_token.ok_or_else(|| anyhow!("No refresh token available"))?;

    let refresh_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9930,
        "method": "public/auth",
        "params": {
            "grant_type": "refresh_token",
            "refresh_token": refresh_token
        }
    });

    socket.send(Message::Text(refresh_message.to_string()))
        .await
        .map_err(|e| anyhow!("Failed to send refresh message: {}", e))?;

    if let Some(msg) = socket.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let response: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| anyhow!("Failed to parse refresh response: {}", e))?;
                
                if let Some(result) = response.get("result") {
                    let auth: AuthResponse = serde_json::from_value(result.clone())
                        .map_err(|e| anyhow!("Failed to parse refresh response: {}", e))?;
                    return Ok(auth);
                }
            }
            _ => return Err(anyhow!("Unexpected message type during token refresh")),
        }
    }

    Err(anyhow!("No response received for token refresh"))
}

fn get_current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis() as u64
}

fn generate_nonce() -> String {
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    (0..8).map(|_| chars[rng.gen_range(0..chars.len())]).collect()
}

fn generate_signature(secret: &str, timestamp: u64, nonce: &str, data: &str) -> String {
    let string_to_sign = format!("{}\n{}\n{}", timestamp, nonce, data);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(string_to_sign.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}