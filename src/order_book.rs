use std::collections::BTreeMap;
use serde::{Serialize, Deserialize};
use ordered_float::OrderedFloat;
use serde_json::Value;
use crate::AppError;

// Wrapper type for OrderedFloat<f64>
#[derive(Clone, Debug)]
pub struct Price(OrderedFloat<f64>);

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let f = f64::deserialize(deserializer)?;
        Ok(Price(OrderedFloat(f)))
    }
}

impl Serialize for Price {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl PartialEq for Price {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Price {}

impl PartialOrd for Price {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl Ord for Price {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderBook {
    bids: BTreeMap<Price, f64>,
    asks: BTreeMap<Price, f64>,
    instrument_name: String,
    change_id: u64,
}

#[derive(Debug, Deserialize)]
struct OrderBookUpdate {
    #[serde(rename = "type")]
    update_type: String,
    timestamp: u64,
    instrument_name: String,
    change_id: u64,
    prev_change_id: Option<u64>,
    bids: Vec<Vec<Value>>,
    asks: Vec<Vec<Value>>,
}

impl OrderBook {
    pub fn new(instrument_name: String) -> Self {
        OrderBook {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            instrument_name,
            change_id: 0,
        }
    }

    pub fn update(&mut self, message: &str) -> Result<(), AppError> {
        let parsed: Value = serde_json::from_str(message).map_err(|e| AppError(e.to_string()))?;
        let update: OrderBookUpdate = serde_json::from_value(parsed["params"]["data"].clone())
            .map_err(|e| AppError(e.to_string()))?;

        if update.instrument_name != self.instrument_name {
            return Err(AppError("Instrument name mismatch".into()));
        }

        if let Some(prev_change_id) = update.prev_change_id {
            if prev_change_id != self.change_id && update.update_type != "snapshot" {
                return Err(AppError("Missed update".into()));
            }
        }

        self.change_id = update.change_id;

        self.process_changes(&update.bids, true)?;
        self.process_changes(&update.asks, false)?;

        Ok(())
    }

    fn process_changes(&mut self, changes: &[Vec<Value>], is_bid: bool) -> Result<(), AppError> {
        let book = if is_bid { &mut self.bids } else { &mut self.asks };

        for change in changes {
            if change.len() != 3 {
                return Err(AppError("Invalid change format".into()));
            }

            let action = change[0].as_str().ok_or_else(|| AppError("Invalid action".into()))?;
            let price = Price(OrderedFloat(change[1].as_f64().ok_or_else(|| AppError("Invalid price".into()))?));
            let amount: f64 = change[2].as_f64().ok_or_else(|| AppError("Invalid amount".into()))?;

            match action {
                "new" | "change" => {
                    if amount > 0.0 {
                        book.insert(price, amount);
                    } else {
                        book.remove(&price);
                    }
                }
                "delete" => {
                    book.remove(&price);
                }
                _ => return Err(AppError("Unknown action".into())),
            }
        }

        Ok(())
    }

    
    pub fn get_instrument_name(&self) -> &str {
        &self.instrument_name
    }

    pub fn get_change_id(&self) -> u64 {
        self.change_id
    }

    pub fn get_mid_price(&self) -> Option<f64> {
        let best_bid = self.bids.keys().next_back()?;
        let best_ask = self.asks.keys().next()?;
        Some((best_bid.0.0 + best_ask.0.0) / 2.0)
    }
    
    pub fn get_spread(&self) -> Option<f64> {
        let best_bid = self.bids.keys().next_back()?;
        let best_ask = self.asks.keys().next()?;
        Some(best_ask.0.0 - best_bid.0.0)
    }


    pub fn get_best_bid(&self) -> Option<(f64, f64)> {
        self.bids.iter().next_back().map(|(price, amount)| (price.0.0, *amount))
    }

    pub fn get_best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(price, amount)| (price.0.0, *amount))
    }

    
    pub fn get_liquidity_depth(&self, depth_percentage: f64) -> f64 {
        let mid_price = self.get_mid_price().unwrap_or(0.0);
        let depth_range = mid_price * depth_percentage;

        let bid_volume: f64 = self.bids
            .iter()
            .take_while(|(price, _)| mid_price - price.0.0 <= depth_range)
            .map(|(_, amount)| amount)
            .sum();

        let ask_volume: f64 = self.asks
            .iter()
            .take_while(|(price, _)| price.0.0 - mid_price <= depth_range)
            .map(|(_, amount)| amount)
            .sum();

        (bid_volume + ask_volume) / (2.0 * depth_range)
    }
}