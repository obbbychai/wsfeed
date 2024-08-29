use std::collections::BTreeMap;
use serde::Deserialize;
use ordered_float::OrderedFloat;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct OrderBook {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    instrument_name: String,
    change_id: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OrderBookUpdate {
    #[serde(rename = "type")]
    update_type: String,
    timestamp: u64,
    instrument_name: String,
    change_id: u64,
    prev_change_id: Option<u64>,
    bids: Vec<Value>,
    asks: Vec<Value>,
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

    pub fn update(&mut self, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        let update: OrderBookUpdate = serde_json::from_str(message)?;

        if update.instrument_name != self.instrument_name {
            return Err("Instrument name mismatch".into());
        }

        if let Some(prev_change_id) = update.prev_change_id {
            if prev_change_id != self.change_id {
                return Err("Missed update".into());
            }
        }

        self.change_id = update.change_id;

        self.process_changes(&update.bids, true)?;
        self.process_changes(&update.asks, false)?;

        Ok(())
    }

    fn process_changes(&mut self, changes: &[Value], is_bid: bool) -> Result<(), Box<dyn std::error::Error>> {
        let book = if is_bid { &mut self.bids } else { &mut self.asks };

        for change in changes {
            if let [action, price, amount] = change.as_array().ok_or("Invalid change format")?.as_slice() {
                let action = action.as_str().ok_or("Invalid action")?;
                let price: OrderedFloat<f64> = OrderedFloat(price.as_f64().ok_or("Invalid price")?);
                let amount: f64 = amount.as_f64().ok_or("Invalid amount")?;

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
                    _ => return Err("Unknown action".into()),
                }
            } else {
                return Err("Invalid change format".into());
            }
        }

        Ok(())
    }

    pub fn get_mid_price(&self) -> Option<f64> {
        let best_bid = self.bids.keys().next_back()?;
        let best_ask = self.asks.keys().next()?;
        Some((best_bid.0 + best_ask.0) / 2.0)
    }
}