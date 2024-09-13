use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::Value;
use std::error::Error as StdError;
use url::Url;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};
use std::collections::VecDeque;

use crate::auth::{authenticate_with_signature, DeribitConfig};
use crate::eventbucket::{EventBucket, Event};

#[derive(Debug, Clone)]
pub struct VolatilityData {
    pub volatility: f64,
    pub timestamp: i64,
}

pub struct VolatilityManager {
    ws_stream: Arc<Mutex<Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>,
    config: DeribitConfig,
    volatility_data: Arc<RwLock<VecDeque<VolatilityData>>>,
    buffer_size: usize,
    event_bucket: Arc<EventBucket>,
}

impl VolatilityManager {
    pub async fn new(config: DeribitConfig, buffer_size: usize, event_bucket: Arc<EventBucket>) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        Ok(VolatilityManager {
            ws_stream: Arc::new(Mutex::new(Some(ws_stream))),
            config,
            volatility_data: Arc::new(RwLock::new(VecDeque::with_capacity(buffer_size))),
            buffer_size,
            event_bucket,
        })
    }

    pub async fn connect_and_subscribe(self: Arc<Self>) -> Result<(), Box<dyn StdError>> {
        println!("VolatilityManager: Connecting and subscribing");
        let ws_stream = {
            let mut ws_stream_guard = self.ws_stream.lock().await;
            ws_stream_guard.take()
        };

        if let Some(mut ws_stream) = ws_stream {
            authenticate_with_signature(&mut ws_stream, &self.config.client_id, &self.config.client_secret).await?;

            let subscribe_message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "public/subscribe",
                "params": {
                    "channels": ["deribit_volatility_index.btc_usd"]
                }
            });

            ws_stream.send(Message::Text(subscribe_message.to_string())).await?;
            println!("VolatilityManager: Subscribed to volatility index");

            let self_clone = self.clone();
            tokio::spawn(async move {
                if let Err(e) = self_clone.start_listening(ws_stream).await {
                    eprintln!("VolatilityManager error: {}", e);
                }
            });
        } else {
            return Err("WebSocket stream is not available".into());
        }
        Ok(())
    }

    async fn start_listening(&self, mut ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> Result<(), Box<dyn StdError>> {
        println!("VolatilityManager: Starting to listen");
        while let Some(msg) = ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                let value: Value = serde_json::from_str(&text)?;
                if let Some(params) = value.get("params") {
                    if let Some(data) = params.get("data") {
                        if let (Some(volatility), Some(timestamp)) = (data["volatility"].as_f64(), data["timestamp"].as_i64()) {
                            let volatility_data = VolatilityData { volatility, timestamp };
                            self.update(volatility_data).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn update(&self, data: VolatilityData) -> Result<(), Box<dyn StdError>> {
    //    println!("VolatilityManager: Updating volatility data: {:?}", data);
        
        let mut write_lock = self.volatility_data.write().await;
        if write_lock.len() == self.buffer_size {
            write_lock.pop_front();
        }
        write_lock.push_back(data.clone());
        let avg_volatility = self.calculate_average_volatility(&write_lock);
        drop(write_lock);

        // Send VolatilityUpdate event
        if let Err(e) = self.event_bucket.send(Event::VolatilityUpdate(avg_volatility)) {
     
        } else {

        }

        Ok(())
    }

    fn calculate_average_volatility(&self, data: &VecDeque<VolatilityData>) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        let sum: f64 = data.iter().map(|data| data.volatility).sum();
        sum / data.len() as f64
    }

    pub async fn get_average_volatility(&self) -> f64 {
        let read_lock = self.volatility_data.read().await;
        self.calculate_average_volatility(&read_lock)
    }
}