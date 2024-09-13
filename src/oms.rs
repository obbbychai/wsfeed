use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use tokio::sync::{RwLock, Mutex};
use std::sync::Arc;
use crate::auth::{authenticate_with_signature, DeribitConfig};
use crate::eventbucket::{EventBucket, Event};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String,
    pub instrument_name: String,
    pub amount: f64,
    pub price: f64,
    pub direction: String,
    pub timestamp: u64,
    // Add other fields as needed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub instrument_name: String,
    pub amount: f64,
    pub filled_amount: f64,
    pub price: f64,
    pub average_price: f64,
    pub direction: String,
    pub order_state: String,
    pub order_type: String,
    #[serde(default)]
    pub timestamp: Option<u64>,
    // Add other fields as needed
}

pub struct OrderManagementSystem {
    orders: RwLock<HashMap<String, Order>>,
    trades: RwLock<Vec<Trade>>,
    config: DeribitConfig,
    event_bucket: Arc<EventBucket>,
    ws_stream: Arc<Mutex<Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>,
}

impl OrderManagementSystem {
    pub async fn new(config: DeribitConfig, event_bucket: Arc<EventBucket>) -> Arc<Self> {
        let url = Url::parse(&config.url).expect("Failed to parse URL");
        let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");
        
        Arc::new(Self {
            orders: RwLock::new(HashMap::new()),
            trades: RwLock::new(Vec::new()),
            config,
            event_bucket,
            ws_stream: Arc::new(Mutex::new(Some(ws_stream))),
        })
    }

    pub async fn start_listening(self: Arc<Self>, instrument_name: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut ws_stream_guard = self.ws_stream.lock().await;
        if let Some(mut ws_stream) = ws_stream_guard.take() {
            authenticate_with_signature(&mut ws_stream, &self.config.client_id, &self.config.client_secret).await?;

            let subscribe_message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "private/subscribe",
                "params": {
                    "channels": [format!("user.orders.{}.raw", instrument_name)]
                }
            });

            ws_stream.send(Message::Text(subscribe_message.to_string())).await?;

            // Put the WebSocket stream back into the Mutex
            *ws_stream_guard = Some(ws_stream);

            // Start a separate task to listen for messages
            let self_clone = Arc::clone(&self);
            tokio::spawn(async move {
                self_clone.listen_for_messages().await;
            });
        }
        Ok(())
    }

    async fn listen_for_messages(&self) {
        loop {
            let mut ws_stream_guard = self.ws_stream.lock().await;
            if let Some(ws_stream) = ws_stream_guard.as_mut() {
                if let Some(msg) = ws_stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                            if let Some(params) = value.get("params") {
                                if let Some(data) = params.get("data") {
                                    let order: Order = serde_json::from_value(data.clone()).unwrap();
                                    self.update_order(order).await.unwrap();
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("WebSocket error: {:?}", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            drop(ws_stream_guard);
        }
    }

    pub async fn update_order(&self, order: Order) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Updated order: {:?}", order);
        
        let mut write_lock = self.orders.write().await;
        write_lock.insert(order.order_id.clone(), order.clone());
        drop(write_lock);

        // Send OrderUpdate event
        if let Err(e) = self.event_bucket.send(Event::OrderUpdate(order)) {
            eprintln!("Failed to send OrderUpdate event: {}", e);
        }

        Ok(())
    }

    async fn send_order_and_wait_for_response(&self, order_message: serde_json::Value) -> Result<Order, Box<dyn std::error::Error + Send + Sync>> {
        let mut ws_stream_guard = self.ws_stream.lock().await;
        if let Some(ws_stream) = ws_stream_guard.as_mut() {
            ws_stream.send(Message::Text(order_message.to_string())).await?;

            if let Some(response) = ws_stream.next().await {
                let response = response?;
                if let Message::Text(text) = response {
                    println!("Received response for order: {}", text);
                    let json: serde_json::Value = serde_json::from_str(&text)?;
                    if let Some(result) = json.get("result") {
                        let order = Order {
                            order_id: result["order_id"].as_str().unwrap_or_default().to_string(),
                            instrument_name: result["instrument_name"].as_str().unwrap_or_default().to_string(),
                            amount: result["amount"].as_f64().unwrap_or_default(),
                            filled_amount: result["filled_amount"].as_f64().unwrap_or_default(),
                            price: result["price"].as_f64().unwrap_or_default(),
                            average_price: result["average_price"].as_f64().unwrap_or_default(),
                            direction: result["direction"].as_str().unwrap_or_default().to_string(),
                            order_state: result["order_state"].as_str().unwrap_or_default().to_string(),
                            order_type: result["order_type"].as_str().unwrap_or_default().to_string(),
                            timestamp: result["creation_timestamp"].as_u64(),
                        };

                        println!("Placed order: {:?}", order);
                        return Ok(order);
                    } else if let Some(error) = json.get("error") {
                        println!("Error placing order: {:?}", error);
                        return Err(format!("Error placing order: {:?}", error).into());
                    }
                }
            }
        }

        Err("No response received from Deribit API".into())
    }

    pub async fn place_limit_buy(&self, instrument_name: &str, price: f64) -> Result<crate::oms::Order, Box<dyn std::error::Error + Send + Sync>> {
        println!("Attempting to place buy order for {} at price {}", instrument_name, price);
        let buy_order_message = json!({
            "jsonrpc": "2.0",
            "method": "private/buy",
            "params": {
                "instrument_name": instrument_name,
                "amount": 10.0,
                "price": price,
                "type": "limit",
                "post_only": true,
                "time_in_force": "good_til_cancelled"
            }
        });
        println!("Sending buy order message: {}", buy_order_message);
        let response = self.send_order_and_wait_for_response(buy_order_message).await?;
        Ok(response)
    }

    pub async fn place_limit_sell(&self, instrument_name: &str, price: f64) -> Result<crate::oms::Order, Box<dyn std::error::Error + Send + Sync>> {
        println!("Attempting to place sell order for {} at price {}", instrument_name, price);
        let sell_order_message = json!({
            "jsonrpc": "2.0",
            "method": "private/sell",
            "params": {
                "instrument_name": instrument_name,
                "amount": 10.0,
                "price": price,
                "type": "limit",
                "post_only": true,
                "time_in_force": "good_til_cancelled"
            }
        });
        println!("Sending sell order message: {}", sell_order_message);
        let response = self.send_order_and_wait_for_response(sell_order_message).await?;
        Ok(response)
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Here you would interact with the Deribit API to cancel the order
        // For now, we'll simulate order cancellation
        let mut write_lock = self.orders.write().await;
        if let Some(order) = write_lock.get_mut(order_id) {
            order.order_state = "cancelled".to_string();
            println!("Cancelled order: {:?}", order);
        }
        Ok(())
    }

    pub async fn get_order(&self, order_id: &str) -> Option<Order> {
        let orders = self.orders.read().await;
        orders.get(order_id).cloned()
    }

    pub async fn get_all_orders(&self) -> Vec<Order> {
        let orders = self.orders.read().await;
        orders.values().cloned().collect()
    }

    pub async fn add_trade(&self, trade: Trade) {
        let mut trades = self.trades.write().await;
        trades.push(trade);
    }

    pub async fn get_all_trades(&self) -> Vec<Trade> {
        let trades = self.trades.read().await;
        trades.clone()
    }
}