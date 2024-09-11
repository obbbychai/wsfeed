use std::sync::Arc;
use tokio::sync::broadcast;
use crate::shared_state::SharedState;
use crate::eventbucket::{EventBucket, Event};
use crate::oms::OrderManagementSystem;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MarketMaker {
    shared_state: Arc<SharedState>,
    oms: Arc<OrderManagementSystem>,
    gamma: f64,
    depth_percentage: f64,
    event_rx: broadcast::Receiver<Event>,
}

impl MarketMaker {
    pub fn new(shared_state: Arc<SharedState>, oms: Arc<OrderManagementSystem>, gamma: f64, depth_percentage: f64, event_bucket: Arc<EventBucket>) -> Self {
        let event_rx = event_bucket.subscribe();
        MarketMaker {
            shared_state,
            oms,
            gamma,
            depth_percentage,
            event_rx,
        }
    }

    pub async fn run(&mut self) {
        while let Ok(event) = self.event_rx.recv().await {
            match event {
                Event::OrderBookUpdate(_order_book) => {
                    println!("Received order book update");
                    self.update_prices().await;
                }
                Event::PortfolioUpdate(_portfolio) => {
                    println!("Received portfolio update");
                    self.update_prices().await;
                }
                Event::VolatilityUpdate(volatility) => {
                    println!("Received volatility update: {}", volatility);
                    self.update_prices().await;
                }
            }
        }
    }

    async fn update_prices(&self) {
        if let (Some(order_book), Some(portfolio), Some(volatility)) = (
            self.shared_state.get_latest_order_book().await,
            self.shared_state.get_latest_portfolio().await,
            self.shared_state.get_latest_volatility().await,
        ) {
            if let Some((bid, ask)) = self.calculate_bid_ask_prices().await {
                println!("Calculated new prices - Bid: {}, Ask: {}", bid, ask);
                // Here you would send the order to the OMS
                // self.oms.place_order(bid, ask).await;
            }
        }
    }



    fn calculate_reservation_price(&self, mid_price: f64, inventory: f64, volatility: f64, remaining_time: f64) -> f64 {
        mid_price - inventory * self.gamma * volatility.powi(2) * remaining_time
    }

    fn calculate_optimal_spread(&self, volatility: f64, remaining_time: f64, liquidity: f64) -> f64 {
        (self.gamma * volatility.powi(2) * remaining_time / liquidity).sqrt()
    }

    pub async fn calculate_bid_ask_prices(&self) -> Option<(f64, f64)> {
        let (order_book, portfolio, volatility) = tokio::join!(
            self.shared_state.get_latest_order_book(),
            self.shared_state.get_latest_portfolio(),
            self.shared_state.get_latest_volatility()
        );

        let (order_book, portfolio, volatility) = match (order_book, portfolio, volatility) {
            (Some(ob), Some(p), Some(v)) => (ob, p, v),
            _ => return None,
        };

        let mid_price = order_book.get_mid_price()?;
        let inventory = portfolio.delta_total;
        let liquidity = order_book.get_liquidity_depth(self.depth_percentage);

        let instrument = self.shared_state.get_instrument("BTC-PERPETUAL").await?;
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