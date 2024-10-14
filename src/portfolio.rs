use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use anyhow::{Result, Context};
use url::Url;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{interval, Duration};

use crate::auth::{authenticate_with_signature, DeribitConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioData {
    pub maintenance_margin: f64,
    pub delta_total: f64,
    pub options_session_rpl: f64,
    pub futures_session_rpl: f64,
    pub delta_total_map: std::collections::HashMap<String, f64>,
    pub session_upl: f64,
    pub fee_balance: f64,
    pub estimated_liquidation_ratio: Option<f64>,
    pub initial_margin: f64,
    pub options_gamma_map: std::collections::HashMap<String, f64>,
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
    pub options_vega_map: std::collections::HashMap<String, f64>,
    pub balance: f64,
    pub additional_reserve: f64,
    pub estimated_liquidation_ratio_map: Option<std::collections::HashMap<String, f64>>,
    pub projected_initial_margin: f64,
    pub available_funds: f64,
    pub spot_reserve: f64,
    pub projected_delta_total: f64,
    pub portfolio_margining_enabled: bool,
    pub total_pl: f64,
    pub margin_balance: f64,
    pub options_theta_map: std::collections::HashMap<String, f64>,
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
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
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
            .context("Failed to send WebSocket message")
    }

    async fn authenticate(&mut self) -> Result<()> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    pub async fn start_listening(mut self) -> Result<()> {
        println!("PortfolioManager: Starting to listen");
        
        self.authenticate().await?;
        
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["user.portfolio.BTC"]
            }
        });
        self.send_ws_message(subscribe_message).await?;

        let mut interval = interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let test_message = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 8212,
                        "method": "public/test",
                        "params": {}
                    });
                    self.send_ws_message(test_message).await?;
                }
                Some(msg) = self.ws_stream.next() => {
                    let msg = msg.context("Failed to receive WebSocket message")?;
                    if let Message::Text(text) = msg {
                        match serde_json::from_str::<WebSocketMessage>(&text) {
                            Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                                if params.channel.starts_with("user.portfolio.") {
                                    let mut write_lock = self.portfolio_data.write().await;
                                    *write_lock = Some(params.data.clone());
                                    drop(write_lock);
                                    
                                    if let Err(e) = self.sender.send(serde_json::to_string(&params.data)
                                        .context("Failed to serialize portfolio data")?).await 
                                    {
                                        eprintln!("Error sending portfolio data: {}", e);
                                    }
                                }
                            },
                            Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                println!("Received response for id {}: {:?}", id, result);
                            },
                            Ok(WebSocketMessage::Heartbeat { .. }) => {
                                println!("Received heartbeat");
                            },
                            Ok(WebSocketMessage::TestRequest { .. }) => {
                                println!("Received test request");
                            },
                            Err(e) => {
                                eprintln!("Error parsing WebSocket message: {}", e);
                                eprintln!("Received message: {}", text);
                            }
                        }
                    }
                }
                else => {
                    println!("All channels closed, exiting...");
                    break;
                }
            }
        }
        Ok(())
    }
}