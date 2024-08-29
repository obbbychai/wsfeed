mod order_book;

use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{SinkExt, StreamExt};
use url::Url;
use serde_json::json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::env;
use order_book::OrderBook;

#[derive(Debug, Deserialize)]
struct Config {
    auth: AuthConfig,
}

#[derive(Debug, Deserialize)]
struct AuthConfig {
    dbit: DeribitConfig,
}

#[derive(Debug, Deserialize)]
struct DeribitConfig {
    client_id: String,
    client_secret: String,
    url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthResponse {
    jsonrpc: String,
    id: i64,
    result: AuthResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthResult {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    scope: String,
    token_type: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Current working directory: {:?}", env::current_dir()?);

    let config: Config = serde_yaml::from_str(&fs::read_to_string("config.yaml").expect("Unable to read config file"))
        .expect("Unable to parse config file");

    println!("Connecting to URL: {}", config.auth.dbit.url);

    let url = Url::parse(&config.auth.dbit.url)?;

    let (ws_stream, response) = connect_async(url).await?;
    println!("Connected with status: {}", response.status());
    
    let (mut write, mut read) = ws_stream.split();

    println!("Connected to Deribit WebSocket");

    let auth_message = json!({
        "jsonrpc": "2.0",
        "id": 9929,
        "method": "public/auth",
        "params": {
            "grant_type": "client_credentials",
            "client_id": config.auth.dbit.client_id,
            "client_secret": config.auth.dbit.client_secret,
        }
    });

    write.send(Message::Text(auth_message.to_string())).await?;
    println!("Sent auth message: {}", auth_message);

    if let Some(message) = read.next().await {
        match message {
            Ok(msg) => {
                println!("Received message: {}", msg);
                if let Ok(auth_result) = serde_json::from_str::<AuthResponse>(&msg.to_string()) {
                    println!("Authenticated successfully. Access token: {}", auth_result.result.access_token);
                } else {
                    println!("Failed to parse authentication response");
                }
            },
            Err(e) => println!("Error receiving message: {}", e),
        }
    }

    let mut order_book = OrderBook::new("BTC-PERPETUAL".to_string());

    let subscribe_message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "public/subscribe",
        "params": {
            "channels": ["book.BTC-PERPETUAL.100ms"]
        }
    });

    write.send(Message::Text(subscribe_message.to_string())).await?;
    println!("Sent subscribe message: {}", subscribe_message);

    while let Some(message) = read.next().await {
        match message {
            Ok(msg) => {
                if let Message::Text(text) = msg {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if json["method"] == "subscription" && json["params"]["channel"] == "book.BTC-PERPETUAL.100ms" {
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