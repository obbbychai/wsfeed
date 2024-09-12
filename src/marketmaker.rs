use std::sync::Arc;
use tokio::sync::broadcast;
use crate::eventbucket::{EventBucket, Event};
use crate::oms::OrderManagementSystem;
use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;
use crate::instrument_names::Instrument;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MarketMaker {
    oms: Arc<OrderManagementSystem>,
    gamma: f64,
    depth_percentage: f64,
    event_rx: broadcast::Receiver<Event>,
    latest_order_book: Option<OrderBook>,
    latest_portfolio: Option<PortfolioData>,
    latest_volatility: Option<f64>,
    latest_instrument: Option<Instrument>,
}

impl MarketMaker {
    pub fn new(oms: Arc<OrderManagementSystem>, gamma: f64, depth_percentage: f64, event_bucket: Arc<EventBucket>) -> Self {
        let event_rx = event_bucket.subscribe();
        MarketMaker {
            oms,
            gamma,
            depth_percentage,
            event_rx,
            latest_order_book: None,
            latest_portfolio: None,
            latest_volatility: None,
            latest_instrument: None,
        }
    }

    pub async fn run(&mut self) {
        while let Ok(event) = self.event_rx.recv().await {
            match event {
                Event::OrderBookUpdate(order_book) => {
                    println!("Received order book update");
                    self.latest_order_book = Some(order_book);
                    self.update_prices().await;
                }
                Event::PortfolioUpdate(portfolio) => {
                    println!("Received portfolio update");
                    self.latest_portfolio = Some(portfolio);
                    self.update_prices().await;
                }
                Event::VolatilityUpdate(volatility) => {
                    println!("Received volatility update: {}", volatility);
                    self.latest_volatility = Some(volatility);
                    self.update_prices().await;
                }
            }
        }
    }

    async fn update_prices(&self) {
        if let Some((bid, ask)) = self.calculate_bid_ask_prices().await {
            println!("Calculated new prices - Bid: {}, Ask: {}", bid, ask);
            // Here you would send the order to the OMS
            // self.oms.place_order(bid, ask).await;
        }
    }

    fn calculate_reservation_price(&self, mid_price: f64, inventory: f64, volatility: f64, remaining_time: f64) -> f64 {
        mid_price - inventory * self.gamma * volatility.powi(2) * remaining_time
    }

    fn calculate_optimal_spread(&self, volatility: f64, remaining_time: f64, liquidity: f64) -> f64 {
        (self.gamma * volatility.powi(2) * remaining_time / liquidity).sqrt()
    }

    pub async fn calculate_bid_ask_prices(&self) -> Option<(f64, f64)> {
        let (order_book, portfolio, volatility, instrument) = match (&self.latest_order_book, &self.latest_portfolio, self.latest_volatility, &self.latest_instrument) {
            (Some(ob), Some(p), Some(v), Some(i)) => (ob, p, v, i),
            _ => return None,
        };

        let mid_price = order_book.get_mid_price()?;
        let inventory = portfolio.delta_total;
        let liquidity = order_book.get_liquidity_depth(self.depth_percentage);

        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as f64;
        let creation_time = instrument.creation_timestamp as f64 / 1000.0;
        let expiration_time = instrument.expiration_timestamp as f64 / 1000.0;
        let total_time = expiration_time - creation_time;
        let remaining_time = (expiration_time - current_time) / total_time;

        let reservation_price = self.calculate_reservation_price(mid_price, inventory, volatility, remaining_time);
        let optimal_spread = self.calculate_optimal_spread(volatility, remaining_time, liquidity);

        let bid = reservation_price - optimal_spread / 2.0;
        let ask = reservation_price + optimal_spread / 2.0;

        Some((bid, ask))
    }
}