use tokio::sync::mpsc;
use std::error::Error as StdError;
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

#[derive(Deserialize)]
struct Parameters {
    risk_aversion: f64,
    book_depth: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum MarketMakerState {
    ReadyToQuote,
    Quoted,
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
    current_instrument: String,
}

impl MarketMaker {
    pub async fn new(
        portfolio_receiver: mpsc::Receiver<String>,
        order_book_receiver: mpsc::Receiver<Arc<OrderBook>>,
        volatility_receiver: mpsc::Receiver<String>,
        order_sender: mpsc::Sender<OrderMessage>,
        shared_state: Arc<RwLock<SharedState>>,
    ) -> Result<Self, Box<dyn StdError>> {
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
            state: MarketMakerState::ReadyToQuote,
            current_instrument: String::new(),
        })
    }

    async fn fetch_instruments() -> Result<HashMap<String, Instrument>, Box<dyn StdError>> {
        let instruments = get_instruments("BTC", "future").await?;
        Ok(instruments)
    }

    fn load_parameters(file_path: &str) -> Result<Parameters, Box<dyn StdError>> {
        let mut file = File::open(file_path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let parameters: Parameters = serde_yaml::from_str(&contents)?;
        Ok(parameters)
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn StdError>> {
        loop {
            match self.state {
                MarketMakerState::ReadyToQuote => self.handle_ready_to_quote().await?,
                MarketMakerState::Quoted => self.handle_quoted().await?,
                MarketMakerState::Filled => self.handle_filled().await?,
            }
        }
    }

    async fn handle_ready_to_quote(&mut self) -> Result<(), Box<dyn StdError>> {
        tokio::select! {
            Some(order_book) = self.order_book_receiver.recv() => {
                self.current_instrument = order_book.instrument_name.clone();
                let quotes = self.calculate_quotes(&order_book).await?;
                self.place_quotes(quotes).await?;
                self.state = MarketMakerState::Quoted;
            }
            Some(portfolio_data) = self.portfolio_receiver.recv() => {
                self.update_delta_total(&portfolio_data);
            }
            Some(volatility_data) = self.volatility_receiver.recv() => {
                self.update_volatility(&volatility_data);
            }
        }
        Ok(())
    }

    async fn handle_quoted(&mut self) -> Result<(), Box<dyn StdError>> {
        tokio::select! {
            Some(order_book) = self.order_book_receiver.recv() => {
                // Optionally update quotes based on new order book
                self.print_best_bid_ask(&order_book);
            }
            Some(portfolio_data) = self.portfolio_receiver.recv() => {
                self.update_delta_total(&portfolio_data);
            }
            Some(volatility_data) = self.volatility_receiver.recv() => {
                self.update_volatility(&volatility_data);
            }
        }
        // Check if any orders have been filled
        if self.check_orders_filled().await {
            self.state = MarketMakerState::Filled;
        }
        Ok(())
    }

    async fn handle_filled(&mut self) -> Result<(), Box<dyn StdError>> {
        println!("Order filled for instrument: {}", self.current_instrument);
        // Implement any necessary logic for filled orders (e.g., updating portfolio, hedging)
        self.state = MarketMakerState::ReadyToQuote;
        Ok(())
    }

    async fn calculate_quotes(&self, order_book: &OrderBook) -> Result<(f64, f64), Box<dyn StdError>> {
        let best_bid = order_book.bids.iter().next_back().map(|(price, _)| price.0).unwrap_or(0.0);
        let best_ask = order_book.asks.iter().next().map(|(price, _)| price.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        let spread = 0.01 * mid_price; // 1% spread, adjust as needed
        Ok((mid_price - spread / 2.0, mid_price + spread / 2.0))
    }

    async fn place_quotes(&mut self, quotes: (f64, f64)) -> Result<(), Box<dyn StdError>> {
        let (bid, ask) = quotes;
        let bid_message = OrderMessage {
            instrument_name: self.current_instrument.clone(),
            is_buy: true,
            amount: 10.0,  // Adjust as needed
            order_type: OrderType::Limit,
            price: Some(bid),
        };
        let ask_message = OrderMessage {
            instrument_name: self.current_instrument.clone(),
            is_buy: false,
            amount: 10.0,  // Adjust as needed
            order_type: OrderType::Limit,
            price: Some(ask),
        };

        self.order_sender.send(bid_message).await?;
        self.order_sender.send(ask_message).await?;
        println!("Placed quotes for {}: Bid = {}, Ask = {}", self.current_instrument, bid, ask);
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