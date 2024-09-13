use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use crate::eventbucket::{EventBucket, Event};
use crate::oms::OrderManagementSystem;
use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;
use crate::instrument_names::Instrument;
use std::time::{SystemTime, UNIX_EPOCH};
use std::fmt;
use crate::oms::Order;




#[derive(Debug, PartialEq)]
enum MarketMakerState {
    Ready,
    WaitingForFill,
    PausedLowFunds,
    WaitingForAskFill,
    WaitingForBidFill
}

pub struct MarketMaker {
    oms: Arc<OrderManagementSystem>,
    gamma: f64,
    depth_percentage: f64,
    event_rx: broadcast::Receiver<Event>,
    latest_order_book: Option<OrderBook>,
    latest_portfolio: Option<PortfolioData>,
    latest_volatility: Option<f64>,
    latest_instrument: Option<Instrument>,
    state: MarketMakerState,
    current_bid_id: Option<String>,
    current_ask_id: Option<String>,
}

impl fmt::Debug for MarketMaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MarketMaker")
            .field("gamma", &self.gamma)
            .field("depth_percentage", &self.depth_percentage)
            .field("state", &self.state)
            .field("current_bid_id", &self.current_bid_id)
            .field("current_ask_id", &self.current_ask_id)
            .finish()
    }
}

impl MarketMaker {
    pub fn new(oms: Arc<OrderManagementSystem>, gamma: f64, depth_percentage: f64, event_bucket: Arc<EventBucket>, instrument: Instrument) -> Self {
        let event_rx = event_bucket.subscribe();
        MarketMaker {
            oms,
            gamma,
            depth_percentage,
            event_rx,
            latest_order_book: None,
            latest_portfolio: None,
            latest_volatility: None,
            latest_instrument: Some(instrument),
            state: MarketMakerState::Ready,
            current_bid_id: None,
            current_ask_id: None,
        }
    }

    pub async fn run(market_maker: Arc<Mutex<Self>>) {
        loop {
            let event = {
                let mut mm = market_maker.lock().await;
                tokio::select! {
                    Ok(event) = mm.event_rx.recv() => Some(event),
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => None,
                }
            };

            if let Some(event) = event {
                let mut mm = market_maker.lock().await;
                mm.handle_event(event).await;
            } else {
                let mut mm = market_maker.lock().await;
                mm.update_prices().await;
            }
        }
    }

    async fn handle_event(&mut self, event: Event) {
        match event {
            Event::OrderBookUpdate(order_book) => {
                println!("Received order book update");
                if let Some(mid_price) = order_book.get_mid_price() {
                
                }
                self.latest_order_book = Some(order_book);
            }
            Event::PortfolioUpdate(portfolio) => {
                println!("Received portfolio update");
                self.latest_portfolio = Some(portfolio);
                self.check_funds().await;
            }
            Event::VolatilityUpdate(volatility) => {
                println!("Received volatility update: {}", volatility);
                self.latest_volatility = Some(volatility);
            }
            Event::InstrumentUpdate(instrument) => {
                println!("Received instrument update");
                self.latest_instrument = Some(instrument);
            }
            Event::OrderUpdate(order) => {
                println!("Received order update: {:?}", order);
                self.handle_order_update(order).await;
            },
        }
        if self.state == MarketMakerState::Ready {
            self.update_prices().await;
        }
    }

    async fn check_funds(&mut self) {
        if let Some(portfolio) = &self.latest_portfolio {
            let funds_ratio = portfolio.available_funds / portfolio.equity;
            if funds_ratio < 0.1 {
                println!("Available funds too low, pausing quoting");
                self.state = MarketMakerState::PausedLowFunds;
            } else if self.state == MarketMakerState::PausedLowFunds {
                println!("Funds sufficient, resuming quoting");
                self.state = MarketMakerState::Ready;
            }
        }
    }

    async fn handle_order_update(&mut self, order: crate::oms::Order) {
        match order.order_state.as_str() {
            "filled" => {
                if self.current_bid_id.is_none() && self.current_ask_id.is_none() {
                    self.state = MarketMakerState::Ready;
                } else if self.current_bid_id.is_some() && self.current_ask_id.is_none() {
                    // Bid order filled, ask order pending
                    self.state = MarketMakerState::WaitingForAskFill;
                } else if self.current_bid_id.is_none() && self.current_ask_id.is_some() {
                    // Ask order filled, bid order pending
                    self.state = MarketMakerState::WaitingForBidFill;
                }
            }
            "cancelled" | "rejected" => {
                if Some(order.order_id.clone()) == self.current_bid_id {
                    self.current_bid_id = None;
                } else if Some(order.order_id.clone()) == self.current_ask_id {
                    self.current_ask_id = None;
                }
                if self.current_bid_id.is_none() && self.current_ask_id.is_none() {
                    self.state = MarketMakerState::Ready;
                }
            }
            _ => {}
        }
    }

