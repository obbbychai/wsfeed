use serde::{Deserialize, Serialize};
use reqwest::Client;
use std::fs;
use std::path::Path;
use std::error::Error as StdError;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct InstrumentResponse {
    pub jsonrpc: String,
    pub result: Vec<Instrument>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Instrument {
    pub tick_size: f64,
    pub tick_size_steps: Vec<f64>,
    pub taker_commission: f64,
    pub settlement_period: String,
    pub settlement_currency: String,
    pub rfq: bool,
    pub quote_currency: String,
    pub price_index: String,
    pub min_trade_amount: f64,
    pub max_liquidation_commission: f64,
    pub max_leverage: i64,
    pub maker_commission: f64,
    pub kind: String,
    pub is_active: bool,
    pub instrument_name: String,
    pub instrument_id: i64,
    pub instrument_type: String,
    pub expiration_timestamp: i64,
    pub creation_timestamp: i64,
    pub counter_currency: String,
    pub contract_size: f64,
    pub block_trade_tick_size: f64,
    pub block_trade_min_trade_amount: f64,
    pub block_trade_commission: f64,
    pub base_currency: String,
}

const CACHE_FILE: &str = "instrument_cache.json";

pub async fn fetch_instruments(currency: &str, kind: &str) -> Result<HashMap<String, Instrument>, Box<dyn StdError>> {
    let client = Client::new();
    let url = format!(
        "https://www.deribit.com/api/v2/public/get_instruments?currency={}&kind={}",
        currency, kind
    );

    println!("Sending request to URL: {}", url);

    let response = client
        .get(&url)
        .header("Content-Type", "application/json")
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    println!("API Response Status: {}", status);
    println!("API Response Body: {}", body);

    let parsed_response: InstrumentResponse = serde_json::from_str(&body)?;
    let instruments: HashMap<String, Instrument> = parsed_response.result.into_iter().map(|i| (i.instrument_name.clone(), i)).collect();
    
    // Cache the full instrument data
    cache_instruments(&instruments)?;

    Ok(instruments)
}

fn cache_instruments(instruments: &HashMap<String, Instrument>) -> Result<(), Box<dyn StdError>> {
    let json = serde_json::to_string_pretty(instruments)?;
    fs::write(CACHE_FILE, json)?;
    Ok(())
}

fn load_cached_instruments() -> Result<HashMap<String, Instrument>, Box<dyn StdError>> {
    let json = fs::read_to_string(CACHE_FILE)?;
    let instruments: HashMap<String, Instrument> = serde_json::from_str(&json)?;
    Ok(instruments)
}

pub async fn get_instruments(currency: &str, kind: &str) -> Result<HashMap<String, Instrument>, Box<dyn StdError>> {
    match fetch_instruments(currency, kind).await {
        Ok(instruments) => Ok(instruments),
        Err(e) => {
            println!("Error fetching instruments from API: {}. Trying to load from cache.", e);
            if Path::new(CACHE_FILE).exists() {
                load_cached_instruments()
            } else {
                Err("Failed to fetch instruments and no cache available".into())
            }
        }
    }
}