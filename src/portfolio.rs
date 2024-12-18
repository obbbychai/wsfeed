use tokio_tungstenite::{connect_async, tungstenite::{protocol::Message}};
use futures_util::{StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use anyhow::{Result, anyhow};  // Changed Context to anyhow
use url::Url;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{interval, Duration};
use std::collections::HashMap;

use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioData {
    pub maintenance_margin: f64,
    pub delta_total: f64,
    pub options_session_rpl: f64,
    pub futures_session_rpl: f64,
    pub delta_total_map: HashMap<String, f64>,
    pub session_upl: f64,
    pub fee_balance: f64,
    pub estimated_liquidation_ratio: Option<f64>,
    pub initial_margin: f64,
    pub options_gamma_map: HashMap<String, f64>,
    pub futures_pl: f64,
    pub currency: String,
    pub options_value: f64,
    pub projected_maintenance_margin: f64,
    pub options_vega: f64,
    pub session_rpl: f64,
    pub futures_session_upl: f64,
    pub options_session_upl: f64,
    pub cross_collateral_enabled: bool,
    pub options_theta: f64,
    pub margin_model: String,
    pub options_delta: f64,
    pub options_pl: f64,
    pub options_vega_map: HashMap<String, f64>,
    pub balance: f64,
    pub additional_reserve: f64,
    pub estimated_liquidation_ratio_map: Option<HashMap<String, f64>>,
    pub projected_initial_margin: f64,
    pub available_funds: f64,
    pub spot_reserve: f64,
    pub projected_delta_total: f64,
    pub portfolio_margining_enabled: bool,
    pub total_pl: f64,
    pub margin_balance: f64,
    pub options_theta_map: HashMap<String, f64>,
    pub available_withdrawal_funds: f64,
    pub equity: f64,
    pub options_gamma: f64,
}

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
    data: PortfolioData,
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

pub struct PortfolioManager {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    portfolio_data: Arc<RwLock<Option<PortfolioData>>>,
    sender: mpsc::Sender<String>,
}

impl PortfolioManager {
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<String>) -> Result<Self> {
        let url = Url::parse(&config.url)
            .map_err(|e| anyhow!("Failed to parse URL: {}", e))?;
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| anyhow!("Failed to connect to WebSocket: {}", e))?;
        
        Ok(PortfolioManager {
            ws_stream,
            config,
            portfolio_data: Arc::new(RwLock::new(None)),
            sender,
        })
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
        println!("PortfolioManager: Refreshing authentication token");
        refresh_token(&mut self.ws_stream, self.config.refresh_token.as_deref()).await
    }

    fn schedule_token_refresh(&self, duration: Duration, refresh_sender: mpsc::Sender<()>) {
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = refresh_sender.send(()).await {
                eprintln!("PortfolioManager: Failed to send refresh signal: {}", e);
            }
        });
    }

    async fn handle_heartbeat(&mut self, message_type: &str) -> Result<()> {
        match message_type {
            "heartbeat" => {
                println!("PortfolioManager: Responding to heartbeat");
                let test_message = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8212,
                    "method": "public/test",
                    "params": {}
                });
                self.send_ws_message(test_message).await?;
            }
            "test_request" => {
                println!("PortfolioManager: Responding to test request");
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
        println!("PortfolioManager: Starting to listen");
        
        loop {
            match self.listen_internal().await {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("PortfolioManager: Connection error: {}. Attempting to reconnect...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let url = Url::parse(&self.config.url)
                        .map_err(|e| anyhow!("Failed to parse URL during reconnection: {}", e))?;
                    if let Ok((new_ws_stream, _)) = connect_async(url).await {
                        self.ws_stream = new_ws_stream;
                        if let Err(auth_err) = self.authenticate().await {
                            eprintln!("PortfolioManager: Authentication failed during reconnection: {}", auth_err);
                            continue;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn listen_internal(&mut self) -> Result<()> {
        // Initial authentication
        let auth_response = self.authenticate().await?;
        
        // Set up refresh channel
        let (refresh_sender, mut refresh_receiver) = mpsc::channel::<()>(1);
        
        // Schedule first token refresh (at 80% of token lifetime)
        let refresh_in = Duration::from_secs((auth_response.expires_in as f64 * 0.8) as u64);
        self.schedule_token_refresh(refresh_in, refresh_sender.clone());
        
        let mut interval = interval(Duration::from_secs(15));

        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["user.portfolio.BTC"]
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
                        eprintln!("PortfolioManager: Failed to send heartbeat: {}", e);
                        return Err(anyhow!("Failed to send heartbeat: {}", e));
                    }
                }
                Some(()) = refresh_receiver.recv() => {
                    match self.refresh_auth_token().await {
                        Ok(new_auth) => {
                            let next_refresh = Duration::from_secs((new_auth.expires_in as f64 * 0.8) as u64);
                            self.schedule_token_refresh(next_refresh, refresh_sender.clone());
                            println!("PortfolioManager: Successfully refreshed authentication token");
                        }
                        Err(e) => {
                            eprintln!("PortfolioManager: Failed to refresh token: {}", e);
                            return Err(anyhow!("Failed to refresh token: {}", e));
                        }
                    }
                }
                Some(msg) = self.ws_stream.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<WebSocketMessage>(&text) {
                                Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                                    if params.channel.starts_with("user.portfolio.") {
                                        let mut write_lock = self.portfolio_data.write().await;
                                        *write_lock = Some(params.data.clone());
                                        drop(write_lock);
                                        
                                        if let Err(e) = self.sender.send(serde_json::to_string(&params.data)?).await {
                                            eprintln!("PortfolioManager: Error sending portfolio data: {}", e);
                                        }
                                    }
                                },
                                Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                    println!("PortfolioManager: Received response for id {}: {:?}", id, result);
                                },
                                Ok(WebSocketMessage::Heartbeat { params, .. }) => {
                                    println!("PortfolioManager: Received heartbeat");
                                    self.handle_heartbeat(&params.heartbeat_type).await?;
                                },
                                Ok(WebSocketMessage::TestRequest { params, .. }) => {
                                    println!("PortfolioManager: Received test request");
                                    self.handle_heartbeat(&params.test_request_type).await?;
                                },
                                Err(e) => {
                                    eprintln!("PortfolioManager: Error parsing WebSocket message: {}", e);
                                    eprintln!("PortfolioManager: Received message: {}", text);
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            println!("PortfolioManager: Received ping, responding with pong");
                            self.ws_stream.send(Message::Pong(data))
                                .await
                                .map_err(|e| anyhow!("Failed to send pong: {}", e))?;
                        }
                        Ok(Message::Pong(_)) => {
                            println!("PortfolioManager: Received pong");
                        }
                        Ok(Message::Close(frame)) => {
                            println!("PortfolioManager: Received close frame: {:?}", frame);
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("PortfolioManager: WebSocket error: {}", e);
                            return Err(anyhow!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                }
                else => {
                    println!("PortfolioManager: WebSocket stream ended");
                    return Ok(());
                }
            }
        }
    }
}