    async fn update_prices(&mut self) {
        println!("Updating prices...");
        if self.state != MarketMakerState::Ready {
            println!("Not ready to update prices. Current state: {:?}", self.state);
            return;
        }
        if self.latest_order_book.is_none() || self.latest_portfolio.is_none() || self.latest_volatility.is_none() || self.latest_instrument.is_none() {
            println!("Waiting for all necessary data...");
            return;
        }
        
        if let Some((optimal_bid, optimal_ask)) = self.calculate_bid_ask_prices().await {
            println!("Calculated new prices - Optimal Bid: {},  Optimal Ask: {}", optimal_bid, optimal_ask);
            
            if let Some(instrument) = &self.latest_instrument {
                let bid_price = optimal_bid;
                let ask_price = optimal_ask;
                
                let buy_order_future = self.place_limit_buy(&instrument.instrument_name, bid_price);
                let sell_order_future = self.place_limit_sell(&instrument.instrument_name, ask_price);

                match tokio::try_join!(buy_order_future, sell_order_future) {
                    Ok((buy_order, sell_order)) => {
                        println!("Placed buy order: {:?}", buy_order);
                        self.current_bid_id = Some(buy_order.order_id.clone());

                        println!("Placed sell order: {:?}", sell_order);
                        self.current_ask_id = Some(sell_order.order_id.clone());

                        self.state = MarketMakerState::WaitingForFill;
                    }
                    Err(e) => {
                        eprintln!("Failed to place orders: {}", e);
                    }
                }
            } else {
                println!("Cannot place orders: instrument information is missing");
            }
        } else {
            println!("Could not calculate bid-ask prices");
        }
    }

    async fn place_limit_buy(&self, instrument_name: &str, price: f64) -> Result<crate::oms::Order, Box<dyn std::error::Error + Send + Sync>> {
        println!("Attempting to place buy order for {} at price {}", instrument_name, price);
        self.oms.place_limit_buy(instrument_name, price).await
    }

    async fn place_limit_sell(&self, instrument_name: &str, price: f64) -> Result<crate::oms::Order, Box<dyn std::error::Error + Send + Sync>> {
        println!("Attempting to place sell order for {} at price {}", instrument_name, price);
        self.oms.place_limit_sell(instrument_name, price).await
    }

    fn calculate_reservation_price(&self, mid_price: f64, inventory: f64, volatility: f64, remaining_time: f64) -> f64 {
        mid_price - inventory * self.gamma * volatility.powi(2) * remaining_time
    }

    fn calculate_optimal_spread(&self, volatility: f64, remaining_time: f64, liquidity: f64) -> f64 {
        (self.gamma * volatility.powi(2) * remaining_time / liquidity).sqrt()
    }

    fn round_to_tick_size(&self, price: f64, tick_size: f64) -> f64 {
        (price / tick_size).round() * tick_size
    }

    pub async fn calculate_bid_ask_prices(&self) -> Option<(f64, f64)> {
        println!("Calculating bid-ask prices...");
        let (order_book, portfolio, volatility, instrument) = match (&self.latest_order_book, &self.latest_portfolio, self.latest_volatility, &self.latest_instrument) {
            (Some(ob), Some(p), Some(v), Some(i)) => (ob, p, v, i),
            _ => {
                println!("Missing data: OrderBook: {}, Portfolio: {}, Volatility: {}, Instrument: {}",
                    self.latest_order_book.is_some(),
                    self.latest_portfolio.is_some(),
                    self.latest_volatility.is_some(),
                    self.latest_instrument.is_some());
                return None;
            }
        };
    
        let mid_price = order_book.get_mid_price()?;
        let inventory = portfolio.delta_total;
        let liquidity = order_book.get_liquidity_depth(self.depth_percentage);
        let tick_size = instrument.tick_size;
    
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as f64;
        let creation_time = instrument.creation_timestamp as f64 / 1000.0;
        let expiration_time = instrument.expiration_timestamp as f64 / 1000.0;
        let total_time = expiration_time - creation_time;
        let remaining_time = (expiration_time - current_time) / total_time;
    
        let reservation_price = self.calculate_reservation_price(mid_price, inventory, volatility, remaining_time);
        let optimal_spread = self.calculate_optimal_spread(volatility, remaining_time, liquidity);
    
        let optimal_bid = self.round_to_tick_size(reservation_price - optimal_spread / 2.0, tick_size);
        let optimal_ask = self.round_to_tick_size(reservation_price + optimal_spread / 2.0, tick_size);
    
        println!("Mid price: {:.2}, Inventory: {:.8}, Volatility: {:.2}, Remaining time: {:.2}", 
                 mid_price, inventory, volatility, remaining_time);
        println!("Reservation price: {:.2}, Optimal spread: {:.2}", reservation_price, optimal_spread);
        println!("Rounded optimal bid: {:.2}, Rounded optimal ask: {:.2}", optimal_bid, optimal_ask);
    
        Some((optimal_bid, optimal_ask))
    }
}

fn parse_order_response(response: serde_json::Value) -> Option<Order> {
    if let Some(order_data) = response.get("result").and_then(|result| result.get("order")) {
        let order_id = order_data.get("order_id").and_then(|id| id.as_str()).map(|id| id.to_string());
        let instrument_name = order_data.get("instrument_name").and_then(|name| name.as_str()).map(|name| name.to_string());
        let amount = order_data.get("amount").and_then(|amount| amount.as_f64());
        let filled_amount = order_data.get("filled_amount").and_then(|amount| amount.as_f64());
        let price = order_data.get("price").and_then(|price| price.as_f64());
        let average_price = order_data.get("average_price").and_then(|price| price.as_f64());
        let direction = order_data.get("direction").and_then(|dir| dir.as_str()).map(|dir| dir.to_string());
        let order_state = order_data.get("order_state").and_then(|state| state.as_str()).map(|state| state.to_string());
        let order_type = order_data.get("order_type").and_then(|ty| ty.as_str()).map(|ty| ty.to_string());
        let timestamp = order_data.get("creation_timestamp").and_then(|ts| ts.as_i64()).map(|ts| ts as u64);

        Some(Order {
            order_id: order_id?,
            instrument_name: instrument_name?,
            amount: amount?,
            filled_amount: filled_amount?,
            price: price?,
            average_price: average_price?,
            direction: direction?,
            order_state: order_state?,
            order_type: order_type?,
            timestamp: timestamp,
        })
    } else {
        None
    }
}