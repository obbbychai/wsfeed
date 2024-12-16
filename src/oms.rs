use std::collections::{HashMap, BTreeMap};
use tokio::sync::mpsc;
use ordered_float::OrderedFloat;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::json;
use crate::orderhandler::{Trade, OrderMessage};
use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};
use anyhow::{Result, anyhow};
use url::Url;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Deserialize)] 
pub struct Position {
    #[serde(default)]
    pub average_price: f64,
    #[serde(default)]
    pub delta: f64,
    #[serde(default)]
    pub direction: String,
    pub estimated_liquidation_price: Option<f64>,
    #[serde(default)]
    pub floating_profit_loss: f64,
    #[serde(default)]
    pub index_price: f64,
    #[serde(default)]
    pub initial_margin: f64,
    pub instrument_name: String,
    #[serde(default)]
    pub interest_value: f64,
    #[serde(default)]
    pub leverage: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub maintenance_margin: f64,
    #[serde(default)]
    pub mark_price: f64,
    #[serde(default)]
    pub open_orders_margin: f64,
    #[serde(default)]
    pub realized_profit_loss: f64,
    #[serde(default)]
    pub settlement_price: f64,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub size_currency: f64,
    #[serde(default)]
    pub total_profit_loss: f64,
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
    pub state: String,  // This is no longer Option<String>
}

#[derive(Debug, Clone)]
pub struct OrderInfo {
    pub price: f64,
    pub amount: f64,
    pub filled_amount: f64,
    pub direction: String,
    pub state: String,
    pub timestamp: u64,
    pub instrument_name: String,
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub enum OrderState {
    Open,
    Filled,
    Canceled,
    Rejected,
    Expired,
}

pub struct OrderManagementSystem {
    // Main order storage
    orders: HashMap<String, Order>,
    order_info: HashMap<String, OrderInfo>,
    
    // Order tracking by state
    orders_by_state: HashMap<OrderState, Vec<String>>,
    
    // Order tracking by direction
    buy_orders: HashMap<String, OrderInfo>,
    sell_orders: HashMap<String, OrderInfo>,
    
    // Price-ordered active orders using OrderedFloat for proper comparison
    active_bids: BTreeMap<OrderedFloat<f64>, Vec<String>>,
    active_asks: BTreeMap<OrderedFloat<f64>, Vec<String>>,
    
    // Trade history
    trades: Vec<Trade>,
    
    // Communication channels
    mm_sender: mpsc::Sender<OMSUpdate>,
    oh_sender: mpsc::Sender<OrderMessage>,
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
}

#[derive(Debug)] 
pub enum OMSUpdate {
    OrderFilled(String),
    OrderPartiallyFilled(String),
    NewTrade(Trade),
    OrderStateChanged(String, OrderState),
}

impl OrderManagementSystem {
    // Connection and messaging methods
    pub async fn new(
        mm_sender: mpsc::Sender<OMSUpdate>,
        oh_sender: mpsc::Sender<OrderMessage>,
        config: DeribitConfig,
    ) -> Result<Self> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;

        let mut oms = OrderManagementSystem {
            orders: HashMap::new(),
            order_info: HashMap::new(),
            orders_by_state: HashMap::new(),
            buy_orders: HashMap::new(),
            sell_orders: HashMap::new(),
            active_bids: BTreeMap::new(),
            active_asks: BTreeMap::new(),
            trades: Vec::new(),
            mm_sender,
            oh_sender,
            ws_stream,
            config,
        };

        oms.orders_by_state.insert(OrderState::Open, Vec::new());
        oms.orders_by_state.insert(OrderState::Filled, Vec::new());
        oms.orders_by_state.insert(OrderState::Canceled, Vec::new());
        oms.orders_by_state.insert(OrderState::Rejected, Vec::new());
        oms.orders_by_state.insert(OrderState::Expired, Vec::new());

        oms.authenticate().await?;
        Ok(oms)
    }

    async fn authenticate(&mut self) -> Result<AuthResponse> {
        authenticate_with_signature(
            &mut self.ws_stream, 
            &self.config.client_id, 
            &self.config.client_secret
        ).await
    }

