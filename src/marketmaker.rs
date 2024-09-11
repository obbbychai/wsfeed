use std::sync::Arc;
use tokio::sync::broadcast;
use crate::shared_state::{SharedState, Event};
use crate::oms::OrderManagementSystem;

pub struct MarketMaker {
    shared_state: Arc<SharedState>,
    oms: Arc<OrderManagementSystem>,
    gamma: f64,
    event_rx: broadcast::Receiver<Event>,
}

impl MarketMaker {
    pub fn new(shared_state: Arc<SharedState>, oms: Arc<OrderManagementSystem>, gamma: f64) -> Self {
        let event_rx = shared_state.event_sender.subscribe();
        MarketMaker {
            shared_state,
            oms,
            gamma,
            event_rx,
        }
    }

    pub async fn run(&mut self) {
        while let Ok(event) = self.event_rx.recv().await {
            match event {
                Event::OrderBookUpdate(_order_book) => {
                    // Process order book update
                    println!("Received order book update");
                }
                Event::PortfolioUpdate(_portfolio) => {
                    // Process portfolio update
                    println!("Received portfolio update");
                }
                Event::VolatilityUpdate(volatility) => {
                    // Process volatility update
                    println!("Received volatility update: {}", volatility);
                }
            }
            
            if let Some((bid, ask)) = self.calculate_bid_ask_prices().await {
                println!("Calculated new prices - Bid: {}, Ask: {}", bid, ask);
                // Send order to OMS
                // self.oms.place_order(bid, ask).await;
            }
        }
    }

    pub async fn calculate_bid_ask_prices(&self) -> Option<(f64, f64)> {
        let order_book = self.shared_state.get_order_book().await;
        let mid_price = order_book.get_mid_price()?;
        let spread = self.gamma * mid_price;
        
        let bid = mid_price - spread / 2.0;
        let ask = mid_price + spread / 2.0;
        
        Some((bid, ask))
    }
}