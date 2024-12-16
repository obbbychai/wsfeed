use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use ordered_float::OrderedFloat;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::{self, protocol::Message}};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use anyhow::{Result, anyhow};  // Changed from Context to anyhow
use std::sync::Arc;
use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};
use tokio::time::{Duration, interval};

// Existing OrderBook and related structs remain the same
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    #[serde(with = "ordered_float_btreemap")]
    pub bids: BTreeMap<OrderedFloat<f64>, f64>,
    #[serde(with = "ordered_float_btreemap")]
    pub asks: BTreeMap<OrderedFloat<f64>, f64>,
    pub instrument_name: String,
    pub change_id: u64,
}

mod ordered_float_btreemap {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(
        value: &BTreeMap<OrderedFloat<f64>, f64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let map: Vec<(f64, f64)> = value
            .iter()
            .map(|(k, v)| (k.into_inner(), *v))
            .collect();
        map.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<OrderedFloat<f64>, f64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec = Vec::<(f64, f64)>::deserialize(deserializer)?;
        Ok(vec
            .into_iter()
            .map(|(k, v)| (OrderedFloat(k), v))
            .collect())
    }
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
struct HeartbeatParams {
    #[serde(rename = "type")]
    heartbeat_type: String,
}

#[derive(Debug, Deserialize)]
struct TestRequestParams {
    #[serde(rename = "type")]
    test_request_type: String,
}

#[derive(Debug, Deserialize)]
struct WebSocketParams {
    data: OrderBookUpdate,
    channel: String,
}

#[derive(Debug, Deserialize)]
pub struct OrderBookUpdate {
    #[serde(rename = "type")]
    pub update_type: String,
    pub timestamp: u64,
    pub instrument_name: String,
    pub change_id: u64,
    pub prev_change_id: Option<u64>,
    pub bids: Vec<Vec<Value>>,
    pub asks: Vec<Vec<Value>>,
}

// OrderBook implementation remains the same
impl OrderBook {
    pub fn new(instrument_name: String) -> Self {
        OrderBook {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            instrument_name,
            change_id: 0,
        }
    }

    pub fn update(&mut self, update: &OrderBookUpdate) -> Result<()> {
        if update.instrument_name != self.instrument_name {
            return Err(anyhow!("Instrument name mismatch"));
        }

        if let Some(prev_change_id) = update.prev_change_id {
            if prev_change_id != self.change_id {
                return Err(anyhow!("Missed update"));
            }
        }

        self.change_id = update.change_id;

        self.process_changes(&update.bids, true)?;
        self.process_changes(&update.asks, false)?;

        Ok(())
    }

    fn process_changes(&mut self, changes: &[Vec<Value>], is_bid: bool) -> Result<()> {
        let book = if is_bid { &mut self.bids } else { &mut self.asks };

        for change in changes {
            if change.len() != 3 {
                return Err(anyhow!("Invalid change format"));
            }

            let action = change[0].as_str().ok_or_else(|| anyhow!("Invalid action"))?;
            let price: OrderedFloat<f64> = OrderedFloat(change[1].as_f64().ok_or_else(|| anyhow!("Invalid price"))?);
            let amount: f64 = change[2].as_f64().ok_or_else(|| anyhow!("Invalid amount"))?;

            match action {
                "new" | "change" => {
                    if amount > 0.0 {
                        book.insert(price, amount);
                    } else {
                        book.remove(&price);
                    }
                }
                "delete" => {
                    book.remove(&price);
                }
                _ => return Err(anyhow!("Unknown action")),
            }
        }

        Ok(())
    }

    pub fn get_mid_price(&self) -> Option<f64> {
        let best_bid = self.bids.keys().next_back()?;
        let best_ask = self.asks.keys().next()?;
        Some((best_bid.0 + best_ask.0) / 2.0)
    }
}

pub struct OrderBookManager {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    sender: mpsc::Sender<Arc<OrderBook>>,
    order_book: Arc<OrderBook>,
    instrument_name: String,
}

impl OrderBookManager {
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<Arc<OrderBook>>, instrument_name: String) -> Result<Self> {
        let url = Url::parse(&config.url)
            .map_err(|e| anyhow!("Failed to parse URL: {}", e))?;
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| anyhow!("Failed to connect to WebSocket: {}", e))?;
        
