use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use url::Url;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use serde_json::Value;

use crate::auth::{authenticate_with_signature, DeribitConfig};
use crate::eventbucket::{EventBucket, Event};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityData {
    pub volatility: f64,
    pub timestamp: i64,
    pub index_name: String,
}

#[derive(Clone)]
pub struct VolatilityManager {
    ws_stream: Arc<tokio::sync::Mutex<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>,
    config: DeribitConfig,
    volatility_data: Arc<RwLock<VecDeque<VolatilityData>>>,
    buffer_size: usize,
    event_bucket: Arc<EventBucket>,
}

impl VolatilityManager {
    pub async fn new(config: DeribitConfig, buffer_size: usize, event_bucket: Arc<EventBucket>) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        let manager = VolatilityManager {
            ws_stream: Arc::new(tokio::sync::Mutex::new(ws_stream)),
            config,
            volatility_data: Arc::new(RwLock::new(VecDeque::with_capacity(buffer_size))),
            buffer_size,
            event_bucket,
        };
        
        Ok(manager)
    }

    pub async fn connect_and_subscribe(&self) -> Result<(), Box<dyn StdError>> {
        let mut ws_stream = self.ws_stream.lock().await;
        authenticate_with_signature(&mut *ws_stream, &self.config.client_id, &self.config.client_secret).await?;
        
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["deribit_volatility_index.btc_usd"]
            }
        });

        ws_stream.send(Message::Text(subscribe_message.to_string())).await?;
        Ok(())
    }

    pub async fn start_listening(&self) -> Result<(), Box<dyn StdError>> {
        let mut ws_stream = self.ws_stream.lock().await;
        while let Some(msg) = ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                let value: Value = serde_json::from_str(&text)?;
                if let Some(params) = value.get("params") {
                    if let Some(data) = params.get("data") {
                        let volatility_data: VolatilityData = serde_json::from_value(data.clone())?;
                        self.update_volatility_data(volatility_data).await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn update_volatility_data(&self, data: VolatilityData) {
        let mut write_lock = self.volatility_data.write().await;
        if write_lock.len() == self.buffer_size {
            write_lock.pop_front();
        }
        write_lock.push_back(data.clone());
        let avg_volatility = self.calculate_average_volatility(&write_lock);
        drop(write_lock);

        // Send VolatilityUpdate event
        if let Err(e) = self.event_bucket.send(Event::VolatilityUpdate(avg_volatility)) {
            eprintln!("Failed to send VolatilityUpdate event: {}", e);
        }

        println!("New volatility data: {:?}", data);
        println!("Moving average volatility (last {} readings): {:.2}", self.buffer_size, avg_volatility);
    }

    fn calculate_average_volatility(&self, data: &VecDeque<VolatilityData>) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        let sum: f64 = data.iter().map(|data| data.volatility).sum();
        sum / data.len() as f64
    }

    pub async fn get_average_volatility(&self) -> Option<f64> {
        let read_lock = self.volatility_data.read().await;
        if read_lock.is_empty() {
            None
        } else {
            Some(self.calculate_average_volatility(&read_lock))
        }
    }
}