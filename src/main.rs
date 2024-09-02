mod order_book;
mod auth;
mod instrument_names;

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

#[derive(Debug, Deserialize)]
struct Config {
    auth: AuthConfig,
}

#[derive(Debug, Deserialize)]
struct AuthConfig {
    dbit: DeribitConfig,
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