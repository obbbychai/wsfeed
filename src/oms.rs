use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use tokio::sync::RwLock;
use std::sync::Arc;

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
    pub timestamp: u64,
    // Add other fields as needed
}

pub struct OrderManagementSystem {
    orders: RwLock<HashMap<String, Order>>,
    trades: RwLock<Vec<Trade>>,
}

impl OrderManagementSystem {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            orders: RwLock::new(HashMap::new()),
            trades: RwLock::new(Vec::new()),
        })
    }

    pub async fn add_or_update_order(&self, order: Order) {
        let mut orders = self.orders.write().await;
        orders.insert(order.order_id.clone(), order);
    }

    pub async fn add_trade(&self, trade: Trade) {
        let mut trades = self.trades.write().await;
        trades.push(trade);
    }

    pub async fn get_order(&self, order_id: &str) -> Option<Order> {
        let orders = self.orders.read().await;
        orders.get(order_id).cloned()
    }

    pub async fn get_all_orders(&self) -> Vec<Order> {
        let orders = self.orders.read().await;
        orders.values().cloned().collect()
    }

    pub async fn get_all_trades(&self) -> Vec<Trade> {
        let trades = self.trades.read().await;
        trades.clone()
    }
}