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

const PRICE_MOVES: [f64; 4] = [-0.01, -0.005, 0.005, 0.01];
const TRANSIENT_BASE_PROB: f64 = 0.7;  // Base probability for price not moving
const ABSORBING_BASE_PROB: f64 = 0.15; // Base probability for price moving


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
        
        loop {
            tokio::select! {
                Some(order_book) = self.order_book_receiver.recv() => {
                    let new_reservation_price = self.calculate_reservation_price(
                        self.calculate_mid_price(&order_book),
                        self.current_position,
                        self.current_volatility,
                        self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
                    );
                    
                    if (new_reservation_price - self.last_reservation_price).abs() >= 16.0 * self.tick_size {
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
                    match oms_update {
                        OMSUpdate::OrderFilled(order_id) => {
                            println!("Order filled: {}", order_id);
                            self.set_state(MarketMakerState::Filled);
                            break;
                        }
                        OMSUpdate::OrderPartiallyFilled(order_id) => {
                            println!("Order partially filled: {}", order_id);
                            self.set_state(MarketMakerState::Filled);
                            break;
                        }
                        OMSUpdate::NewTrade(trade) => {
                            println!("New trade: {:?}", trade);
                            let size_change = if trade.direction == "buy" {
                                trade.amount
                            } else {
                                -trade.amount
                            };
                            self.current_position += size_change;
                            println!("Updated position from trade: {}", self.current_position);
                            self.set_state(MarketMakerState::Filled);
                            break;
                        }
                        OMSUpdate::OrderStateChanged(order_id, new_state) => {
                            if new_state == OrderState::Filled {
                                println!("Order {} filled", order_id);
                                self.set_state(MarketMakerState::Filled);
                                break;
                            }
                        }
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
        println!("Handling fill state - Immediate requote process starting");
        
        // 1. Cancel outstanding orders immediately
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
        
        // 2. Update position immediately
        let position_size = {
            let mut oms = self.oms.write().await;
            match oms.fetch_position(&self.current_instrument).await {
                Ok(position) => {
                    println!("Got immediate position update after fill: {}", position.size);
                    Some(position.size)
                },
                Err(e) => {
                    eprintln!("Failed to get position from API: {}", e);
                    None
                }
            }
        };
    
        if let Some(size) = position_size {
            self.current_position = size;
            println!("Updated current position to: {}", self.current_position);
        }
        
        // 3. Get latest market data immediately
        let order_book = self.order_book_receiver.recv().await
            .ok_or_else(|| anyhow!("Failed to get fresh order book"))?;
    
        // 4. Quick market state update
        while let Ok(portfolio_data) = self.portfolio_receiver.try_recv() {
            self.update_delta_total(&portfolio_data);
        }
        
        while let Ok(volatility_data) = self.volatility_receiver.try_recv() {
            self.update_volatility(&volatility_data);
        }
        
        // 5. Calculate and place new quotes immediately
        let quotes = self.calculate_quotes(&order_book).await?;
        self.place_quotes(&order_book, quotes).await?;
        
        // 6. Update reservation price
        self.last_reservation_price = self.calculate_reservation_price(
            self.calculate_mid_price(&order_book),
            self.current_position,
            self.current_volatility,
            self.get_time_to_expiry(&self.instruments[&self.current_instrument]),
        );
        
        println!("Successfully placed new quotes after fill. Reservation price: {}", 
                 self.last_reservation_price);
        
        self.set_state(MarketMakerState::WaitingForEvent);
        Ok(())
    }

    async fn calculate_quotes(&self, order_book: &OrderBook) -> Result<(f64, f64, f64, f64)> {
        let instrument = self.instruments.get(&order_book.instrument_name)
            .ok_or_else(|| anyhow!("Instrument not found"))?;
    
        // Use Stoikov micro-price instead of VWAP
        let micro_price = self.calculate_micro_price_stoikov(order_book);
        let time_to_expiry = self.get_time_to_expiry(instrument);
        let liquidity = self.calculate_order_book_liquidity(order_book).await;

        // Calculate Stoikov optimal spread
        let gamma = self.parameters.risk_aversion;
        let scaled_volatility = self.current_volatility * 0.01; // Scale volatility to decimal
        let optimal_spread = scaled_volatility.powi(2) * time_to_expiry / gamma * 
            (1.0 + (2.0 / gamma * liquidity).ln());

        // Ensure minimum spread of 2 ticks
        let spread = optimal_spread.max(2.0 * self.tick_size);
        let half_spread = spread / 2.0;

        // Calculate base quotes around micro price
        let mut bid_price = micro_price - half_spread;
        let mut ask_price = micro_price + half_spread;

        // Round to tick size
        bid_price = (bid_price / self.tick_size).round() * self.tick_size;
        ask_price = (ask_price / self.tick_size).round() * self.tick_size;

        // Basic size calculation
        let base_size = 10.0; // Fixed base size for now
        let bid_size = base_size;
        let ask_size = base_size;

        // Market sanity checks
        let best_bid = order_book.bids.iter().next_back()
            .map(|(price, _)| price.0)
            .unwrap_or(0.0);
        let best_ask = order_book.asks.iter().next()
            .map(|(price, _)| price.0)
            .unwrap_or(f64::MAX);

        // Don't post inside the spread
        if bid_price >= best_ask {
            bid_price = best_ask - self.tick_size;
        }
        if ask_price <= best_bid {
            ask_price = best_bid + self.tick_size;
        }
    
        println!("Quote calculation:");
        println!("Micro price: {}", micro_price);
        println!("Time to expiry: {}", time_to_expiry);
        println!("Volatility: {}", self.current_volatility);
        println!("Liquidity: {}", liquidity);
        println!("Optimal spread: {}", spread);
        println!("Final spread: {}", ask_price - bid_price);
        println!("Best bid: {}, Best ask: {}", best_bid, best_ask);
        
        Ok((bid_price, ask_price, bid_size, ask_size))
    }


    pub async fn initialize_position(&mut self) -> Result<()> {
        let mut oms = self.oms.write().await;
        
        // Initialize OMS with subscriptions and initial state
        oms.initialize(&self.current_instrument).await?;
        
        // Get current position once at startup
        let position = oms.fetch_position(&self.current_instrument).await?;
        self.current_position = position.size;
        
        println!("Market Maker initialized with position: {}", self.current_position);
        
        Ok(())
    }




    // Stoikov Model
    fn calculate_micro_price_stoikov(&self, order_book: &OrderBook) -> f64 {
        let mid_price = self.calculate_mid_price(order_book);
        let imbalance = self.calculate_order_imbalance(order_book);
        
        // Calculate transition probabilities based on order book imbalance
        let q = self.calculate_transient_probability(imbalance);
        let r = self.calculate_absorbing_probability(imbalance);
        
        // Apply Stoikov's first-step approximation formula
        let expected_move = PRICE_MOVES.iter()
            .zip(r.iter())
            .map(|(&k, &prob)| k * prob)
            .sum::<f64>();
            
        let q_inv = 1.0 / (1.0 - q);
        
        mid_price + q_inv * expected_move
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
        let gamma = self.parameters.risk_aversion * 0.01; // Scale down risk aversion
        
        // Normalize inventory relative to max position
        let normalized_inventory = inventory / Self::MAX_POSITION_USD;
        
        // Scale volatility down
        let scaled_volatility = (volatility * 0.01) * 0.1;
        
        // Calculate inventory risk with better scaling
        let inventory_risk = gamma * normalized_inventory * scaled_volatility.powi(2) * time_to_expiry;
        
        // Apply inventory risk to mid price
        let reservation_price = mid_price - (inventory_risk * mid_price);
        
        // Ensure the reservation price doesn't deviate too far from mid price
        let max_deviation = mid_price * 0.001; // 0.1% max deviation
        reservation_price.clamp(
            mid_price - max_deviation,
            mid_price + max_deviation
        )
    }

    fn calculate_optimal_spread(&self, volatility: f64, time_to_expiry: f64, liquidity: f64) -> f64 {
        let gamma = self.parameters.risk_aversion;
        let scaled_volatility = volatility * 0.01;
        
        // Pure Stoikov spread: s* = γσ²T(1 + ln(1 + γ/k))
        // where k is the order arrival rate (approximated by liquidity)
        let base_spread = scaled_volatility.powi(2) * time_to_expiry / gamma * 
            (1.0 + (2.0 / gamma * liquidity).ln());
        
        // Only round to tick size, no other bounds
        (base_spread / self.tick_size).round() * self.tick_size
    }

    fn get_time_to_expiry(&self, instrument: &Instrument) -> f64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as f64;
        let expiry_time = instrument.expiration_timestamp as f64 / 1000.0; // Convert milliseconds to seconds
        let time_to_expiry = (expiry_time - now) / (365.0 * 24.0 * 60.0 * 60.0); // in years
        time_to_expiry.max(0.0) // Ensure non-negative
    }

    async fn calculate_order_book_liquidity(&self, order_book: &OrderBook) -> f64 {
        let depth = self.parameters.book_depth;
        
        let bid_liquidity: f64 = order_book.bids.iter().rev()
            .take(depth)
            .map(|(_, size)| size)
            .sum();
            
        let ask_liquidity: f64 = order_book.asks.iter()
            .take(depth)
            .map(|(_, size)| size)
            .sum();
            
        bid_liquidity + ask_liquidity
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

    fn calculate_order_imbalance(&self, order_book: &OrderBook) -> f64 {
        let (bid_vol, ask_vol) = self.get_volume_imbalance(order_book, 5);
        (bid_vol - ask_vol) / (bid_vol + ask_vol)
    }

    fn calculate_transient_probability(&self, imbalance: f64) -> f64 {
        // Probability decreases with higher imbalance (more likely to move)
        let base_prob = TRANSIENT_BASE_PROB;
        base_prob * (1.0 - imbalance.abs().min(1.0))
    }

    fn calculate_absorbing_probability(&self, imbalance: f64) -> Vec<f64> {
        let base_prob = ABSORBING_BASE_PROB;
        let imb = imbalance.clamp(-1.0, 1.0);
        
        // Higher probability of moving in direction of imbalance
        let up_prob = base_prob * (1.0 + imb);
        let down_prob = base_prob * (1.0 - imb);
        
        // Probability distribution across price moves
        // [-0.01, -0.005, 0.005, 0.01]
        vec![
            0.2 * down_prob,  // Large down move
            0.8 * down_prob,  // Small down move
            0.8 * up_prob,    // Small up move
            0.2 * up_prob     // Large up move
        ]
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
        let max_skew_ticks = 8.0; // Increased from 12.0
        base_skew * max_skew_ticks * self.tick_size
    }

    fn get_volume_imbalance(&self, order_book: &OrderBook, depth: usize) -> (f64, f64) {
        let bid_volume = order_book.bids.iter().rev()
            .take(depth)
            .map(|(_, size)| size)
            .sum();
            
        let ask_volume = order_book.asks.iter()
            .take(depth)
            .map(|(_, size)| size)
            .sum();
            
        (bid_volume, ask_volume)
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

    fn handle_position_update(&mut self, new_position: f64) {
        if (new_position - self.current_position).abs() > 0.0001 {
            println!("Position change detected - Old: {}, New: {}", self.current_position, new_position);
            self.current_position = new_position;
            
            // Only change state if position change is significant
            if (new_position - self.current_position).abs() > self.base_order_size {
                self.set_state(MarketMakerState::CancelQuotes);
            }
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