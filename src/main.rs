mod order_book;
mod auth;
mod instrument_names;
mod orderhandler;
mod oms;
mod portfolio;
use oms::OrderManagementSystem;
use portfolio::PortfolioManager;

use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};
use url::Url;
use serde_json::json;
use serde::Deserialize;
use std::fs;
use std::env;
use order_book::OrderBook;
use auth::{DeribitConfig, authenticate_with_signature};
use instrument_names::get_instrument_names;
use orderhandler::{OrderHandler, create_default_market_order};

#[derive(Debug, Deserialize)]
struct Config {
    auth: AuthConfig,
}

#[derive(Debug, Deserialize)]
struct AuthConfig {
    dbit: DeribitConfig,
}

async fn place_orders(handler: &mut OrderHandler, instrument_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Place a market buy order
    match handler.create_market_order(instrument_name, true, 10.0).await {
        Ok(order_id) => println!("Market buy order placed successfully with ID: {}", order_id),
        Err(e) => println!("Error placing market buy order: {}", e),
    }

    // Calculate the limit sell price (1% above the current mid price)
    let mid_price = 54338.75; // You should get this dynamically from the order book
    let limit_sell_price = mid_price * 1.01;

    // Place a limit sell order
    match handler.create_limit_order(instrument_name, false, 10.0, limit_sell_price).await {
        Ok(order_id) => println!("Limit sell order placed successfully with ID: {}", order_id),
        Err(e) => println!("Error placing limit sell order: {}", e),
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Current working directory: {:?}", env::current_dir()?);

    let config: Config = serde_yaml::from_str(&fs::read_to_string("config.yaml").expect("Unable to read config file"))
        .expect("Unable to parse config file");

    println!("Fetching instrument names");
    let instrument_names = match get_instrument_names("BTC", "future").await {
        Ok(names) => names,
        Err(e) => {
            eprintln!("Error fetching instrument names: {}", e);
            if let Some(source) = e.source() {
                eprintln!("Error source: {}", source);
                if let Some(source2) = source.source() {
                    eprintln!("Underlying error: {}", source2);
                }
            }
            return Err(e);
        }
    };

    println!("Available BTC futures instruments:");
    for name in &instrument_names {
        println!("  {}", name);
    }

    println!("Connecting to URL: {}", config.auth.dbit.url);

    let url = Url::parse(&config.auth.dbit.url)?;

    let (ws_stream, response) = connect_async(url).await?;
    println!("Connected with status: {}", response.status());
    
    let (mut write, mut read) = ws_stream.split();

    println!("Connected to Deribit WebSocket");

    if let Err(e) = authenticate_with_signature(&mut write, &config.auth.dbit.client_id, &config.auth.dbit.client_secret).await {
        println!("Authentication failed: {}", e);
        return Err(e.into());
    }

    if let Some(message) = read.next().await {
        match message {
            Ok(msg) => println!("Authentication response: {}", msg),
            Err(e) => {
                println!("Error receiving auth message: {}", e);
                return Err(e.into());
            }
        }
    }

    // Use the first instrument for this example
    let instrument_name = instrument_names.first().expect("No instruments found");
    let mut order_book = OrderBook::new(instrument_name.clone());

    let subscribe_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "public/subscribe",
        "params": {
            "channels": [format!("book.{}.100ms", instrument_name)]
        }
    });

    write.send(Message::Text(subscribe_message.to_string())).await?;
    println!("Sent subscribe message: {}", subscribe_message);

    let oms = OrderManagementSystem::new();
    let mut order_handler = OrderHandler::new(config.auth.dbit.clone(), oms.clone()).await?;

    // Place a default market order
    match create_default_market_order(&mut order_handler, instrument_name, true).await {
        Ok(order_id) => println!("Default market buy order placed successfully with ID: {}", order_id),
        Err(e) => println!("Error placing default market buy order: {}", e),
    }

    let portfolio_config = DeribitConfig {
        url: config.auth.dbit.url.clone(),
        client_id: config.auth.dbit.client_id.clone(),
        client_secret: config.auth.dbit.client_secret.clone(),
    };
    let portfolio_manager = PortfolioManager::new(portfolio_config).await?;

    // Start listening for portfolio updates in a separate task
    tokio::spawn(async move {
        if let Err(e) = portfolio_manager.start_listening().await {
            eprintln!("Error in portfolio manager: {}", e);
        }
    });

    while let Some(message) = read.next().await {
        match message {
            Ok(msg) => {
                if let Message::Text(text) = msg {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if json["method"] == "subscription" && json["params"]["channel"] == format!("book.{}.100ms", instrument_name) {
                            match order_book.update(&json["params"]["data"].to_string()) {
                                Ok(_) => {
                                    println!("Order book updated successfully");
                                    if let Some(mid_price) = order_book.get_mid_price() {
                                        println!("Current mid price: {}", mid_price);
                                        
                                        // Place orders based on the current mid price
                                        if let Err(e) = place_orders(&mut order_handler, instrument_name, mid_price).await {
                                            println!("Error placing orders: {}", e);
                                        }
                                    }
                                    order_book.print_order_book();
                                }
                                Err(e) => println!("Error updating order book: {}", e),
                            }
                        }
                    }
                }
            },
            Err(e) => println!("Error receiving message: {}", e),
        }
    }

    Ok(())
}