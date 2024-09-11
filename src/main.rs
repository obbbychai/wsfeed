use tokio;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};
use url::Url;
use serde_json::json;
use std::fs;
use std::sync::Arc;

mod order_book;
mod auth;
mod instrument_names;
mod orderhandler;
mod oms;
mod volatility;
mod portfolio;
mod shared_state;
mod marketmaker;
mod eventbucket;

use order_book::OrderBook;
use auth::{DeribitConfig, authenticate_with_signature};
use instrument_names::fetch_instruments;
use oms::OrderManagementSystem;
use portfolio::PortfolioManager;
use volatility::VolatilityManager;
use shared_state::SharedState;
use marketmaker::MarketMaker;
use eventbucket::EventBucket;

// Define a custom error type that is Send + Sync
#[derive(Debug)]
pub struct AppError(pub String);

impl std::error::Error for AppError {}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let config_str = fs::read_to_string("config.yaml").map_err(|e| AppError(e.to_string()))?;
    let config: serde_yaml::Value = serde_yaml::from_str(&config_str).map_err(|e| AppError(e.to_string()))?;

    let dbit_config = DeribitConfig {
        url: config["auth"]["dbit"]["url"].as_str().unwrap().to_string(),
        client_id: config["auth"]["dbit"]["client_id"].as_str().unwrap().to_string(),
        client_secret: config["auth"]["dbit"]["client_secret"].as_str().unwrap().to_string(),
    };

    let instruments = fetch_instruments("BTC", "future").await.map_err(|e| AppError(e.to_string()))?;
    let instrument_name = instruments.first().expect("No instruments found").instrument_name.clone();

    let event_bucket = Arc::new(EventBucket::new(100));
    let shared_state = Arc::new(SharedState::new(Arc::clone(&event_bucket)));

    let order_book = Arc::new(tokio::sync::RwLock::new(OrderBook::new(instrument_name.clone())));
    

    let oms = Arc::new(OrderManagementSystem::new());
    let portfolio_manager = Arc::new(PortfolioManager::new(dbit_config.clone(), Arc::clone(&event_bucket)).await.map_err(|e| AppError(e.to_string()))?);
    let volatility_manager = Arc::new(VolatilityManager::new(dbit_config.clone(), 100, Arc::clone(&event_bucket)).await.map_err(|e| AppError(e.to_string()))?);

    let market_maker = Arc::new(MarketMaker::new(
        Arc::clone(&shared_state),
        Arc::clone(&oms),
        config["parameters"]["gamma"].as_f64().unwrap(),
        config["parameters"]["depth_percentage"].as_f64().unwrap(),
        Arc::clone(&event_bucket)
    ));

    let url = Url::parse(&dbit_config.url).map_err(|e| AppError(e.to_string()))?;
    let (ws_stream, _) = connect_async(url).await.map_err(|e| AppError(e.to_string()))?;
    let (mut write, mut read) = ws_stream.split();

    authenticate_with_signature(&mut write, &dbit_config.client_id, &dbit_config.client_secret).await.map_err(|e| AppError(e.to_string()))?;

    let subscribe_message = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "public/subscribe",
        "params": {
            "channels": [format!("book.{}.100ms", instrument_name)]
        }
    });
    write.send(Message::Text(subscribe_message.to_string())).await.map_err(|e| AppError(e.to_string()))?;

    // Start background tasks
    tokio::spawn({
        let pm = Arc::clone(&portfolio_manager);
        async move {
            if let Err(e) = pm.start_listening().await {
                eprintln!("Portfolio manager error: {}", e);
            }
        }
    });

    tokio::spawn({
        let vm = Arc::clone(&volatility_manager);
        async move {
            if let Err(e) = vm.connect_and_subscribe().await {
                eprintln!("Volatility manager error: {}", e);
            }
        }
    });

    // WebSocket listener task
    tokio::spawn({
        let _ss = Arc::clone(&shared_state);
        let ob = Arc::clone(&order_book);
        let eb = Arc::clone(&event_bucket);
        async move {
            while let Some(message) = read.next().await {
                let message = message.map_err(|e| AppError(e.to_string()))?;
                if let Message::Text(text) = message {
                    println!("Received WebSocket message: {}", text);
                    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| AppError(e.to_string()))?;
                    if let Some(params) = json.get("params") {
                        if let Some(data) = params.get("data") {
                            if let Some(change_id) = data.get("change_id") {
                                println!("Updating order book with change_id: {}", change_id);
                                let mut order_book = ob.write().await;
                                match order_book.update(&text) {
                                    Ok(_) => {
                                        println!("Order book updated successfully");
                                        order_book.print_order_book();
                                        // Print best bid and ask
                                        if let Some((bid_price, bid_amount)) = order_book.get_best_bid() {
                                            println!("Best Bid: Price = {}, Amount = {}", bid_price, bid_amount);
                                        }
                                        if let Some((ask_price, ask_amount)) = order_book.get_best_ask() {
                                            println!("Best Ask: Price = {}, Amount = {}", ask_price, ask_amount);
                                        }
                                        let order_book_clone = order_book.clone();
                                        drop(order_book);
                                        if let Err(e) = eb.send(eventbucket::Event::OrderBookUpdate(order_book_clone)) {
                                            eprintln!("Failed to send OrderBookUpdate event: {}", e);
                                        }
                                    },
                                    Err(e) => println!("Failed to update order book: {}", e),
                                }
                            } else {
                                println!("Received data without change_id: {:?}", data);
                            }
                        } else {
                            println!("Received params without data: {:?}", params);
                        }
                    } else {
                        println!("Received message without params: {:?}", json);
                    }
                } else {
                    println!("Received non-text message: {:?}", message);
                }
            }
            Ok::<_, AppError>(())
        }
    });

    // Event processing loop
    let mut event_rx = event_bucket.subscribe();
    println!("Starting event processing loop");
    loop {
        tokio::select! {
            result = event_rx.recv() => {
                match result {
                    Ok(event) => {
                        println!("Received event: {:?}", event);
                        match event {
                            eventbucket::Event::OrderBookUpdate(order_book) => {
                                println!("Processing OrderBookUpdate event");
                                println!("Instrument: {}", order_book.get_instrument_name());
                                println!("Change ID: {}", order_book.get_change_id());
                                println!("Best Bid: {:?}", order_book.get_best_bid());
                                println!("Best Ask: {:?}", order_book.get_best_ask());
                                shared_state.update_order_book(order_book).await;
                            }
                            eventbucket::Event::PortfolioUpdate(portfolio_data) => {
                                println!("Processing PortfolioUpdate event");
                                shared_state.update_portfolio(portfolio_data).await;
                            }
                            eventbucket::Event::VolatilityUpdate(volatility) => {
                                println!("Processing VolatilityUpdate event");
                                shared_state.update_volatility(volatility).await;
                            }
                        }
                    }
                    Err(e) => eprintln!("Error receiving event: {:?}", e),
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                if let Some((bid, ask)) = market_maker.calculate_bid_ask_prices().await {
                    println!("Market Maker: Bid: {}, Ask: {}", bid, ask);
                    // Here you would send the order to the OMS
                    // oms.place_order(bid, ask).await?;
                }
            }
        }
    }
}