use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use anyhow::{Result, Context, anyhow};
use tokio_tungstenite::connect_async;
use url::Url;
use tokio::sync::{mpsc, RwLock};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use crate::sharedstate::SharedState;
use crate::auth::{authenticate_with_signature, DeribitConfig};

#[derive(Debug, Clone)]
pub enum OrderType {
    Market,
    Limit,
    CancelAll,
    CancelOrder,
    EditOrder,
}

#[derive(Debug, Clone)]
pub struct OrderMessage {
    pub instrument_name: String,
    pub order_type: OrderType,
    pub amount: Option<f64>,
    pub price: Option<f64>,
    pub is_buy: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String,
    pub instrument_name: String,
    pub amount: f64,
    pub price: f64,
    pub direction: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    #[serde(rename = "time_in_force")]
    pub time_in_force: Option<String>,
    #[serde(rename = "reduce_only")]
    pub reduce_only: Option<bool>,
    pub price: Option<f64>,
    #[serde(rename = "post_only")]
    pub post_only: Option<bool>,
    #[serde(rename = "order_type")]
    pub order_type: Option<String>,
    #[serde(rename = "order_state")]
    pub order_state: Option<String>,
    #[serde(rename = "order_id")]
    pub order_id: Option<String>,
    #[serde(rename = "max_show")]
    pub max_show: Option<f64>,
    #[serde(rename = "last_update_timestamp")]
    pub last_update_timestamp: Option<u64>,
    pub label: Option<String>,
    #[serde(rename = "is_rebalance")]
    pub is_rebalance: Option<bool>,
    #[serde(rename = "is_liquidation")]
    pub is_liquidation: Option<bool>,
    #[serde(rename = "instrument_name")]
    pub instrument_name: Option<String>,
    #[serde(rename = "filled_amount")]
    pub filled_amount: Option<f64>,
    pub direction: Option<String>,
    #[serde(rename = "creation_timestamp")]
    pub creation_timestamp: Option<u64>,
    #[serde(rename = "average_price")]
    pub average_price: Option<f64>,
    pub api: Option<bool>,
    pub amount: Option<f64>,
}

pub struct OrderHandler {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    order_receiver: mpsc::Receiver<OrderMessage>,
    shared_state: Arc<RwLock<SharedState>>,
}

impl OrderHandler {
    pub async fn new(
        config: DeribitConfig,
        order_receiver: mpsc::Receiver<OrderMessage>,
        shared_state: Arc<RwLock<SharedState>>,
    ) -> Result<Self> {
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
        let mut handler = OrderHandler {
            ws_stream,
            config,
            order_receiver,
            shared_state,
        };
        handler.authenticate().await.context("Failed to authenticate")?;
        
        Ok(handler)
    }

