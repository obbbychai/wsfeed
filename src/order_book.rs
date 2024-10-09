use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use ordered_float::OrderedFloat;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use anyhow::{Result, Context, anyhow};
use std::sync::Arc;
use crate::auth::{authenticate_with_signature, DeribitConfig};
use tokio::time::{sleep, Duration};

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
}

impl OrderBookManager {
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<Arc<OrderBook>>, instrument_name: String) -> Result<Self> {
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
        let order_book = Arc::new(OrderBook::new(instrument_name.clone()));
        
        let mut manager = OrderBookManager {
            ws_stream,
            config,
            sender,
            order_book,
        };
        manager.authenticate().await.context("Failed to authenticate")?;
        manager.subscribe_to_order_book(&instrument_name).await.context("Failed to subscribe to order book")?;
        
        Ok(manager)
    }

    async fn authenticate(&mut self) -> Result<()> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    async fn subscribe_to_order_book(&mut self, instrument_name: &str) -> Result<()> {
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": [format!("book.{}.raw", instrument_name)]
            }
        });

        self.ws_stream.send(Message::Text(subscribe_message.to_string())).await
            .context("Failed to send subscription message")?;
        Ok(())
    }

    pub async fn start_listening(mut self) -> Result<()> {
        println!("OrderBookManager: Starting to listen");
        let mut reconnect_attempts = 0;
        loop {
            match self.listen_internal().await {
                Ok(_) => {
                    println!("OrderBookManager: WebSocket connection closed normally");
                    break;
                }
                Err(e) => {
                    eprintln!("OrderBookManager: Error in WebSocket connection: {}", e);
                    reconnect_attempts += 1;
                    if reconnect_attempts > 5 {
                        return Err(anyhow!("Failed to reconnect after 5 attempts"));
                    }
                    println!("OrderBookManager: Attempting to reconnect in 5 seconds...");
                    sleep(Duration::from_secs(5)).await;
                    let new_manager = OrderBookManager::new(self.config.clone(), self.sender.clone(), self.order_book.instrument_name.clone()).await?;
                    self = new_manager;
                }
            }
        }
        Ok(())
    }

    async fn listen_internal(&mut self) -> Result<()> {
        while let Some(msg) = self.ws_stream.next().await {
            let msg = msg.context("Failed to receive WebSocket message")?;
            match msg {
                Message::Text(text) => {
                //    println!("OrderBookManager: Received text message: {}", text);
                    match serde_json::from_str::<WebSocketMessage>(&text) {
                        Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                            if params.channel.starts_with("book.") {
                //                println!("OrderBookManager: Received order book update for {}", params.data.instrument_name);
                                let order_book = Arc::make_mut(&mut self.order_book);
                                if let Err(e) = order_book.update(&params.data) {
                                    eprintln!("Error updating order book: {}", e);
                                } else {
                //                    println!("OrderBookManager: Sending updated order book");
                                    if let Err(e) = self.sender.send(self.order_book.clone()).await {
                                        eprintln!("Error sending order book: {}", e);
                                    }
                                }
                            } else {
                //                println!("OrderBookManager: Received message for channel: {}", params.channel);
                            }
                        },
                        Ok(WebSocketMessage::ResponseMessage { result, .. }) => {
                            println!("OrderBookManager: Received response: {:?}", result);
                        },
                        Err(e) => {
                            eprintln!("OrderBookManager: Error parsing WebSocket message: {}", e);
                            eprintln!("Received message: {}", text);
                        }
                    }
                },
                Message::Binary(data) => {
                    println!("OrderBookManager: Received binary message of {} bytes", data.len());
                },
                Message::Ping(_) => println!("OrderBookManager: Received ping"),
                Message::Pong(_) => println!("OrderBookManager: Received pong"),
                Message::Close(_) => {
                    println!("OrderBookManager: Received close message");
                    return Ok(());
                },
                Message::Frame(_) => println!("OrderBookManager: Received raw frame"),
            }
        }
        Ok(())
    }
}