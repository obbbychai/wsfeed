use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use ordered_float::OrderedFloat;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use std::error::Error as StdError;
use std::sync::Arc;
use crate::auth::{authenticate_with_signature, DeribitConfig};

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
struct OrderBookUpdate {
    #[serde(rename = "type")]
    update_type: String,
    timestamp: u64,
    instrument_name: String,
    change_id: u64,
    prev_change_id: Option<u64>,
    bids: Vec<Vec<Value>>,
    asks: Vec<Vec<Value>>,
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

    pub fn update(&mut self, update: &OrderBookUpdate) -> Result<(), Box<dyn StdError + Send + Sync>> {
        if update.instrument_name != self.instrument_name {
            return Err("Instrument name mismatch".into());
        }

        if let Some(prev_change_id) = update.prev_change_id {
            if prev_change_id != self.change_id {
                return Err("Missed update".into());
            }
        }

        self.change_id = update.change_id;

        self.process_changes(&update.bids, true)?;
        self.process_changes(&update.asks, false)?;

        Ok(())
    }

    fn process_changes(&mut self, changes: &[Vec<Value>], is_bid: bool) -> Result<(), Box<dyn StdError + Send + Sync>> {
        let book = if is_bid { &mut self.bids } else { &mut self.asks };

        for change in changes {
            if change.len() != 3 {
                return Err("Invalid change format".into());
            }

            let action = change[0].as_str().ok_or("Invalid action")?;
            let price: OrderedFloat<f64> = OrderedFloat(change[1].as_f64().ok_or("Invalid price")?);
            let amount: f64 = change[2].as_f64().ok_or("Invalid amount")?;

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
                _ => return Err("Unknown action".into()),
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
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<Arc<OrderBook>>, instrument_name: String) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        let order_book = Arc::new(OrderBook::new(instrument_name.clone()));
        
        let mut manager = OrderBookManager {
            ws_stream,
            config,
            sender,
            order_book,
        };
        manager.authenticate().await?;
        manager.subscribe_to_order_book(&instrument_name).await?;
        
        Ok(manager)
    }

    async fn authenticate(&mut self) -> Result<(), Box<dyn StdError>> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    async fn subscribe_to_order_book(&mut self, instrument_name: &str) -> Result<(), Box<dyn StdError>> {
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": [format!("book.{}.raw", instrument_name)]
            }
        });

        self.ws_stream.send(Message::Text(subscribe_message.to_string())).await?;
        Ok(())
    }

    pub async fn start_listening(mut self) -> Result<(), Box<dyn StdError + Send + Sync>> {
        while let Some(msg) = self.ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                match serde_json::from_str::<WebSocketMessage>(&text) {
                    Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                        if params.channel.starts_with("book.") {
                            let order_book = Arc::make_mut(&mut self.order_book);
                            if let Err(e) = order_book.update(&params.data) {
                                eprintln!("Error updating order book: {}", e);
                            } else {
                                self.sender.send(self.order_book.clone()).await?;
                            }
                        }
                    },
                    Ok(WebSocketMessage::ResponseMessage { result, .. }) => {
                        println!("Received response: {:?}", result);
                    },
                    Err(e) => {
                        eprintln!("Error parsing WebSocket message: {}", e);
                        eprintln!("Received message: {}", text);
                    }
                }
            }
        }
        Ok(())
    }
}