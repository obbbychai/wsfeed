use tokio::sync::mpsc;
use anyhow::{Result, Context, anyhow};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::order_book::OrderBook;
use crate::instrument_names::{get_instruments, Instrument};
use crate::orderhandler::{OrderMessage, OrderType};
use crate::sharedstate::SharedState;
use std::collections::HashMap;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use serde_json::Value;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Deserialize)]
struct Parameters {
    risk_aversion: f64,
    book_depth: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MarketMakerState {
    WaitingForFeeds,
    Quoting,
    WaitingForEvent,
    CancelQuotes,
    Filled,
}


pub struct MarketMaker {
    portfolio_receiver: mpsc::Receiver<String>,
    order_book_receiver: mpsc::Receiver<Arc<OrderBook>>,
    volatility_receiver: mpsc::Receiver<String>,
    order_sender: mpsc::Sender<OrderMessage>,
    instruments: HashMap<String, Instrument>,
    parameters: Parameters,
    delta_total: f64,
    current_volatility: f64,
    shared_state: Arc<RwLock<SharedState>>,
    state: MarketMakerState,
    has_order_book: bool,
    has_portfolio: bool,
    has_volatility: bool,
    last_reservation_price: f64,
    tick_size: f64,
    current_instrument: String,
}



impl MarketMaker {
    pub async fn new(
        portfolio_receiver: mpsc::Receiver<String>,
        order_book_receiver: mpsc::Receiver<Arc<OrderBook>>,
        volatility_receiver: mpsc::Receiver<String>,
        order_sender: mpsc::Sender<OrderMessage>,
        shared_state: Arc<RwLock<SharedState>>,
    ) -> Result<Self> {
        let instruments = Self::fetch_instruments().await?;
        let parameters = Self::load_parameters("parameters.yaml")?;

        Ok(MarketMaker {
            portfolio_receiver,
            order_book_receiver,
            volatility_receiver,
            order_sender,
            instruments,
            parameters,
            delta_total: 0.0,
            current_volatility: 0.0,
            shared_state,
            state: MarketMakerState::WaitingForFeeds,
            has_order_book: false,
            has_portfolio: false,
            has_volatility: false,
            last_reservation_price: 0.0,
            tick_size: 2.5, // Assuming BTC-USD tick size, adjust as needed
            current_instrument: String::new(),
        })
    }

    async fn fetch_instruments() -> Result<HashMap<String, Instrument>> {
        get_instruments("BTC", "future").await.context("Failed to fetch instruments")
    }

