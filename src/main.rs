use std::fs;
use std::sync::Arc;
use serde::Deserialize;
use tokio::sync::mpsc;
use crate::auth::DeribitConfig;
use crate::portfolio::PortfolioManager;
use crate::order_book::{OrderBookManager, OrderBook};
use crate::volatility::VolatilityManager;
use crate::marketmaker::MarketMaker;
use crate::orderhandler::{OrderHandler, OrderMessage};
use crate::oms::{OrderManagementSystem, OMSUpdate};
use tokio::sync::RwLock;

mod auth;
mod instrument_names;
mod order_book;
mod orderhandler;
mod portfolio;
mod volatility;
mod marketmaker;
mod oms;

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

    let (portfolio_sender, portfolio_receiver) = mpsc::channel(10);
    let (order_book_sender, order_book_receiver) = mpsc::channel::<Arc<OrderBook>>(10);
    let (volatility_sender, volatility_receiver) = mpsc::channel(10);
    let (order_sender, order_receiver) = mpsc::channel::<OrderMessage>(100);
    let (oms_update_sender, oms_update_receiver) = mpsc::channel::<OMSUpdate>(100);

    // Create OMS with config
    let oms = Arc::new(RwLock::new(OrderManagementSystem::new(
        oms_update_sender,
        order_sender.clone(),
        config.auth.dbit.clone(), // Pass the config here
    ).await?));

    let portfolio_manager = PortfolioManager::new(config.auth.dbit.clone(), portfolio_sender).await?;
    let order_book_manager = OrderBookManager::new(config.auth.dbit.clone(), order_book_sender, "BTC-20DEC24".to_string()).await?;
    let volatility_manager = VolatilityManager::new(config.auth.dbit.clone(), volatility_sender).await?;
    let mut order_handler = OrderHandler::new(config.auth.dbit.clone(), order_receiver, oms.clone()).await?;

    let mut market_maker = MarketMaker::new(
        portfolio_receiver,
        order_book_receiver,
        volatility_receiver,
        order_sender,
        oms.clone(),
        oms_update_receiver,
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
        if let Err(e) = order_handler.start_listening().await {
            eprintln!("Error in order handler: {}", e);
        }
    });

    market_maker.run().await?;

    Ok(())
}