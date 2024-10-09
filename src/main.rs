use std::fs;
use serde::Deserialize;
use tokio::sync::mpsc;
use crate::auth::DeribitConfig;
use crate::portfolio::PortfolioManager;
use crate::order_book::{OrderBookManager, OrderBook};
use crate::volatility::VolatilityManager;
use crate::marketmaker::MarketMaker;
use crate::orderhandler::{OrderHandler, OrderMessage};
use crate::sharedstate::SharedState;
use std::sync::Arc;
use tokio::sync::RwLock;

mod auth;
mod instrument_names;
mod order_book;
mod orderhandler;
mod portfolio;
mod volatility;
mod marketmaker;
mod sharedstate;


#[derive(Deserialize)]
struct Config {
    auth: AuthConfig,
}

#[derive(Deserialize)]
struct AuthConfig {
    dbit: DeribitConfig,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config: Config = serde_yaml::from_str(&fs::read_to_string("config.yaml").expect("Unable to read config file"))
        .expect("Unable to parse config file");

    let (portfolio_sender, portfolio_receiver) = mpsc::channel(100);
    let (order_book_sender, order_book_receiver) = mpsc::channel::<Arc<OrderBook>>(100);
    let (volatility_sender, volatility_receiver) = mpsc::channel(100);
    let (order_sender, order_receiver) = mpsc::channel::<OrderMessage>(100);

    let portfolio_manager = PortfolioManager::new(config.auth.dbit.clone(), portfolio_sender).await?;
    let order_book_manager = OrderBookManager::new(config.auth.dbit.clone(), order_book_sender, "BTC-11OCT24".to_string()).await?;
    let volatility_manager = VolatilityManager::new(config.auth.dbit.clone(), volatility_sender).await?;

    let shared_state = Arc::new(RwLock::new(SharedState::new()));

    let mut order_handler = OrderHandler::new(config.auth.dbit.clone(), order_receiver, shared_state.clone()).await?;

    let mut market_maker = MarketMaker::new(
        portfolio_receiver,
        order_book_receiver,
        volatility_receiver,
        order_sender,
        shared_state.clone()
    ).await?;

    tokio::spawn(async move {
        if let Err(e) = portfolio_manager.start_listening().await {
            eprintln!("Error in portfolio manager: {}", e);
        }
    });

    tokio::spawn(async move {
        if let Err(e) = order_book_manager.start_listening().await {
            eprintln!("Error in order book manager: {}", e);
        }
    });

    tokio::spawn(async move {
        if let Err(e) = volatility_manager.start_listening().await {
            eprintln!("Error in volatility manager: {}", e);
        }
    });

    tokio::spawn(async move {
        if let Err(e) = order_handler.run().await {
            eprintln!("Error in order handler: {}", e);
        }
    });

    market_maker.run().await?;

    Ok(())
}