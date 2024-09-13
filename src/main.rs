use tokio;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};
use url::Url;
use serde_json::json;
use std::fs;
use std::sync::Arc;
use tokio::sync::Mutex;

mod order_book;
mod auth;
mod instrument_names;
mod oms;
mod volatility;
mod portfolio;
mod marketmaker;
mod eventbucket;

use order_book::OrderBook;
use auth::{DeribitConfig, authenticate_with_signature};
use instrument_names::fetch_instruments;
use oms::OrderManagementSystem;
use portfolio::PortfolioManager;
use volatility::VolatilityManager;
use marketmaker::MarketMaker;
use eventbucket::{EventBucket, Event};

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_str = fs::read_to_string("config.yaml").map_err(|e| AppError(e.to_string()))?;
    let config: serde_yaml::Value = serde_yaml::from_str(&config_str).map_err(|e| AppError(e.to_string()))?;

    let dbit_config = DeribitConfig {
        url: config["auth"]["dbit"]["url"].as_str().unwrap().to_string(),
        client_id: config["auth"]["dbit"]["client_id"].as_str().unwrap().to_string(),
        client_secret: config["auth"]["dbit"]["client_secret"].as_str().unwrap().to_string(),
    };

    let instruments = fetch_instruments("BTC", "future").await.map_err(|e| AppError(e.to_string()))?;
    let instrument = instruments.first().expect("No instruments found").clone();
    let instrument_name = instrument.instrument_name.clone();

    let event_bucket = Arc::new(EventBucket::new(100));

    let order_book = Arc::new(tokio::sync::RwLock::new(OrderBook::new(instrument_name.clone())));
    
    let oms = Arc::new(OrderManagementSystem::new(dbit_config.clone(), Arc::clone(&event_bucket)).await);
    let portfolio_manager = Arc::new(PortfolioManager::new(dbit_config.clone(), Arc::clone(&event_bucket)).await.map_err(|e| AppError(e.to_string()))?);
    let volatility_manager = Arc::new(VolatilityManager::new(dbit_config.clone(), 100, Arc::clone(&event_bucket)).await.map_err(|e| AppError(e.to_string()))?);

    let market_maker = Arc::new(Mutex::new(MarketMaker::new(
        Arc::clone(&oms),
        config["parameters"]["gamma"].as_f64().unwrap(),
        config["parameters"]["depth_percentage"].as_f64().unwrap(),
        Arc::clone(&event_bucket),
        instrument
    )));

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
            println!("Starting PortfolioManager task");
            if let Err(e) = pm.start_listening().await {
                eprintln!("Portfolio manager error: {}", e);
            }
            println!("PortfolioManager task ended");
        }
    });

    tokio::spawn({
        let vm = Arc::clone(&volatility_manager);
        async move {
            println!("Starting VolatilityManager task");
            if let Err(e) = vm.connect_and_subscribe().await {
                eprintln!("Volatility manager error: {}", e);
            }
            println!("VolatilityManager task ended");
        }
    });

    tokio::spawn({
        let oms_clone: Arc<OrderManagementSystem> = Arc::clone(&oms);
        async move {
            println!("Starting OrderManagementSystem task");
            loop {
                if let Err(e) = oms_clone.clone().start_listening("BTC-20SEP24").await {
                    eprintln!("OrderManagementSystem error: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    });

    tokio::spawn({
        let mm = Arc::clone(&market_maker);
        async move {
            println!("Starting MarketMaker task");
            MarketMaker::run(mm).await;
            println!("MarketMaker task ended");
        }
    });

    // WebSocket listener task
    tokio::spawn({
        let ob = Arc::clone(&order_book);
        let eb = Arc::clone(&event_bucket);
        async move {
            while let Some(message) = read.next().await {
                let message = message.map_err(|e| AppError(e.to_string()))?;
                if let Message::Text(text) = message {
                  //  println!("Received WebSocket message: {}", text);
                    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| AppError(e.to_string()))?;
                    if let Some(params) = json.get("params") {
                        if let Some(data) = params.get("data") {
                            if let Some(change_id) = data.get("change_id") {
                                let mut order_book = ob.write().await;
                                match order_book.update(&text) {
                                    Ok(_) => {
                                        let order_book_clone = order_book.clone();
                                        drop(order_book);
                                        if let Err(e) = eb.send(Event::OrderBookUpdate(order_book_clone)) {
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
        match event_rx.recv().await {
            Ok(event) => {
              //  println!("Received event: {:?}", event);
                match event {
                    Event::OrderBookUpdate(order_book) => {
                //        println!("Processing OrderBookUpdate event");
                    }
                    Event::PortfolioUpdate(_portfolio_data) => {
                  //      println!("Processing PortfolioUpdate event");
                    }
                    Event::VolatilityUpdate(volatility) => {
                    //    println!("Processing VolatilityUpdate event");
                      //  println!("New volatility: {}", volatility);
                    }
                    Event::InstrumentUpdate(_instrument) => {
                        //println!("Processing InstrumentUpdate event");
                    }
                    Event::OrderUpdate(order) => {
                        //println!("Processing OrderUpdate event");
                        //println!("Updated order: {:?}", order);
                    }
                }
            }
            Err(e) => eprintln!("Error receiving event: {:?}", e),
        }
    }
}