        let order_book = Arc::new(OrderBook::new(instrument_name.clone()));
        
        Ok(OrderBookManager {
            ws_stream,
            config,
            sender,
            order_book,
            instrument_name,
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
        println!("OrderBookManager: Refreshing authentication token");
        refresh_token(&mut self.ws_stream, self.config.refresh_token.as_deref()).await
    }

    fn schedule_token_refresh(&self, duration: Duration, refresh_sender: mpsc::Sender<()>) {
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = refresh_sender.send(()).await {
                eprintln!("OrderBookManager: Failed to send refresh signal: {}", e);
            }
        });
    }

    async fn handle_heartbeat(&mut self, message_type: &str) -> Result<()> {
        match message_type {
            "heartbeat" => {
                println!("OrderBookManager: Responding to heartbeat");
                let test_message = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8212,
                    "method": "public/test",
                    "params": {}
                });
                self.send_ws_message(test_message).await?;
            }
            "test_request" => {
                println!("OrderBookManager: Responding to test request");
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
        println!("OrderBookManager: Starting to listen");
        
        loop {
            match self.listen_internal().await {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("OrderBookManager: Connection error: {}. Attempting to reconnect...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let url = Url::parse(&self.config.url)
                        .map_err(|e| anyhow!("Failed to parse URL during reconnection: {}", e))?;
                    if let Ok((new_ws_stream, _)) = connect_async(url).await {
                        self.ws_stream = new_ws_stream;
                        if let Err(auth_err) = self.authenticate().await {
                            eprintln!("OrderBookManager: Authentication failed during reconnection: {}", auth_err);
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
                "channels": [format!("book.{}.raw", self.instrument_name)]
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
                        eprintln!("OrderBookManager: Failed to send heartbeat: {}", e);
                        return Err(anyhow!("Failed to send heartbeat: {}", e));
                    }
                }
                Some(()) = refresh_receiver.recv() => {
                    match self.refresh_auth_token().await {
                        Ok(new_auth) => {
                            let next_refresh = Duration::from_secs((new_auth.expires_in as f64 * 0.8) as u64);
                            self.schedule_token_refresh(next_refresh, refresh_sender.clone());
                            println!("OrderBookManager: Successfully refreshed authentication token");
                        }
                        Err(e) => {
                            eprintln!("OrderBookManager: Failed to refresh token: {}", e);
                            return Err(anyhow!("Failed to refresh token: {}", e));
                        }
                    }
                }
                Some(msg) = self.ws_stream.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<WebSocketMessage>(&text) {
                                Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                                    if params.channel.starts_with("book.") {
                                        let order_book = Arc::make_mut(&mut self.order_book);
                                        if let Err(e) = order_book.update(&params.data) {
                                            eprintln!("OrderBookManager: Error updating order book: {}", e);
                                        } else {
                                            if let Err(e) = self.sender.send(self.order_book.clone()).await {
                                                eprintln!("OrderBookManager: Error sending order book: {}", e);
                                            }
                                        }
                                    }
                                },
                                Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                    println!("OrderBookManager: Received response for id {}: {:?}", id, result);
                                },
                                Ok(WebSocketMessage::Heartbeat { params, .. }) => {
                                    println!("OrderBookManager: Received heartbeat");
                                    self.handle_heartbeat(&params.heartbeat_type).await?;
                                },
                                Ok(WebSocketMessage::TestRequest { params, .. }) => {
                                    println!("OrderBookManager: Received test request");
                                    self.handle_heartbeat(&params.test_request_type).await?;
                                },
                                Err(e) => {
                                    eprintln!("OrderBookManager: Error parsing WebSocket message: {}", e);
                                    eprintln!("OrderBookManager: Received message: {}", text);
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            self.ws_stream.send(Message::Pong(data))
                                .await
                                .map_err(|e| anyhow!("Failed to send pong: {}", e))?;
                        }
                        Ok(Message::Pong(_)) => {
                            println!("OrderBookManager: Received pong");
                        }
                        Ok(Message::Close(frame)) => {
                            println!("OrderBookManager: Received close frame: {:?}", frame);
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("OrderBookManager: WebSocket error: {}", e);
                            return Err(anyhow!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                }
                else => {
                    println!("OrderBookManager: WebSocket stream ended");
                    return Ok(());
                }
            }
        }
    }
}