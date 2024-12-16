use tokio::sync::mpsc;
use anyhow::{Result, Context, anyhow};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::order_book::OrderBook;
use crate::instrument_names::{get_instruments, Instrument};
use crate::orderhandler::{OrderMessage, OrderType};
use crate::oms::{OrderManagementSystem, OMSUpdate, OrderState};
use std::collections::HashMap;
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH, Duration};

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
    state: MarketMakerState,
    has_order_book: bool,
    has_portfolio: bool,
    has_volatility: bool,
    last_reservation_price: f64,
    tick_size: f64,
    current_instrument: String,
    current_position: f64,
    base_order_size: f64,
    oms_update_receiver: mpsc::Receiver<OMSUpdate>,
    oms: Arc<RwLock<OrderManagementSystem>>,
}

impl MarketMaker {
    const CONTRACT_SIZE: f64 = 10.0;  // Minimum contract size in USD
    const MAX_POSITION_USD: f64 = 800.0;  // Maximum position in USD
    const MIN_ORDER_SIZE: f64 = 10.0;  // Minimum order size in USD (1 contract)
    const MAX_ORDER_SIZE: f64 = 100.0;  // Maximum order size in USD
    const RISK_LIMIT_FACTOR: f64 = 0.5; // Increased from 0.2 to be more aggressive
    const POSITION_REDUCTION_URGENCY: f64 = 2.0; // New constant for aggressive skewing


    fn set_state(&mut self, new_state: MarketMakerState) {
        println!("State transition: {:?} -> {:?}", self.state, new_state);
        self.state = new_state;
    }