    fn load_parameters(file_path: &str) -> Result<Parameters> {
        let mut file = File::open(file_path).context("Failed to open parameters file")?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).context("Failed to read parameters file")?;
        let parameters: Parameters = serde_yaml::from_str(&contents).context("Failed to parse parameters")?;
        Ok(parameters)
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            match self.state {
                MarketMakerState::WaitingForFeeds => self.handle_waiting_for_feeds().await?,
                MarketMakerState::Quoting => self.handle_quoting().await?,
                MarketMakerState::WaitingForEvent => self.handle_waiting_for_event().await?,
                MarketMakerState::CancelQuotes => self.handle_cancel_quotes().await?,
                MarketMakerState::Filled => self.handle_filled().await?,
            }
        }
    }

    async fn handle_waiting_for_feeds(&mut self) -> Result<()> {
        println!("Waiting for feeds...");
        loop {
            tokio::select! {
                Some(order_book) = self.order_book_receiver.recv() => {
                    println!("Received order book update for instrument: {}", order_book.instrument_name);
                    self.current_instrument = order_book.instrument_name.clone();
                    self.has_order_book = true;
                }
                Some(portfolio_data) = self.portfolio_receiver.recv() => {
                    println!("Received portfolio update");
                    self.update_delta_total(&portfolio_data);
                    self.has_portfolio = true;
                }
                Some(volatility_data) = self.volatility_receiver.recv() => {
                    println!("Received volatility update");
                    self.update_volatility(&volatility_data);
                    self.has_volatility = true;
                }
                else => {
                    println!("All channels closed, exiting...");
                    return Ok(());
                }
            }

            if self.has_order_book && self.has_portfolio && self.has_volatility {
                println!("All feeds received. Transitioning to Quoting state.");
                self.state = MarketMakerState::Quoting;
                break;
            }
        }
        Ok(())
    }



    async fn handle_quoting(&mut self) -> Result<()> {
        println!("Entering Quoting state");
        loop {
            tokio::select! {
                Some(order_book) = self.order_book_receiver.recv() => {
                    self.current_instrument = order_book.instrument_name.clone();
                    let quotes = self.calculate_quotes(&order_book).await?;
                    self.place_quotes(&order_book, quotes).await?;
                    self.last_reservation_price = self.calculate_reservation_price(
                        self.calculate_mid_price(&order_book),
                        self.delta_total,
                        self.current_volatility,
                        self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
                    );
                    println!("Transitioning to WaitingForEvent state");
                    self.state = MarketMakerState::WaitingForEvent;
                    break;
                }
            }
        }
        Ok(())
    }


    async fn handle_waiting_for_event(&mut self) -> Result<()> {
        println!("Entering WaitingForEvent state");
        loop {
            tokio::select! {
                Some(order_book) = self.order_book_receiver.recv() => {
                    let new_reservation_price = self.calculate_reservation_price(
                        self.calculate_mid_price(&order_book),
                        self.delta_total,
                        self.current_volatility,
                        self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
                    );
                    if (new_reservation_price - self.last_reservation_price).abs() >= 8.0 * self.tick_size {
                        println!("Reservation price moved by 8 ticks. Transitioning to CancelQuotes state");
                        self.state = MarketMakerState::CancelQuotes;
                        break;
                    }
                }
                Some(portfolio_data) = self.portfolio_receiver.recv() => {
                    self.update_delta_total(&portfolio_data);
                }
                Some(volatility_data) = self.volatility_receiver.recv() => {
                    self.update_volatility(&volatility_data);
                }
                else => {
                    println!("All channels closed, exiting...");
                    return Ok(());
                }
            }

            if self.check_orders_filled().await {
                println!("Orders filled. Transitioning to Filled state");
                self.state = MarketMakerState::Filled;
                break;
            }
        }
        Ok(())
    }

    async fn handle_cancel_quotes(&mut self) -> Result<()> {
        println!("Entering CancelQuotes state");
        
        let cancel_all_message = OrderMessage {
            instrument_name: self.current_instrument.clone(),
            order_type: OrderType::CancelAll,
            amount: None,
            price: None,
            is_buy: None,
        };

        self.order_sender.send(cancel_all_message)
            .await
            .context("Failed to send cancel all orders message")?;

        println!("Cancel all orders message sent. Transitioning back to Quoting state");
        self.state = MarketMakerState::Quoting;
        Ok(())
    }



    async fn handle_filled(&mut self) -> Result<()> {
        println!("Order filled for instrument: {}", self.current_instrument);
        // Implement any necessary logic for filled orders (e.g., updating portfolio, hedging)
        self.state = MarketMakerState::Quoting;
        Ok(())
    }

    async fn calculate_quotes(&self, order_book: &OrderBook) -> Result<(f64, f64)> {
        let instrument = self.instruments.get(&order_book.instrument_name)
            .ok_or_else(|| anyhow!("Instrument not found"))?;

        let mid_price = self.calculate_mid_price(order_book);
        let inventory = self.delta_total;
        let time_to_expiry = self.get_time_to_expiry(instrument);
        let liquidity = self.calculate_order_book_liquidity(order_book).await;
        
        let reservation_price = self.calculate_reservation_price(
            mid_price, 
            inventory, 
            self.current_volatility, 
            time_to_expiry
        );
        
        let optimal_spread = self.calculate_optimal_spread(self.current_volatility, time_to_expiry, liquidity);
        
        let bid_price = reservation_price - optimal_spread / 2.0;
        let ask_price = reservation_price + optimal_spread / 2.0;

        println!("Mid price: {}", mid_price);
        println!("Inventory: {}", inventory);
        println!("Time to expiry: {}", time_to_expiry);
        println!("Volatility: {}", self.current_volatility);
        println!("Liquidity: {}", liquidity);
        println!("Reservation price: {}", reservation_price);
        println!("Optimal spread: {}", optimal_spread);
        println!("Calculated quotes: Bid = {}, Ask = {}", bid_price, ask_price);
        
        Ok((bid_price, ask_price))
    }

    fn calculate_reservation_price(&self, mid_price: f64, inventory: f64, volatility: f64, time_to_expiry: f64) -> f64 {
        let gamma = self.parameters.risk_aversion;
        mid_price - gamma * inventory * volatility.powi(2) * time_to_expiry
    }

    fn calculate_optimal_spread(&self, volatility: f64, time_to_expiry: f64, liquidity: f64) -> f64 {
        let gamma = self.parameters.risk_aversion;
        (volatility.powi(2) * time_to_expiry / gamma) * (1.0 + (2.0 / gamma * liquidity).ln())
    }





    fn get_time_to_expiry(&self, instrument: &Instrument) -> f64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as f64;
        let expiry_time = instrument.expiration_timestamp as f64 / 1000.0; // Convert milliseconds to seconds
        let time_to_expiry = (expiry_time - now) / (365.0 * 24.0 * 60.0 * 60.0); // in years
        time_to_expiry.max(0.0) // Ensure non-negative
    }

    async fn calculate_order_book_liquidity(&self, order_book: &OrderBook) -> f64 {
        let bid_liquidity: f64 = order_book.bids.iter().rev().take(self.parameters.book_depth)
            .map(|(_, size)| size)
            .sum();
        let ask_liquidity: f64 = order_book.asks.iter().take(self.parameters.book_depth)
            .map(|(_, size)| size)
            .sum();
        (bid_liquidity + ask_liquidity) / 2.0
    }

    fn calculate_mid_price(&self, order_book: &OrderBook) -> f64 {
        let best_bid = order_book.bids.iter().next_back()
            .map(|(price, _)| price.0)
            .unwrap_or(0.0);
        let best_ask = order_book.asks.iter().next()
            .map(|(price, _)| price.0)
            .unwrap_or(0.0);
    
        if best_bid > 0.0 && best_ask > 0.0 {
            (best_bid + best_ask) / 2.0
        } else {
            0.0 // Or handle this case as appropriate for your system
        }
    }

    async fn place_quotes(&mut self, order_book: &OrderBook, quotes: (f64, f64)) -> Result<()> {
        let (bid, ask) = quotes;
        let bid_message = OrderMessage {
            instrument_name: order_book.instrument_name.clone(),
            order_type: OrderType::Limit,
            is_buy: Some(true),
            amount: Some(10.0),  // Adjust as needed
            price: Some(bid),
        };
        let ask_message = OrderMessage {
            instrument_name: order_book.instrument_name.clone(),
            order_type: OrderType::Limit,
            is_buy: Some(false),
            amount: Some(10.0),  // Adjust as needed
            price: Some(ask),
        };

        self.order_sender.send(bid_message).await.context("Failed to send bid message")?;
        self.order_sender.send(ask_message).await.context("Failed to send ask message")?;
        println!("Placed quotes for {}: Bid = {}, Ask = {}", order_book.instrument_name, bid, ask);
        Ok(())
    }

    async fn check_orders_filled(&self) -> bool {
        let shared_state = self.shared_state.read().await;
        shared_state.get_all_orders().iter().any(|order| order.order_state.as_deref() == Some("filled"))
    }



    fn update_delta_total(&mut self, portfolio_data: &str) {
        match serde_json::from_str::<Value>(portfolio_data) {
            Ok(json) => {
                if let Some(delta_total) = json["delta_total"].as_f64() {
                    self.delta_total = delta_total;
                    println!("Updated delta_total: {}", self.delta_total);
                } else {
                    println!("Failed to find delta_total in portfolio data");
                }
            }
            Err(e) => println!("Failed to parse portfolio data as JSON: {}", e),
        }
    }

    fn update_volatility(&mut self, volatility_data: &str) {
        match serde_json::from_str::<Value>(volatility_data) {
            Ok(json) => {
                if let Some(volatility) = json["volatility"].as_f64() {
                    self.current_volatility = volatility;
                    println!("Updated volatility: {}", self.current_volatility);
                } else {
                    println!("Failed to find volatility in volatility data");
                }
            }
            Err(e) => println!("Failed to parse volatility data as JSON: {}", e),
        }
    }

    fn print_best_bid_ask(&self, order_book: &Arc<OrderBook>) {
        let bids = &order_book.bids;
        let asks = &order_book.asks;

        let best_bid = bids.iter().next_back();
        let best_ask = asks.iter().next();

        match best_bid {
            Some((price, size)) => println!(
                "Best Bid for {}: Price = {:.2}, Size = {:.4}",
                order_book.instrument_name, price.0, size
            ),
            None => println!("No bids available for {}", order_book.instrument_name),
        }

        match best_ask {
            Some((price, size)) => println!(
                "Best Ask for {}: Price = {:.2}, Size = {:.4}",
                order_book.instrument_name, price.0, size
            ),
            None => println!("No asks available for {}", order_book.instrument_name),
        }
    }
}