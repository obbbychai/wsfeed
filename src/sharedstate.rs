// sharedstate.rs

use std::collections::HashMap;
use crate::orderhandler::{Order, Trade};

pub struct SharedState {
    pub orders: HashMap<String, Order>,
    pub trades: Vec<Trade>,
}

impl SharedState {
    pub fn new() -> Self {
        SharedState {
            orders: HashMap::new(),
            trades: Vec::new(),
        }
    }

    pub fn add_or_update_order(&mut self, order: Order) {
        if let Some(order_id) = &order.order_id {
            self.orders.insert(order_id.clone(), order);
        }
    }

    pub fn add_trade(&mut self, trade: Trade) {
        self.trades.push(trade);
    }

    pub fn get_open_orders(&self, instrument_name: &str) -> Vec<Order> {
        self.orders.values()
            .filter(|order| 
                order.instrument_name.as_deref() == Some(instrument_name) &&
                order.order_state.as_deref() == Some("open")
            )
            .cloned()
            .collect()
    }

    pub fn get_partially_filled_orders(&self, instrument_name: &str) -> Vec<Order> {
        self.orders.values()
            .filter(|order| 
                order.instrument_name.as_deref() == Some(instrument_name) && 
                order.filled_amount.map(|fa| fa > 0.0).unwrap_or(false) && 
                order.filled_amount.zip(order.amount).map(|(fa, a)| fa < a).unwrap_or(false)
            )
            .cloned()
            .collect()
    }

    pub fn get_order(&self, order_id: &str) -> Option<Order> {
        self.orders.get(order_id).cloned()
    }

    pub fn get_all_orders(&self) -> Vec<Order> {
        self.orders.values().cloned().collect()
    }

    pub fn get_all_trades(&self) -> Vec<Trade> {
        self.trades.clone()
    }
}