    async fn authenticate(&mut self) -> Result<()> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                Some(order_message) = self.order_receiver.recv() => {
                    self.process_order_message(order_message).await?;
                }
                else => break,
            }
        }
        Ok(())
    }

    async fn process_order_message(&mut self, order_message: OrderMessage) -> Result<()> {
        match order_message.order_type {
            OrderType::Market => {
                if let (Some(is_buy), Some(amount)) = (order_message.is_buy, order_message.amount) {
                    self.create_market_order(&order_message.instrument_name, is_buy, amount).await?;
                } else {
                    return Err(anyhow!("Market order requires is_buy and amount"));
                }
            }
            OrderType::Limit => {
                if let (Some(is_buy), Some(amount), Some(price)) = (order_message.is_buy, order_message.amount, order_message.price) {
                    self.create_limit_order(&order_message.instrument_name, is_buy, amount, price).await?;
                } else {
                    return Err(anyhow!("Limit order requires is_buy, amount, and price"));
                }
            }
            OrderType::CancelAll => {
                self.cancel_all_orders().await?;
            }
            OrderType::CancelOrder => {
                return Err(anyhow!("CancelOrder not implemented"));
            }
            OrderType::EditOrder => {
                return Err(anyhow!("EditOrder not implemented"));
            }
        }
        Ok(())
    }

    async fn create_market_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64) -> Result<()> {
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

        self.ws_stream.send(WsMessage::Text(order_message.to_string()))
            .await
            .context("Failed to send market order message")?;
        self.handle_order_response().await
    }

    async fn cancel_order(&mut self, order_id: &str) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "private/cancel",
            "params": {
                "order_id": order_id
            }
        });
        self.ws_stream.send(WsMessage::Text(request.to_string()))
            .await
            .context("Failed to send cancel order message")?;
        self.handle_order_response().await
    }

    async fn cancel_all_orders(&mut self) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "private/cancel_all",
            "params": {}
        });
        self.ws_stream.send(WsMessage::Text(request.to_string()))
            .await
            .context("Failed to send cancel all orders message")?;
        self.handle_order_response().await
    }

    async fn edit_order(&mut self, order_id: &str) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "private/edit",
            "params": {
                "order_id": order_id
            }
        });
        self.ws_stream.send(WsMessage::Text(request.to_string()))
            .await
            .context("Failed to send edit order message")?;
        self.handle_order_response().await
    }




    async fn create_limit_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64, price: f64) -> Result<()> {
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
                "price": adjusted_price,
                "post_only": "true"
                
            }
        });

        self.ws_stream.send(WsMessage::Text(order_message.to_string()))
            .await
            .context("Failed to send limit order message")?;
        self.handle_order_response().await
    }

    async fn handle_order_response(&mut self) -> Result<()> {
        if let Some(msg) = self.ws_stream.next().await {
            let msg = msg.context("Failed to receive WebSocket message")?;
            if let WsMessage::Text(text) = msg {
                let response: Value = serde_json::from_str(&text)
                    .context("Failed to parse order response as JSON")?;
                println!("Order response: {}", text);
                
                self.process_order_response(response).await?;
                
                Ok(())
            } else {
                Err(anyhow!("Unexpected message type"))
            }
        } else {
            Err(anyhow!("No response received"))
        }
    }

    async fn process_order_response(&self, response: Value) -> Result<()> {
        if response["error"].is_object() {
            let error_message = response["error"]["message"].as_str().unwrap_or("Unknown error");
            return Err(anyhow!(error_message.to_string()));
        }

        if let Some(result) = response.get("result") {
            if let Some(order_data) = result.get("order") {
                let order: Order = serde_json::from_value(order_data.clone())
                    .context("Failed to parse order data")?;
                self.add_or_update_order(order).await;
            }

            if let Some(trades) = result.get("trades").and_then(|t| t.as_array()) {
                for trade_data in trades {
                    let trade: Trade = serde_json::from_value(trade_data.clone())
                        .context("Failed to parse trade data")?;
                    self.add_trade(trade).await;
                }
            }

            println!("Order processed successfully");
            Ok(())
        } else {
            Err(anyhow!("Failed to process order response: no result found"))
        }
    }

    fn adjust_to_tick_size(&self, _instrument_name: &str, price: f64) -> Result<f64> {
        // TODO: Implement a way to get the tick size for each instrument
        // For now, we'll use a hardcoded value of 2.5 for BTC futures
        let tick_size = 2.5;
        let adjusted_price = (price / tick_size).round() * tick_size;
        Ok(adjusted_price)
    }

    async fn add_or_update_order(&self, order: Order) {
        let mut shared_state = self.shared_state.write().await;
        shared_state.add_or_update_order(order);
    }
    
    async fn add_trade(&self, trade: Trade) {
        let mut shared_state = self.shared_state.write().await;
        shared_state.add_trade(trade);
    }

    pub async fn get_open_orders(&mut self) -> Result<Vec<Order>> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1953,
            "method": "private/get_open_orders",
            "params": {}
        });
    
        self.ws_stream.send(WsMessage::Text(request.to_string()))
            .await
            .context("Failed to send get_open_orders request")?;
    
        if let Some(msg) = self.ws_stream.next().await {
            let msg = msg.context("Failed to receive WebSocket message")?;
            if let WsMessage::Text(text) = msg {
                let response: Value = serde_json::from_str(&text)
                    .context("Failed to parse get_open_orders response as JSON")?;
                
                if let Some(error) = response.get("error") {
                    let error_message = error["message"].as_str().unwrap_or("Unknown error");
                    return Err(anyhow!(error_message.to_string()));
                }
    
                if let Some(result) = response.get("result") {
                    let open_orders: Vec<Order> = serde_json::from_value(result.clone())
                        .context("Failed to parse open orders")?;
                    
                    // Update the shared state with the open orders
                    let mut shared_state = self.shared_state.write().await;
                    for order in &open_orders {
                        shared_state.add_or_update_order(order.clone());
                    }
    
                    return Ok(open_orders);
                }
            }
        }
    
        Ok(Vec::new())  // Return an empty vector instead of an error when no orders are found
    }

    pub async fn get_all_trades(&self) -> Vec<Trade> {
        let shared_state = self.shared_state.read().await;
        shared_state.get_all_trades()
    }
}