use std::collections::BTreeMap;
use serde::Deserialize;
use ordered_float::OrderedFloat;
use serde_json::Value;
use crate::AppError;

#[derive(Debug, Clone)]
pub struct OrderBook {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
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
            let price: OrderedFloat<f64> = OrderedFloat(change[1].as_f64().ok_or_else(|| AppError("Invalid price".into()))?);
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

    pub fn get_mid_price(&self) -> Option<f64> {
        let best_bid = self.bids.keys().next_back()?;
        let best_ask = self.asks.keys().next()?;
        Some((best_bid.0 + best_ask.0) / 2.0)
    }

    pub fn get_best_bid(&self) -> Option<(f64, f64)> {
        self.bids.iter().next_back().map(|(price, amount)| (price.0, *amount))
    }

    pub fn get_best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(price, amount)| (price.0, *amount))
    }

    pub fn print_order_book(&self) {
        println!("Order Book for {}:", self.instrument_name);
        println!("Bids:");
        for (price, amount) in self.bids.iter().rev().take(5) {
            println!("  {:.2}: {:.8}", price.0, amount);
        }
        println!("Asks:");
        for (price, amount) in self.asks.iter().take(5) {
            println!("  {:.2}: {:.8}", price.0, amount);
        }
        println!("---");
    }
}