use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use serde::{Deserialize, Serialize};
use std::error::Error;

#[derive(Debug, Deserialize, Serialize)]
pub struct TickerData {
    pub e: String,  // Event type
    pub E: u64,     // Event time
    pub s: String,  // Symbol
    pub p: String,  // Price change
    pub P: String,  // Price change percent
    pub w: String,  // Weighted average price
    pub x: String,  // First trade(F)-1 price
    pub c: String,  // Last price
    pub Q: String,  // Last quantity
    pub b: String,  // Best bid price
    pub B: String,  // Best bid quantity
    pub a: String,  // Best ask price
    pub A: String,  // Best ask quantity
    pub o: String,  // Open price
    pub h: String,  // High price
    pub l: String,  // Low price
    pub v: String,  // Total traded base asset volume
    pub q: String,  // Total traded quote asset volume
    pub O: u64,     // Statistics open time
    pub C: u64,     // Statistics close time
    pub F: u64,     // First trade ID
    pub L: u64,     // Last trade Id
    pub n: u64,     // Total number of trades
}

pub struct BinanceFeed {
    url: String,
}

impl BinanceFeed {
    pub fn new(url: String) -> Self {
        BinanceFeed { url }
    }

    pub async fn connect_and_subscribe(&self, symbol: &str) -> Result<(), Box<dyn Error>> {
        let ws_url = format!("{}/ws/{}@ticker", self.url, symbol.to_lowercase());
        let url = Url::parse(&ws_url)?;

        let (ws_stream, _) = connect_async(url).await?;
        println!("WebSocket connected");

        let (mut write, mut read) = ws_stream.split();

        // You can add a subscription message here if needed
        // let subscribe_msg = serde_json::json!({
        //     "method": "SUBSCRIBE",
        //     "params": [format!("{}@ticker", symbol.to_lowercase())],
        //     "id": 1
        // });
        // write.send(Message::Text(subscribe_msg.to_string())).await?;

        println!("Subscribed to {} ticker", symbol);

        while let Some(message) = read.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<TickerData>(&text) {
                        Ok(ticker_data) => {
                            println!("Received ticker data for {}: {:?}", symbol, ticker_data);
                            // Here you can process the ticker data as needed
                            // For example, you could send it to a channel, update a shared state, etc.
                        }
                        Err(e) => println!("Error parsing ticker data: {}", e),
                    }
                }
                Ok(Message::Ping(_)) => {
                    write.send(Message::Pong(vec![])).await?;
                }
                Ok(_) => {}
                Err(e) => println!("Error receiving message: {}", e),
            }
        }

        Ok(())
    }
}