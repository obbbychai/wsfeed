use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::{protocol::Message}};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use serde_json::Value;
use anyhow::{Result, anyhow};  // Changed from Context to anyhow
use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};
use tokio::time::{interval, Duration};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WebSocketMessage {
    SubscriptionData {
        jsonrpc: String,
        method: String,
        params: WebSocketParams,
    },
    ResponseMessage {
        jsonrpc: String,
        id: u64,
        result: Value,
    },
    Heartbeat {
        jsonrpc: String,
        method: String,
        params: HeartbeatParams,
    },
    TestRequest {
        jsonrpc: String,
        method: String,
        params: TestRequestParams,
    },
}

#[derive(Debug, Deserialize)]
struct WebSocketParams {
    channel: String,
    data: Value,
}

#[derive(Debug, Deserialize)]
struct HeartbeatParams {
    #[serde(rename = "type")]
    heartbeat_type: String,
}

#[derive(Debug, Deserialize)]
struct TestRequestParams {
    #[serde(rename = "type")]
    test_request_type: String,
}

pub struct VolatilityManager {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    sender: mpsc::Sender<String>,
}

impl VolatilityManager {
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<String>) -> Result<Self> {
        let url = Url::parse(&config.url)
            .map_err(|e| anyhow!("Failed to parse URL: {}", e))?;
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| anyhow!("Failed to connect to WebSocket: {}", e))?;
        
        Ok(VolatilityManager { ws_stream, config, sender })
    }

    async fn send_ws_message(&mut self, message: Value) -> Result<()> {
        self.ws_stream.send(Message::Text(message.to_string()))
            .await
            .map_err(|e| anyhow!("Failed to send WebSocket message: {}", e))
    }

    async fn authenticate(&mut self) -> Result<AuthResponse> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    async fn refresh_auth_token(&mut self) -> Result<AuthResponse> {
        println!("VolatilityManager: Refreshing authentication token");
        refresh_token(&mut self.ws_stream, self.config.refresh_token.as_deref()).await
    }

    fn schedule_token_refresh(&self, duration: Duration, refresh_sender: mpsc::Sender<()>) {
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = refresh_sender.send(()).await {
                eprintln!("VolatilityManager: Failed to send refresh signal: {}", e);
            }
        });
    }

    async fn handle_heartbeat(&mut self, message_type: &str) -> Result<()> {
        match message_type {
            "heartbeat" => {
                println!("VolatilityManager: Responding to heartbeat");
                let test_message = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8212,
                    "method": "public/test",
                    "params": {}
                });
                self.send_ws_message(test_message).await?;
            }
            "test_request" => {
                println!("VolatilityManager: Responding to test request");
                let test_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8213,
                    "method": "public/test",
                    "params": {
                        "expected_result": "test_request"
                    }
                });
                self.send_ws_message(test_response).await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn start_listening(mut self) -> Result<()> {
        println!("VolatilityManager: Starting to listen");
        
        loop {
            match self.listen_internal().await {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("VolatilityManager: Connection error: {}. Attempting to reconnect...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let url = Url::parse(&self.config.url)
                        .map_err(|e| anyhow!("Failed to parse URL during reconnection: {}", e))?;
                    if let Ok((new_ws_stream, _)) = connect_async(url).await {
                        self.ws_stream = new_ws_stream;
                        if let Err(auth_err) = self.authenticate().await {
                            eprintln!("VolatilityManager: Authentication failed during reconnection: {}", auth_err);
                            continue;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn listen_internal(&mut self) -> Result<()> {
        let auth_response = self.authenticate().await?;
        
        let (refresh_sender, mut refresh_receiver) = mpsc::channel::<()>(1);
        
        let refresh_in = Duration::from_secs((auth_response.expires_in as f64 * 0.8) as u64);
        self.schedule_token_refresh(refresh_in, refresh_sender.clone());
        
        let mut interval = interval(Duration::from_secs(15));

        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["deribit_volatility_index.btc_usd"]
            }
        });
        self.send_ws_message(subscribe_message).await?;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let test_message = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 8212,
                        "method": "public/test",
                        "params": {}
                    });
                    if let Err(e) = self.send_ws_message(test_message).await {
                        eprintln!("VolatilityManager: Failed to send heartbeat: {}", e);
                        return Err(anyhow!("Failed to send heartbeat: {}", e));
                    }
                }
                Some(()) = refresh_receiver.recv() => {
                    match self.refresh_auth_token().await {
                        Ok(new_auth) => {
                            let next_refresh = Duration::from_secs((new_auth.expires_in as f64 * 0.8) as u64);
                            self.schedule_token_refresh(next_refresh, refresh_sender.clone());
                            println!("VolatilityManager: Successfully refreshed authentication token");
                        }
                        Err(e) => {
                            eprintln!("VolatilityManager: Failed to refresh token: {}", e);
                            return Err(anyhow!("Failed to refresh token: {}", e));
                        }
                    }
                }
                Some(msg) = self.ws_stream.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<WebSocketMessage>(&text) {
                                Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                                    if params.channel == "deribit_volatility_index.btc_usd" {
                                        if let Err(e) = self.sender.send(serde_json::to_string(&params.data)?).await {
                                            eprintln!("VolatilityManager: Error sending volatility data: {}", e);
                                        }
                                    }
                                },
                                Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                    println!("VolatilityManager: Received response for id {}: {:?}", id, result);
                                },
                                Ok(WebSocketMessage::Heartbeat { params, .. }) => {
                                    println!("VolatilityManager: Received heartbeat");
                                    self.handle_heartbeat(&params.heartbeat_type).await?;
                                },
                                Ok(WebSocketMessage::TestRequest { params, .. }) => {
                                    println!("VolatilityManager: Received test request");
                                    self.handle_heartbeat(&params.test_request_type).await?;
                                },
                                Err(e) => {
                                    eprintln!("VolatilityManager: Error parsing WebSocket message: {}", e);
                                    eprintln!("VolatilityManager: Received message: {}", text);
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            self.ws_stream.send(Message::Pong(data))
                                .await
                                .map_err(|e| anyhow!("Failed to send pong: {}", e))?;
                        }
                        Ok(Message::Pong(_)) => {
                            println!("VolatilityManager: Received pong");
                        }
                        Ok(Message::Close(frame)) => {
                            println!("VolatilityManager: Received close frame: {:?}", frame);
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("VolatilityManager: WebSocket error: {}", e);
                            return Err(anyhow!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                }
                else => {
                    println!("VolatilityManager: WebSocket stream ended");
                    return Ok(());
                }
            }
        }
    }
}