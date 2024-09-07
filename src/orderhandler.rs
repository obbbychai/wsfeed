use tokio_tungstenite::tungstenite::protocol::Message;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::error::Error as StdError;
use tokio_tungstenite::connect_async;
use url::Url;

use crate::auth::{authenticate_with_signature, DeribitConfig};

pub struct OrderHandler {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
}

impl OrderHandler {
    pub async fn new(config: DeribitConfig) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        let mut handler = OrderHandler { ws_stream, config };
        handler.authenticate().await?;
        
        Ok(handler)
    }

    async fn authenticate(&mut self) -> Result<(), Box<dyn StdError>> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    pub async fn create_market_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64) -> Result<(), Box<dyn StdError>> {
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
        
        // Handle the response
        if let Some(msg) = self.ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                println!("Order response: {}", text);
                // Here you can parse the response and handle any errors or confirmations
            }
        }

        Ok(())
    }

    pub async fn create_limit_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64, price: f64) -> Result<(), Box<dyn StdError>> {
        let method = if is_buy { "private/buy" } else { "private/sell" };
        let order_message = json!({
            "jsonrpc": "2.0",
            "id": 5276,
            "method": method,
            "params": {
                "instrument_name": instrument_name,
                "amount": amount,
                "type": "limit",
                "price": price
            }
        });

        self.ws_stream.send(Message::Text(order_message.to_string())).await?;
        
        // Handle the response
        if let Some(msg) = self.ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                println!("Order response: {}", text);
                // Here you can parse the response and handle any errors or confirmations
            }
        }

        Ok(())
    }

    // Add more methods for other order types or order management as needed
}

// Helper function to create a market order with default size
pub async fn create_default_market_order(handler: &mut OrderHandler, instrument_name: &str, is_buy: bool) -> Result<(), Box<dyn StdError>> {
    const DEFAULT_SIZE: f64 = 10.0;
    handler.create_market_order(instrument_name, is_buy, DEFAULT_SIZE).await
}