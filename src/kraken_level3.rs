use std::collections::{BTreeMap, HashMap};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use crc32fast::Hasher;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Order {
    pub order_id: String,
    pub limit_price: Decimal,
    pub order_qty: Decimal,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct PriceLevel {
    #[allow(dead_code)]
    pub price: Decimal,
    pub orders: Vec<Order>,
}

pub struct KrakenLevel3OrderBook {
    bids: BTreeMap<Decimal, PriceLevel>,
    asks: BTreeMap<Decimal, PriceLevel>,
    order_map: HashMap<String, Decimal>,
}

impl KrakenLevel3OrderBook {
    pub fn new() -> Self {
        KrakenLevel3OrderBook {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_map: HashMap::new(),
        }
    }

    pub fn update(&mut self, data: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(bids) = data["bids"].as_array() {
            self.process_orders(bids, true)?;
        }
        if let Some(asks) = data["asks"].as_array() {
            self.process_orders(asks, false)?;
        }
        self.truncate_to_depth(10); // Truncate to top 10 levels
        Ok(())
    }

    fn process_orders(&mut self, orders: &[serde_json::Value], is_bid: bool) -> Result<(), Box<dyn std::error::Error>> {
        for order in orders {
            let order: Order = serde_json::from_value(order.clone())?;
            let price_levels = if is_bid { &mut self.bids } else { &mut self.asks };
            
            if order.order_qty == Decimal::ZERO {
                // Remove order
                if let Some(price) = self.order_map.remove(&order.order_id) {
                    if let Some(level) = price_levels.get_mut(&price) {
                        level.orders.retain(|o| o.order_id != order.order_id);
                        if level.orders.is_empty() {
                            price_levels.remove(&price);
                        }
                    }
                }
            } else {
                // Add or update order
                self.order_map.insert(order.order_id.clone(), order.limit_price);
                price_levels.entry(order.limit_price)
                    .or_insert_with(|| PriceLevel { price: order.limit_price, orders: Vec::new() })
                    .orders.push(order);
            }
        }
        Ok(())
    }

    fn truncate_to_depth(&mut self, depth: usize) {
        self.bids = self.bids.iter().rev().take(depth)
            .map(|(k, v)| (*k, v.clone())).collect();
        self.asks = self.asks.iter().take(depth)
            .map(|(k, v)| (*k, v.clone())).collect();
    }

    pub fn calculate_checksum(&self) -> u32 {
        let mut checksum_string = String::new();

        // Process asks
        for (_, level) in self.asks.iter().take(10) {
            for order in &level.orders {
                checksum_string.push_str(&format_for_checksum(&order.limit_price, &order.order_qty));
            }
        }

        // Process bids
        for (_, level) in self.bids.iter().rev().take(10) {
            for order in &level.orders {
                checksum_string.push_str(&format_for_checksum(&order.limit_price, &order.order_qty));
            }
        }

        // Calculate CRC32
        let mut hasher = Hasher::new();
        hasher.update(checksum_string.as_bytes());
        hasher.finalize()
    }
}

fn format_for_checksum(price: &Decimal, qty: &Decimal) -> String {
    let price_str = price.to_string().replace(".", "").trim_start_matches('0').to_string();
    let qty_str = qty.to_string().replace(".", "").trim_start_matches('0').to_string();
    format!("{}{}", price_str, qty_str)
}