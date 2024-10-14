use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use serde_json::Value;
use anyhow::{Result, Context};
use crate::auth::{authenticate_with_signature, DeribitConfig};
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
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
        Ok(VolatilityManager { ws_stream, config, sender })
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
        println!("VolatilityManager: Starting to listen");
        
        self.authenticate().await?;
        
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["deribit_volatility_index.btc_usd"]
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
                                if params.channel == "deribit_volatility_index.btc_usd" {
                                    self.sender.send(serde_json::to_string(&params.data)?)
                                        .await
                                        .context("Failed to send volatility data")?;
                                }
                            },
                            Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                println!("VolatilityManager: Received response for id {}: {:?}", id, result);
                            },
                            Ok(WebSocketMessage::Heartbeat { .. }) => {
                                println!("VolatilityManager: Received heartbeat");
                            },
                            Ok(WebSocketMessage::TestRequest { .. }) => {
                                println!("VolatilityManager: Received test request");
                            },
                            Err(e) => {
                                eprintln!("VolatilityManager: Error parsing WebSocket message: {}", e);
                                eprintln!("VolatilityManager: Received message: {}", text);
                            }
                        }
                    }
                }
                else => {
                    println!("VolatilityManager: All channels closed, exiting...");
                    break;
                }
            }
        }
        Ok(())
    }
}