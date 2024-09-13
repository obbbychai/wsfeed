use serde::{Deserialize, Serialize};
use reqwest::Client;
use std::fs;
use std::path::Path;
use crate::AppError;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Instrument {
    pub instrument_name: String,
    pub kind: String,
    pub instrument_type: String,
    pub quote_currency: String,
    pub min_trade_amount: f64,
    pub settlement_currency: String,
    pub expiration_timestamp: i64,
    pub is_active: bool,
    pub tick_size: f64,
    pub contract_size: f64,
    pub base_currency: String,
    pub instrument_id: i64,
    pub creation_timestamp: i64,
    pub taker_commission: f64,
    pub maker_commission: f64,
    pub settlement_period: String,
    pub counter_currency: String,
    pub price_index: String,
    pub rfq: bool,
    pub max_leverage: i64,
    pub max_liquidation_commission: f64,
    pub block_trade_commission: f64,
    pub block_trade_min_trade_amount: f64,
    pub block_trade_tick_size: f64,
    pub future_type: Option<String>,
    #[serde(default)]
    pub tick_size_steps: Vec<TickSizeStep>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TickSizeStep {
    pub above_price: f64,
    pub tick_size: f64,
}

#[derive(Debug, Deserialize, Serialize)]
struct InstrumentResponse {
    jsonrpc: String,
    result: Option<Vec<Instrument>>,
    error: Option<ErrorResponse>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ErrorResponse {
    code: i32,
    message: String,
}

const CACHE_FILE: &str = "instrument_cache.json";




pub async fn fetch_instruments(currency: &str, kind: &str) -> Result<Vec<Instrument>, AppError> {
    if let Ok(cached_instruments) = read_cached_instruments() {
        println!("Using cached instruments");
        return Ok(cached_instruments);
    }

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
        .await
        .map_err(|e| AppError(e.to_string()))?;

    let status = response.status();
    let body = response.text().await.map_err(|e| AppError(e.to_string()))?;
    println!("API Response Status: {}", status);
    println!("API Response Body: {}", body);

    let parsed_response: InstrumentResponse = serde_json::from_str(&body).map_err(|e| AppError(e.to_string()))?;

    if let Some(error) = parsed_response.error {
        return Err(AppError(format!("API Error: {} (Code: {})", error.message, error.code)));
    }

    let instruments = parsed_response.result.ok_or_else(|| AppError("No instruments found".into()))?;
    
    // Cache the instruments
    cache_instruments(&instruments)?;

    Ok(instruments)
}

fn cache_instruments(instruments: &[Instrument]) -> Result<(), AppError> {
    let json = serde_json::to_string_pretty(instruments).map_err(|e| AppError(e.to_string()))?;
    fs::write(CACHE_FILE, json).map_err(|e| AppError(e.to_string()))?;
    println!("Instruments cached to {}", CACHE_FILE);
    Ok(())
}

fn read_cached_instruments() -> Result<Vec<Instrument>, AppError> {
    if !Path::new(CACHE_FILE).exists() {
        return Err(AppError("Cache file does not exist".into()));
    }

    let json = fs::read_to_string(CACHE_FILE).map_err(|e| AppError(e.to_string()))?;
    let instruments: Vec<Instrument> = serde_json::from_str(&json).map_err(|e| AppError(e.to_string()))?;
    Ok(instruments)
}

pub fn load_cached_instruments() -> Result<Vec<Instrument>, AppError> {
    if !Path::new(CACHE_FILE).exists() {
        return Err(AppError("Cache file does not exist".into()));
    }

    let json = fs::read_to_string(CACHE_FILE).map_err(|e| AppError(e.to_string()))?;
    let instruments: Vec<Instrument> = serde_json::from_str(&json).map_err(|e| AppError(e.to_string()))?;
    Ok(instruments)
}