    async fn send_ws_message(&mut self, message: serde_json::Value) -> Result<serde_json::Value> {
        self.ws_stream.send(Message::Text(message.to_string()))
            .await
            .map_err(|e| anyhow!("Failed to send message: {}", e))?;

        if let Some(msg) = self.ws_stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let response: serde_json::Value = serde_json::from_str(&text)?;
                    if let Some(result) = response.get("result") {
                        Ok(result.clone())
                    } else if let Some(error) = response.get("error") {
                        Err(anyhow!("API error: {:?}", error))
                    } else {
                        Err(anyhow!("Invalid response format"))
                    }
                }
                _ => Err(anyhow!("Unexpected message type")),
            }
        } else {
            Err(anyhow!("No response received"))
        }
    }
    // Core async API methods (renamed from get_* to fetch_*)
    pub async fn fetch_position(&mut self, instrument_name: &str) -> Result<Position> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 404,
            "method": "private/get_position",
            "params": {
                "instrument_name": instrument_name
            }
        });
    
        let result = self.send_ws_message(request).await?;
        
        // Handle null response
        if result.is_null() {
            return Ok(Position {
                instrument_name: instrument_name.to_string(),
                average_price: 0.0,
                delta: 0.0,
                direction: "none".to_string(),
                estimated_liquidation_price: None,  // Changed from 0.0 to None
                floating_profit_loss: 0.0,
                index_price: 0.0,
                initial_margin: 0.0,
                interest_value: 0.0,
                leverage: 0,
                kind: "future".to_string(),
                maintenance_margin: 0.0,
                mark_price: 0.0,
                open_orders_margin: 0.0,
                realized_profit_loss: 0.0,
                settlement_price: 0.0,
                size: 0.0,
                size_currency: 0.0,
                total_profit_loss: 0.0,
            });
        }
    
        serde_json::from_value(result).map_err(|e| anyhow!("Failed to parse position: {}", e))
    }

    pub async fn fetch_open_orders(&mut self, instrument_name: &str) -> Result<Vec<Order>> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1953,
            "method": "private/get_open_orders",
            "params": {
                "instrument_name": instrument_name
            }
        });

        let result = self.send_ws_message(request).await?;
        let orders: Vec<Order> = serde_json::from_value(result)?;
        
        for order in orders.iter() {
            self.add_or_update_order(order.clone()).await;
        }

        Ok(orders)
    }

    pub async fn fetch_recent_trades(&mut self, instrument_name: &str) -> Result<Vec<Trade>> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1955,
            "method": "private/get_user_trades_by_instrument",
            "params": {
                "instrument_name": instrument_name,
                "count": 100,
                "include_old": false
            }
        });

        let result = self.send_ws_message(request).await?;
        let trades: Vec<Trade> = serde_json::from_value(result)?;
        
        for trade in trades.iter() {
            self.add_trade(trade.clone()).await;
        }

        Ok(trades)
    }




    pub fn get_orders_by_direction(&self, is_buy: bool) -> Vec<&OrderInfo> {
        if is_buy {
            self.buy_orders.values().collect()
        } else {
            self.sell_orders.values().collect()
        }
    }

    pub fn get_best_bid(&self) -> Option<(f64, &Vec<String>)> {
        self.active_bids.iter().next_back()
            .map(|(k, v)| (k.0, v))
    }

    pub fn get_best_ask(&self) -> Option<(f64, &Vec<String>)> {
        self.active_asks.iter().next()
            .map(|(k, v)| (k.0, v))
    }

    pub fn get_order_info(&self, order_id: &str) -> Option<&OrderInfo> {
        self.order_info.get(order_id)
    }

    pub fn get_active_orders(&self) -> Vec<&OrderInfo> {
        self.order_info.values()
            .filter(|info| info.state == "open")
            .collect()
    }

    pub fn get_position_by_instrument(&self, instrument_name: &str) -> f64 {
        self.order_info.values()
            .filter(|info| info.instrument_name == instrument_name && info.state == "filled")
            .fold(0.0, |pos, info| {
                pos + if info.direction == "buy" {
                    info.filled_amount
                } else {
                    -info.filled_amount
                }
            })
    }

    // Local state access (renamed for clarity)
    pub fn get_open_orders_sync(&self, instrument_name: &str) -> Vec<Order> {
        self.orders.values()
            .filter(|order| 
                order.instrument_name.as_ref().map(String::as_str) == Some(instrument_name) &&
                order.state == "open"  // Direct comparison since state is String
            )
            .cloned()
            .collect()
    }

    pub fn get_recent_trades_sync(&self, instrument_name: &str) -> Vec<Trade> {
        self.trades.iter()
            .filter(|trade| trade.instrument_name == instrument_name)
            .cloned()
            .collect()
    }

    // Update methods
    pub async fn add_or_update_order(&mut self, order: Order) {
        if let Some(order_id) = &order.order_id {
            println!("Adding or updating order to OMS: {:?}", order);
            
            let order_info = OrderInfo {
                price: order.price.unwrap_or_default(),
                amount: order.amount.unwrap_or_default(),
                filled_amount: order.filled_amount.unwrap_or_default(),
                direction: order.direction.clone().unwrap_or_default(),
                state: order.state.clone(),  // No longer needs unwrap_or_default
                timestamp: order.creation_timestamp.unwrap_or_default(),
                instrument_name: order.instrument_name.clone().unwrap_or_default(),
            };
    
            let old_order = self.orders.insert(order_id.clone(), order.clone());
            self.order_info.insert(order_id.clone(), order_info.clone());
    
            let state = match order.state.as_str() {  // Use as_str() instead of as_deref()
                "open" => OrderState::Open,
                "filled" => OrderState::Filled,
                "cancelled" => OrderState::Canceled,
                "rejected" => OrderState::Rejected,
                "expired" => OrderState::Expired,
                _ => OrderState::Open,
            };
    
            // Update state tracking
            self.update_order_state(order_id, &state);
            self.update_price_levels(order_id, &order_info, &state);
    
            // Send updates
            self.send_order_updates(order_id, &order, old_order, &state).await;
        }
    }

    async fn send_order_updates(&mut self, order_id: &str, order: &Order, old_order: Option<Order>, state: &OrderState) {
        if order.state == "filled" {  // Direct comparison since state is String
            if let Err(e) = self.mm_sender.send(OMSUpdate::OrderFilled(order_id.to_string())).await {
                eprintln!("Failed to send OrderFilled update: {}", e);
            }
        } else if let (Some(old), Some(new)) = (old_order.and_then(|o| o.filled_amount), order.filled_amount) {
            if new > old {
                if let Err(e) = self.mm_sender.send(OMSUpdate::OrderPartiallyFilled(order_id.to_string())).await {
                    eprintln!("Failed to send OrderPartiallyFilled update: {}", e);
                }
            }
        }

        if let Err(e) = self.mm_sender.send(OMSUpdate::OrderStateChanged(order_id.to_string(), state.clone())).await {
            eprintln!("Failed to send OrderStateChanged update: {}", e);
        }
    }

    fn update_order_state(&mut self, order_id: &str, state: &OrderState) {
        for state_orders in self.orders_by_state.values_mut() {
            state_orders.retain(|id| id != order_id);
        }
        self.orders_by_state
            .entry(state.clone())
            .or_insert_with(Vec::new)
            .push(order_id.to_string());
    }

    fn update_price_levels(&mut self, order_id: &str, order_info: &OrderInfo, state: &OrderState) {
        // Clean up old price levels
        if let Some(old_info) = self.buy_orders.get(order_id) {
            if let Some(orders) = self.active_bids.get_mut(&OrderedFloat(old_info.price)) {
                orders.retain(|id| id != order_id);
            }
        }
        if let Some(old_info) = self.sell_orders.get(order_id) {
            if let Some(orders) = self.active_asks.get_mut(&OrderedFloat(old_info.price)) {
                orders.retain(|id| id != order_id);
            }
        }

        // Update direction-based tracking and price levels
        match order_info.direction.as_str() {
            "buy" => {
                self.buy_orders.insert(order_id.to_string(), order_info.clone());
                if state == &OrderState::Open {
                    self.active_bids.entry(OrderedFloat(order_info.price))
                        .or_insert_with(Vec::new)
                        .push(order_id.to_string());
                }
            }
            "sell" => {
                self.sell_orders.insert(order_id.to_string(), order_info.clone());
                if state == &OrderState::Open {
                    self.active_asks.entry(OrderedFloat(order_info.price))
                        .or_insert_with(Vec::new)
                        .push(order_id.to_string());
                }
            }
            _ => {}
        }

        // Clean up empty price levels
        self.active_bids.retain(|_, orders| !orders.is_empty());
        self.active_asks.retain(|_, orders| !orders.is_empty());
    }

    pub async fn add_trade(&mut self, trade: Trade) {
        println!("Adding trade to OMS: {:?}", trade);
        self.trades.push(trade.clone());
        if let Err(e) = self.mm_sender.send(OMSUpdate::NewTrade(trade)).await {
            eprintln!("Failed to send NewTrade update: {}", e);
        }
    }

    pub async fn send_order(&mut self, order_message: OrderMessage) {
        if let Err(e) = self.oh_sender.send(order_message).await {
            eprintln!("Failed to send order message to OrderHandler: {}", e);
        }
    }
}