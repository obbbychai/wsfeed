use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::{json, Value};
use anyhow::{Result, Context, anyhow};
use url::Url;
use tokio::sync::{mpsc, RwLock};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use crate::oms::{self, OrderManagementSystem};  // Updated import
use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};
use tokio::time::{interval, Duration};


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
    pub state: String,
}

pub struct OrderHandler {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    order_receiver: mpsc::Receiver<OrderMessage>,
    oms: Arc<RwLock<OrderManagementSystem>>,
}



impl OrderHandler {
    pub async fn new(
        config: DeribitConfig,
        order_receiver: mpsc::Receiver<OrderMessage>,
        oms: Arc<RwLock<OrderManagementSystem>>,
    ) -> Result<Self> {
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
        Ok(OrderHandler {
            ws_stream,
            config,
            order_receiver,
            oms,
        })
    }

    
    async fn authenticate(&mut self) -> Result<AuthResponse> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    async fn refresh_auth_token(&mut self) -> Result<AuthResponse> {
        println!("OrderHandler: Refreshing authentication token");
        refresh_token(&mut self.ws_stream, self.config.refresh_token.as_deref()).await
    }

    fn schedule_token_refresh(&self, duration: Duration, refresh_sender: mpsc::Sender<()>) {
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = refresh_sender.send(()).await {
                eprintln!("OrderHandler: Failed to send refresh signal: {}", e);
            }
        });
    }


    async fn send_ws_message(&mut self, message: Value) -> Result<()> {
        self.ws_stream.send(Message::Text(message.to_string()))
            .await
            .context("Failed to send WebSocket message")
    }


    async fn handle_heartbeat(&mut self, message_type: &str) -> Result<()> {
        match message_type {
            "heartbeat" => {
                println!("OrderHandler: Responding to heartbeat");
                let test_message = json!({
                    "jsonrpc": "2.0",
                    "id": 8212,
                    "method": "public/test",
                    "params": {}
                });
                self.send_ws_message(test_message).await?;
            }
            "test_request" => {
                println!("OrderHandler: Responding to test request");
                let test_response = json!({
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

    pub async fn start_listening(&mut self) -> Result<()> {
        println!("OrderHandler: Starting to listen");
        
        loop {
            match self.listen_internal().await {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("OrderHandler: Connection error: {}. Attempting to reconnect...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let url = Url::parse(&self.config.url)?;
                    if let Ok((new_ws_stream, _)) = connect_async(url).await {
                        self.ws_stream = new_ws_stream;
                        if let Err(auth_err) = self.authenticate().await {
                            eprintln!("OrderHandler: Authentication failed during reconnection: {}", auth_err);
                            continue;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn listen_internal(&mut self) -> Result<()> {
        // Initial authentication
        let auth_response = self.authenticate().await?;
        
        // Set up refresh channel
        let (refresh_sender, mut refresh_receiver) = mpsc::channel::<()>(1);
        
        // Schedule first token refresh (at 80% of token lifetime)
        let refresh_in = Duration::from_secs((auth_response.expires_in as f64 * 0.8) as u64);
        self.schedule_token_refresh(refresh_in, refresh_sender.clone());
        
        // Set up heartbeat interval
        let mut interval = interval(Duration::from_secs(15));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let test_message = json!({
                        "jsonrpc": "2.0",
                        "id": 8212,
                        "method": "public/test",
                        "params": {}
                    });
                    if let Err(e) = self.send_ws_message(test_message).await {
                        eprintln!("OrderHandler: Failed to send heartbeat: {}", e);
                        return Err(e);
                    }
                }
                Some(()) = refresh_receiver.recv() => {
                    match self.refresh_auth_token().await {
                        Ok(new_auth) => {
                            // Schedule next refresh
                            let next_refresh = Duration::from_secs((new_auth.expires_in as f64 * 0.8) as u64);
                            self.schedule_token_refresh(next_refresh, refresh_sender.clone());
                            println!("OrderHandler: Successfully refreshed authentication token");
                        }
                        Err(e) => {
                            eprintln!("OrderHandler: Failed to refresh token: {}", e);
                            return Err(e);
                        }
                    }
                }


                Some(order_message) = self.order_receiver.recv() => {
                    if let Err(e) = self.process_order_message(order_message).await {
                        eprintln!("OrderHandler: Error processing order message: {}", e);
                    }
                }
                Some(msg) = self.ws_stream.next() => {
                    match msg {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<WebSocketMessage>(&text) {
                                Ok(WebSocketMessage::SubscriptionData { params, .. }) => {
                                    self.process_subscription_data(params).await?;
                                },
                                Ok(WebSocketMessage::ResponseMessage { id, result, .. }) => {
                                    self.process_response_message(id, result).await?;
                                },
                                Ok(WebSocketMessage::Heartbeat { params, .. }) => {
                                    println!("OrderHandler: Received heartbeat");
                                    self.handle_heartbeat(&params.heartbeat_type).await?;
                                },
                                Ok(WebSocketMessage::TestRequest { params, .. }) => {
                                    println!("OrderHandler: Received test request");
                                    self.handle_heartbeat(&params.test_request_type).await?;
                                },
                                Err(e) => {
                                    eprintln!("OrderHandler: Error parsing WebSocket message: {}", e);
                                    eprintln!("OrderHandler: Received message: {}", text);
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            println!("OrderHandler: Received ping, responding with pong");
                            self.ws_stream.send(Message::Pong(data)).await?;
                        }
                        Ok(Message::Pong(_)) => {
                            println!("OrderHandler: Received pong");
                        }
                        Ok(Message::Close(frame)) => {
                            println!("OrderHandler: Received close frame: {:?}", frame);
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!("OrderHandler: WebSocket error: {}", e);
                            return Err(e.into());
                        }
                        _ => {}
                    }
                }
                else => {
                    println!("OrderHandler: All channels closed, exiting...");
                    return Ok(());
                }
            }
        }
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

        self.send_ws_message(order_message).await
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

        self.send_ws_message(order_message).await
    }

    async fn cancel_all_orders(&mut self) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "private/cancel_all",
            "params": {}
        });
        self.send_ws_message(request).await
    }

    fn adjust_to_tick_size(&self, _instrument_name: &str, price: f64) -> Result<f64> {
        let tick_size = 2.5;
        let adjusted_price = (price / tick_size).round() * tick_size;
        Ok(adjusted_price)
    }

    async fn process_subscription_data(&self, params: WebSocketParams) -> Result<()> {
        println!("OrderHandler: Received subscription data for channel: {}", params.channel);
        Ok(())
    }

    async fn process_response_message(&mut self, id: u64, result: Value) -> Result<()> {
        println!("OrderHandler: Received response for id {}: {:?}", id, result);
        
        // Only process order data if we get trades
        if let Some(trades) = result.get("trades").and_then(|t| t.as_array()) {
            for trade_data in trades {
                if let Ok(trade) = serde_json::from_value(trade_data.clone()) {
                    self.add_trade(trade).await;
                }
            }
        }
        Ok(())
    }


    async fn add_or_update_order(&self, order: Order) {
        let mut oms = self.oms.write().await;
        oms.add_or_update_order(order.into()).await;  // Convert Order type using .into()
    }
    
    async fn add_trade(&self, trade: Trade) {
        let mut oms = self.oms.write().await;
        oms.add_trade(trade).await;
    }

}

impl From<Order> for oms::Order {
    fn from(order: Order) -> oms::Order {  // Fully qualify the return type
        oms::Order {
            time_in_force: order.time_in_force,
            reduce_only: order.reduce_only,
            price: order.price,
            post_only: order.post_only,
            order_type: order.order_type,
            order_state: order.order_state,
            order_id: order.order_id,
            max_show: order.max_show,
            last_update_timestamp: order.last_update_timestamp,
            label: order.label,
            is_rebalance: order.is_rebalance,
            is_liquidation: order.is_liquidation,
            instrument_name: order.instrument_name,
            filled_amount: order.filled_amount,
            direction: order.direction,
            creation_timestamp: order.creation_timestamp,
            average_price: order.average_price,
            api: order.api,
            amount: order.amount,
            state: order.state,
        }
    }
}

impl From<oms::Order> for Order {
    fn from(order: oms::Order) -> Order {  // Specify return type as Order
        Order {
            time_in_force: order.time_in_force,
            reduce_only: order.reduce_only,
            price: order.price,
            post_only: order.post_only,
            order_type: order.order_type,
            order_state: order.order_state,
            order_id: order.order_id,
            max_show: order.max_show,
            last_update_timestamp: order.last_update_timestamp,
            label: order.label,
            is_rebalance: order.is_rebalance,
            is_liquidation: order.is_liquidation,
            instrument_name: order.instrument_name,
            filled_amount: order.filled_amount,
            direction: order.direction,
            creation_timestamp: order.creation_timestamp,
            average_price: order.average_price,
            api: order.api,
            amount: order.amount,
            state: order.state,
        }
    }
}