    pub async fn new(
        portfolio_receiver: mpsc::Receiver<String>,
        order_book_receiver: mpsc::Receiver<Arc<OrderBook>>,
        volatility_receiver: mpsc::Receiver<String>,
        order_sender: mpsc::Sender<OrderMessage>,
        oms: Arc<RwLock<OrderManagementSystem>>,
        oms_update_receiver: mpsc::Receiver<OMSUpdate>,
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
            oms,
            oms_update_receiver,
            state: MarketMakerState::WaitingForFeeds,
            has_order_book: false,
            has_portfolio: false,
            has_volatility: false,
            last_reservation_price: 0.0,
            tick_size: 2.5,
            current_instrument: String::new(),
            current_position: 0.0,
            base_order_size: 0.1, // Reduced base size for more granular sizing
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
                println!("All feeds received. Initializing position...");
                self.initialize_position().await?;
                println!("All feeds received. Transitioning to Quoting state.");
                self.state = MarketMakerState::Quoting;
                break;
            }
        }
        Ok(())
    }

    async fn handle_quoting(&mut self) -> Result<()> {
        println!("Entering Quoting state");
        if let Some(order_book) = self.order_book_receiver.recv().await {
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
        }
        Ok(())
    }

    async fn handle_waiting_for_event(&mut self) -> Result<()> {
        println!("Entering WaitingForEvent state");
        let mut position_check_interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            tokio::select! {
                _ = position_check_interval.tick() => {
                    // Scope the OMS lock to avoid conflicting with other mutable self operations
                    let position_size = {
                        let mut oms = self.oms.write().await;
                        match oms.fetch_position(&self.current_instrument).await {
                            Ok(position) => Some(position.size),
                            Err(e) => {
                                eprintln!("Error getting position: {}", e);
                                None
                            }
                        }
                    }; // OMS lock is dropped here

                    // Process position update outside the lock
                    if let Some(size) = position_size {
                        if (size - self.current_position).abs() > 0.0001 {
                            println!("Position changed from {} to {}", self.current_position, size);
                            // **Corrected comparison**
                            if (size - self.current_position).abs() > self.base_order_size {
                                self.set_state(MarketMakerState::CancelQuotes);
                                break;
                            }
                            self.current_position = size;
                        }
                    }
                }
                
                Some(order_book) = self.order_book_receiver.recv() => {
                    let new_reservation_price = self.calculate_reservation_price(
                        self.calculate_mid_price(&order_book),
                        self.delta_total,
                        self.current_volatility,
                        self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
                    );
                    
                    if (new_reservation_price - self.last_reservation_price).abs() >= 16.0 * self.tick_size {
                        println!("Price moved significantly, transitioning to CancelQuotes");
                        self.set_state(MarketMakerState::CancelQuotes);
                        break;
                    }
                }
                
                Some(portfolio_data) = self.portfolio_receiver.recv() => {
                    self.update_delta_total(&portfolio_data);
                }
                
                Some(volatility_data) = self.volatility_receiver.recv() => {
                    self.update_volatility(&volatility_data);
                }
                
                Some(oms_update) = self.oms_update_receiver.recv() => {
                    let should_break = match oms_update {
                        OMSUpdate::OrderFilled(order_id) => {
                            println!("Order filled: {}", order_id);
                            self.set_state(MarketMakerState::Filled);
                            true
                        }
                        OMSUpdate::OrderPartiallyFilled(order_id) => {
                            println!("Order partially filled: {}", order_id);
                            self.set_state(MarketMakerState::Filled);
                            true
                        }
                        OMSUpdate::NewTrade(trade) => {
                            println!("New trade: {:?}", trade);
                            // Let OMS track the trade, we'll get position updates via API
                            false
                        }
                        OMSUpdate::OrderStateChanged(order_id, new_state) => {
                            if new_state == OrderState::Filled {
                                println!("Order {} filled", order_id);
                                self.set_state(MarketMakerState::Filled);
                                true
                            } else {
                                false
                            }
                        }
                    };
    
                    if should_break {
                        break;
                    }
                }
                
                else => {
                    println!("All channels closed");
                    return Ok(());
                }
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
        println!("Handling fill state");
        
        // Cancel outstanding orders
        let cancel_message = OrderMessage {
            instrument_name: self.current_instrument.clone(),
            order_type: OrderType::CancelAll,
            amount: None,
            price: None,
            is_buy: None,
        };
        
        self.order_sender.send(cancel_message)
            .await
            .context("Failed to send cancel message after fill")?;
        
            let position_size = {
                let mut oms = self.oms.write().await;
                match oms.fetch_position(&self.current_instrument).await {
                    Ok(position) => {
                        println!("Got position update from API: {}", position.size);
                        Some(position.size)
                    },
                    Err(e) => {
                        eprintln!("Failed to get position from API: {}", e);
                        None
                    }
                }
            }; // OMS lock is dropped here
    
        // Update position outside the lock if we got a valid update
        if let Some(size) = position_size {
            self.current_position = size;
            println!("Updated current position to: {}", self.current_position);
        }
        
        // Small delay to allow for order processing
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // Try to get latest market data
        if let Some(order_book) = self.order_book_receiver.recv().await {
            // Update market state with any new portfolio data
            while let Ok(portfolio_data) = self.portfolio_receiver.try_recv() {
                self.update_delta_total(&portfolio_data);
            }
            
            // Update market state with any new volatility data
            while let Ok(volatility_data) = self.volatility_receiver.try_recv() {
                self.update_volatility(&volatility_data);
            }
            
            // Calculate new quotes based on updated state
            let quotes = match self.calculate_quotes(&order_book).await {
                Ok(quotes) => quotes,
                Err(e) => {
                    eprintln!("Failed to calculate quotes: {}", e);
                    self.set_state(MarketMakerState::WaitingForEvent);
                    return Ok(());
                }
            };
    
            // Place new quotes
            if let Err(e) = self.place_quotes(&order_book, quotes).await {
                eprintln!("Failed to place quotes: {}", e);
                self.set_state(MarketMakerState::WaitingForEvent);
                return Ok(());
            }
            
            // Update reservation price
            self.last_reservation_price = self.calculate_reservation_price(
                self.calculate_mid_price(&order_book),
                self.delta_total,
                self.current_volatility,
                self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
            );
            
            println!("Successfully placed new quotes with reservation price: {}", 
                     self.last_reservation_price);
        } else {
            eprintln!("Failed to receive order book update");
        }
        
        self.set_state(MarketMakerState::WaitingForEvent);
        Ok(())
    }

    async fn calculate_quotes(&self, order_book: &OrderBook) -> Result<(f64, f64, f64, f64)> {
        let instrument = self.instruments.get(&order_book.instrument_name)
            .ok_or_else(|| anyhow!("Instrument not found"))?;
    
        let mid_price = self.calculate_mid_price(order_book);
        let inventory = self.current_position;
        let time_to_expiry = self.get_time_to_expiry(instrument);
        let liquidity = self.calculate_order_book_liquidity(order_book).await;
        
        let reservation_price = self.calculate_reservation_price(
            mid_price, 
            inventory, 
            self.current_volatility, 
            time_to_expiry
        );
        
        let optimal_spread = self.calculate_optimal_spread(self.current_volatility, time_to_expiry, liquidity);
        
        // Calculate the skew factor
        let skew_factor = self.calculate_skew_factor(inventory);
        
        // Apply the skew to the bid and ask prices
        let bid_size = self.calculate_order_size(true);
        let ask_size = self.calculate_order_size(false);
        let bid_price = reservation_price - optimal_spread / 2.0 - skew_factor;
        let ask_price = reservation_price + optimal_spread / 2.0 + skew_factor;
    
        println!("Mid price: {}", mid_price);
        println!("Inventory: {}", inventory);
        println!("Time to expiry: {}", time_to_expiry);
        println!("Volatility: {}", self.current_volatility);
        println!("Liquidity: {}", liquidity);
        println!("Reservation price: {}", reservation_price);
        println!("Optimal spread: {}", optimal_spread);
        println!("Skew factor: {}", skew_factor);
        println!("Calculated quotes: Bid = {}, Ask = {}", bid_price, ask_price);
        
        Ok((bid_price, ask_price, bid_size, ask_size))
    }


    async fn initialize_position(&mut self) -> Result<()> {
        // Get position from OMS API
        let mut oms = self.oms.write().await;
        let position = oms.fetch_position(&self.current_instrument).await?;
        
        // Update our position tracking (position.size will be 0.0 if no position exists)
        self.current_position = position.size;
        
        // Also get any open orders
        let open_orders = oms.fetch_open_orders(&self.current_instrument).await?;
        println!("Current position: {}, Open orders: {}", self.current_position, open_orders.len());
        
        // Get recent trades
        let recent_trades = oms.fetch_recent_trades(&self.current_instrument).await?;
        println!("Recent trades count: {}", recent_trades.len());
        
        Ok(())
    }

    
    fn calculate_order_size(&self, is_bid: bool) -> f64 {
        let time_to_expiry = self.get_time_to_expiry(
            &self.instruments[&self.current_instrument]
        );
    
        let position_usd = self.current_position * self.last_reservation_price;
        let effective_position_limit = Self::MAX_POSITION_USD * 
            (1.0 - Self::RISK_LIMIT_FACTOR * time_to_expiry.sqrt());
    
        let position_utilization = position_usd / effective_position_limit;
    
        // More aggressive size asymmetry
        let size_multiplier = if is_bid {
            if position_utilization > 0.0 {
                // If long, reduce bid size more aggressively
                0.5 * (1.0 - position_utilization)
            } else {
                1.0 - position_utilization
            }
        } else {
            if position_utilization < 0.0 {
                // If short, reduce ask size more aggressively
                0.5 * (1.0 + position_utilization)
            } else {
                1.0 + position_utilization
            }
        };
    
        let volatility_scaling = 1.0 / (1.0 + self.current_volatility).sqrt();
        let time_factor = (time_to_expiry + 0.01).sqrt();
        let mut base_size = 100.0 * volatility_scaling * time_factor;
        
        let position_dampener = (1.0 - position_utilization.abs()).max(0.2);
        base_size *= size_multiplier * position_dampener;
    
        let size = base_size.clamp(Self::MIN_ORDER_SIZE, Self::MAX_ORDER_SIZE);
        let contracts = (size / Self::CONTRACT_SIZE).round();
        let rounded_size = contracts * Self::CONTRACT_SIZE;
        
        rounded_size.max(Self::CONTRACT_SIZE)
    }

    fn calculate_reservation_price(&self, mid_price: f64, inventory: f64, volatility: f64, time_to_expiry: f64) -> f64 {
        let gamma = self.parameters.risk_aversion;
        
        // Enhanced reservation price with inventory risk
        let inventory_risk = gamma * inventory * volatility.powi(2) * time_to_expiry;
        
        // Add urgency factor as position approaches limits
        let position_urgency = (inventory / Self::MAX_POSITION_USD).powi(3);
        let urgency_adjustment = position_urgency * volatility * self.tick_size * 5.0;
        
        mid_price - inventory_risk - urgency_adjustment
    }

    fn calculate_optimal_spread(&self, volatility: f64, time_to_expiry: f64, liquidity: f64) -> f64 {
        let gamma = self.parameters.risk_aversion;
        
        // Calculate base spread from Stoikov model
        let base_spread = (volatility.powi(2) * time_to_expiry / gamma) * 
            (1.0 + (2.0 / gamma * liquidity).ln());
        
        // Add position-based spread widening
        let position_factor = (self.current_position / Self::MAX_POSITION_USD).powi(2);
        let position_spread = base_spread * (1.0 + position_factor);
        
        // Add liquidity-based spread component
        let liquidity_factor = (1.0 / liquidity).sqrt();
        position_spread * (1.0 + liquidity_factor)
    }

    fn get_time_to_expiry(&self, instrument: &Instrument) -> f64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as f64;
        let expiry_time = instrument.expiration_timestamp as f64 / 1000.0; // Convert milliseconds to seconds
        let time_to_expiry = (expiry_time - now) / (365.0 * 24.0 * 60.0 * 60.0); // in years
        time_to_expiry.max(0.0) // Ensure non-negative
    }

    async fn calculate_order_book_liquidity(&self, order_book: &OrderBook) -> f64 {
        let depth = self.parameters.book_depth;
        let decay_factor: f64 = 0.85; // Specify f64 type explicitly
        
        let bid_liquidity: f64 = order_book.bids.iter().rev()
            .take(depth)
            .enumerate()
            .map(|(i, (_, size))| size * decay_factor.powi(i as i32))
            .sum();
            
        let ask_liquidity: f64 = order_book.asks.iter()
            .take(depth)
            .enumerate()
            .map(|(i, (_, size))| size * decay_factor.powi(i as i32))
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




    fn calculate_skew_factor(&self, position: f64) -> f64 {
        let time_to_expiry = self.get_time_to_expiry(
            &self.instruments[&self.current_instrument]
        );
        
        // More aggressive position normalization
        let normalized_position = (position / Self::MAX_POSITION_USD).clamp(-1.0, 1.0);
        
        // Increase skew based on position reduction urgency
        let base_skew = normalized_position * 
            self.current_volatility * 
            time_to_expiry.sqrt() * 
            Self::POSITION_REDUCTION_URGENCY; // Added urgency multiplier
        
        // Increased max skew ticks for more aggressive pricing
        let max_skew_ticks = 20.0; // Increased from 12.0
        base_skew * max_skew_ticks * self.tick_size
    }












    async fn place_quotes(&mut self, order_book: &OrderBook, quotes: (f64, f64, f64, f64)) -> Result<()> {
        let (bid, ask, mut bid_size, mut ask_size) = quotes;
        
        // Round prices to tick size
        let bid_ticks = (bid / self.tick_size).round();
        let ask_ticks = (ask / self.tick_size).round();
        let rounded_bid = bid_ticks * self.tick_size;
        let rounded_ask = ask_ticks * self.tick_size;

        // Convert USD sizes to contracts and round
        bid_size = (bid_size / Self::CONTRACT_SIZE).round() * Self::CONTRACT_SIZE;
        ask_size = (ask_size / Self::CONTRACT_SIZE).round() * Self::CONTRACT_SIZE;

        // Ensure minimum contract size
        bid_size = bid_size.max(Self::CONTRACT_SIZE);
        ask_size = ask_size.max(Self::CONTRACT_SIZE);

        let bid_message = OrderMessage {
            instrument_name: order_book.instrument_name.clone(),
            order_type: OrderType::Limit,
            is_buy: Some(true),
            amount: Some(bid_size),
            price: Some(rounded_bid),
        };
        let ask_message = OrderMessage {
            instrument_name: order_book.instrument_name.clone(),
            order_type: OrderType::Limit,
            is_buy: Some(false),
            amount: Some(ask_size),
            price: Some(rounded_ask),
        };
    
        self.order_sender.send(bid_message).await.context("Failed to send bid message")?;
        self.order_sender.send(ask_message).await.context("Failed to send ask message")?;
        println!("Placed quotes for {}: Bid = {} (size: {}), Ask = {} (size: {})", 
                 order_book.instrument_name, rounded_bid, bid_size, rounded_ask, ask_size);
        Ok(())
    }

    async fn check_orders_filled(&self) -> bool {
        let oms = self.oms.read().await;
        oms.get_active_orders()
            .iter()
            .any(|order| order.state == "filled")
    }

    fn update_delta_total(&mut self, portfolio_data: &str) {
        match serde_json::from_str::<Value>(portfolio_data) {
            Ok(json) => {
                if let Some(delta_total) = json["delta_total"].as_f64() {
                    self.delta_total = delta_total;
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
                } else {
                    println!("Failed to find volatility in volatility data");
                }
            }
            Err(e) => println!("Failed to parse volatility data as JSON: {}", e),
        }
    }



    async fn update_current_position(&mut self) -> Result<()> {
        // Obtain a write lock to get mutable access
        let mut oms = self.oms.write().await;
        
        // Fetch recent trades with mutable access
        let trades = oms.fetch_recent_trades(&self.current_instrument).await?;
        
        // Calculate position from trades
        let position_change: f64 = trades.iter()
            .map(|trade| {
                if trade.direction == "buy" {
                    trade.amount
                } else {
                    -trade.amount
                }
            })
            .sum();
    
        self.current_position += position_change;
        println!("Updated current position to: {}", self.current_position);
        Ok(())
    }


}