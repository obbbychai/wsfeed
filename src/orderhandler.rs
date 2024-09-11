use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::error::Error as StdError;
use tokio_tungstenite::connect_async;
use url::Url;
use std::sync::Arc;
use crate::auth::{authenticate_with_signature, DeribitConfig};
use crate::oms::{OrderManagementSystem, Order, Trade};
use crate::AppError;




pub struct OrderHandler {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    oms: Arc<OrderManagementSystem>,
}

impl OrderHandler {
    pub async fn new(config: DeribitConfig, oms: Arc<OrderManagementSystem>) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        let mut handler = OrderHandler { ws_stream, config, oms };
        handler.authenticate().await?;
        
        Ok(handler)
    }

    async fn authenticate(&mut self) -> Result<(), AppError> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    pub async fn create_market_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64) -> Result<String, Box<dyn StdError>> {
        let method = if is_buy { "private/buy" } else { "private/sell" };
        let order_message = json!({
            "jsonrpc": "2.0",
            "id": 5275,
            "method": method,
            "params": {
                "instrument_name": instrument_name,
                "amount": amount,
                "type": "market"
            }
        });

        self.ws_stream.send(Message::Text(order_message.to_string())).await?;
        self.handle_order_response().await?;
        // Return the order ID (you might need to modify this based on the actual response structure)
        Ok("order_id".to_string())
    }

    pub async fn create_limit_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64, price: f64) -> Result<String, Box<dyn StdError>> {
        let method = if is_buy { "private/buy" } else { "private/sell" };
        let adjusted_price = self.adjust_to_tick_size(instrument_name, price)?;
        let order_message = json!({
            "jsonrpc": "2.0",
            "id": 5276,
            "method": method,
            "params": {
                "instrument_name": instrument_name,
                "amount": amount,
                "type": "limit",
                "price": adjusted_price
            }
        });

        self.ws_stream.send(Message::Text(order_message.to_string())).await?;
        self.handle_order_response().await?;
        // Return the order ID (you might need to modify this based on the actual response structure)
        Ok("order_id".to_string())
    }

    async fn handle_order_response(&mut self) -> Result<String, Box<dyn StdError>> {
        if let Some(msg) = self.ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                let response: Value = serde_json::from_str(&text)?;
                println!("Order response: {}", text);
                if response["error"].is_object() {
                    let error_message = response["error"]["message"].as_str().unwrap_or("Unknown error");
                    return Err(error_message.into());
                } else if let Some(result) = response.get("result") {
                    if let Some(order_data) = result.get("order") {
                        let mut order: Order = serde_json::from_value(order_data.clone())?;
                        // Set the timestamp from creation_timestamp
                        order.timestamp = order_data["creation_timestamp"].as_u64().unwrap_or(0);
                        self.oms.add_or_update_order(order).await;
                    }
                    if let Some(trades) = result.get("trades").and_then(|t| t.as_array()) {
                        for trade_data in trades {
                            let mut trade: Trade = serde_json::from_value(trade_data.clone())?;
                            // Set the timestamp from the trade data
                            trade.timestamp = trade_data["timestamp"].as_u64().unwrap_or(0);
                            self.oms.add_trade(trade).await;
                        }
                    }
                    // Return the order ID
                    if let Some(order_id) = result.get("order").and_then(|o| o.get("order_id")).and_then(|id| id.as_str()) {
                        return Ok(order_id.to_string());
                    }
                } else {
                    println!("Unexpected response format: {}", text);
                }
            }
        }
        Err("Failed to process order response".into())
    }

    fn adjust_to_tick_size(&self, _instrument_name: &str, price: f64) -> Result<f64, Box<dyn StdError>> {
        // TODO: Implement a way to get the tick size for each instrument
        // For now, we'll use a hardcoded value of 2.5 for BTC futures
        let tick_size = 2.5;
        let adjusted_price = (price / tick_size).round() * tick_size;
        Ok(adjusted_price)
    }

    // Add more methods for other order types or order management as needed
}

// Update this function to return a Result<String, Box<dyn StdError>>
pub async fn create_default_market_order(handler: &mut OrderHandler, instrument_name: &str, is_buy: bool) -> Result<String, Box<dyn StdError>> {
    const DEFAULT_SIZE: f64 = 10.0;
    handler.create_market_order(instrument_name, is_buy, DEFAULT_SIZE).await
}