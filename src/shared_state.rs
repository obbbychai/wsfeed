use tokio::sync::{RwLock, broadcast};
use std::sync::Arc;
use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;
use crate::instrument_names::Instrument;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Event {
    OrderBookUpdate(OrderBook),
    PortfolioUpdate(PortfolioData),
    VolatilityUpdate(f64),
}

pub struct SharedState {
    pub event_sender: broadcast::Sender<Event>,
    order_book: Arc<RwLock<OrderBook>>,
    portfolio: Arc<RwLock<Option<PortfolioData>>>,
    volatility: Arc<RwLock<Option<f64>>>,
    instruments: Arc<RwLock<HashMap<String, Instrument>>>,
}

impl SharedState {
    pub fn new(event_sender: broadcast::Sender<Event>) -> Self {
        SharedState {
            event_sender,
            order_book: Arc::new(RwLock::new(OrderBook::new("".to_string()))),
            portfolio: Arc::new(RwLock::new(None)),
            volatility: Arc::new(RwLock::new(None)),
            instruments: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn update_order_book(&self, new_order_book: OrderBook) {
        let mut order_book = self.order_book.write().await;
        *order_book = new_order_book.clone();
        let _ = self.event_sender.send(Event::OrderBookUpdate(new_order_book));
    }

    pub async fn update_portfolio(&self, new_portfolio: PortfolioData) {
        let mut portfolio = self.portfolio.write().await;
        *portfolio = Some(new_portfolio.clone());
        let _ = self.event_sender.send(Event::PortfolioUpdate(new_portfolio));
    }

    pub async fn update_volatility(&self, new_volatility: f64) {
        let mut volatility = self.volatility.write().await;
        *volatility = Some(new_volatility);
        let _ = self.event_sender.send(Event::VolatilityUpdate(new_volatility));
    }

    pub async fn update_instruments(&self, new_instruments: Vec<Instrument>) {
        let mut instruments = self.instruments.write().await;
        instruments.clear();
        for instrument in new_instruments {
            instruments.insert(instrument.instrument_name.clone(), instrument);
        }
    }

    pub async fn get_order_book(&self) -> OrderBook {
        self.order_book.read().await.clone()
    }

    pub async fn get_portfolio(&self) -> Option<PortfolioData> {
        self.portfolio.read().await.clone()
    }

    pub async fn get_volatility(&self) -> Option<f64> {
        *self.volatility.read().await
    }
}