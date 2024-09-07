use reqwest::{Client, header::{HeaderMap, HeaderValue}};
use serde::Deserialize;
use std::error::Error as StdError;
use hmac::{Hmac, Mac};
use sha2::{Sha256, Sha512, Digest}; // Add Digest here
use base64;

// Remove this unused import
// use urlencoding::encode;

#[derive(Debug, Deserialize, Clone)]
pub struct KrakenConfig {
    pub api_key: String,
    pub api_secret: String,
}

#[derive(Debug, Deserialize)]
struct WebSocketTokenResponse {
    error: Vec<String>,
    result: Option<WebSocketTokenResult>,
}

#[derive(Debug, Deserialize)]
struct WebSocketTokenResult {
    token: String,
}

pub async fn get_websocket_token(config: &KrakenConfig) -> Result<String, Box<dyn StdError>> {
    let client = Client::new();
    let url = "https://api.kraken.com/0/private/GetWebSocketsToken";
    let endpoint = "/0/private/GetWebSocketsToken";
    
    let nonce = get_current_timestamp();
    let post_data = format!("nonce={}", nonce);
    
    let signature = generate_kraken_signature(&config.api_secret, endpoint, &nonce, &post_data);

    let mut headers = HeaderMap::new();
    headers.insert("API-Key", HeaderValue::from_str(&config.api_key)?);
    headers.insert("API-Sign", HeaderValue::from_str(&signature)?);

    let response = client.post(url)
        .headers(headers)
        .body(post_data.clone())
        .send()
        .await?;

    println!("Request URL: {}", url);
    println!("Request Headers: {:?}", response.headers());
    println!("Request Body: {}", post_data);

    let response_text = response.text().await?;
    println!("Kraken API response: {}", response_text);
    
    let token_response: WebSocketTokenResponse = serde_json::from_str(&response_text)?;

    if !token_response.error.is_empty() {
        return Err(format!("Kraken API error: {:?}", token_response.error).into());
    }

    token_response.result
        .ok_or_else(|| "No result in Kraken API response".into())
        .map(|result| result.token)
}

fn get_current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    duration.as_millis().to_string()
}

fn generate_kraken_signature(secret: &str, endpoint: &str, nonce: &str, post_data: &str) -> String {
    let secret = base64::decode(secret).unwrap();
    let mut mac = Hmac::<Sha512>::new_from_slice(&secret).unwrap();

    let mut sha256 = Sha256::new();
    sha256.update(nonce.as_bytes());
    sha256.update(post_data.as_bytes());
    let post_data_hash = sha256.finalize();

    mac.update(endpoint.as_bytes());
    mac.update(&post_data_hash);

    let signature = mac.finalize();
    base64::encode(signature.into_bytes())
}