use tokio::sync::RwLock;
use std::sync::Arc;
use std::collections::{HashMap, VecDeque};
use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;
use crate::instrument_names::Instrument;
use crate::eventbucket::EventBucket;

const BUFFER_SIZE: usize = 100;  // Adjust this value as needed

pub struct SharedState {
    event_bucket: Arc<EventBucket>,
    order_books: Arc<RwLock<VecDeque<OrderBook>>>,
    portfolios: Arc<RwLock<VecDeque<PortfolioData>>>,
    volatilities: Arc<RwLock<VecDeque<f64>>>,
    instruments: Arc<RwLock<HashMap<String, Instrument>>>,
}

impl SharedState {
    pub fn new(event_bucket: Arc<EventBucket>) -> Self {
        SharedState {
            event_bucket,
            order_books: Arc::new(RwLock::new(VecDeque::with_capacity(BUFFER_SIZE))),
            portfolios: Arc::new(RwLock::new(VecDeque::with_capacity(BUFFER_SIZE))),
            volatilities: Arc::new(RwLock::new(VecDeque::with_capacity(BUFFER_SIZE))),
            instruments: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn update_order_book(&self, new_order_book: OrderBook) {
        let mut order_books = self.order_books.write().await;
        if order_books.len() == BUFFER_SIZE {
            order_books.pop_front();
        }
        order_books.push_back(new_order_book.clone());
        drop(order_books);
        let _ = self.event_bucket.send(crate::eventbucket::Event::OrderBookUpdate(new_order_book));
    }

    pub async fn update_portfolio(&self, new_portfolio: PortfolioData) {
        let mut portfolios = self.portfolios.write().await;
        if portfolios.len() == BUFFER_SIZE {
            portfolios.pop_front();
        }
        portfolios.push_back(new_portfolio.clone());
        drop(portfolios);
        let _ = self.event_bucket.send(crate::eventbucket::Event::PortfolioUpdate(new_portfolio));
    }

    pub async fn update_volatility(&self, new_volatility: f64) {
        let mut volatilities = self.volatilities.write().await;
        if volatilities.len() == BUFFER_SIZE {
            volatilities.pop_front();
        }
        volatilities.push_back(new_volatility);
        drop(volatilities);
        let _ = self.event_bucket.send(crate::eventbucket::Event::VolatilityUpdate(new_volatility));
    }

    pub async fn update_instruments(&self, new_instruments: Vec<Instrument>) {
        let mut instruments = self.instruments.write().await;
        instruments.clear();
        for instrument in new_instruments {
            instruments.insert(instrument.instrument_name.clone(), instrument);
        }
    }

    pub async fn get_latest_order_book(&self) -> Option<OrderBook> {
        self.order_books.read().await.back().cloned()
    }

    pub async fn get_latest_portfolio(&self) -> Option<PortfolioData> {
        self.portfolios.read().await.back().cloned()
    }

    pub async fn get_latest_volatility(&self) -> Option<f64> {
        self.volatilities.read().await.back().cloned()
    }

    pub async fn get_instrument(&self, instrument_name: &str) -> Option<Instrument> {
        self.instruments.read().await.get(instrument_name).cloned()
    }

    // New methods to get historical data
    pub async fn get_order_book_history(&self) -> Vec<OrderBook> {
        self.order_books.read().await.iter().cloned().collect()
    }

    pub async fn get_portfolio_history(&self) -> Vec<PortfolioData> {
        self.portfolios.read().await.iter().cloned().collect()
    }

    pub async fn get_volatility_history(&self) -> Vec<f64> {
        self.volatilities.read().await.iter().cloned().collect()
    }
}