use serde::{Deserialize, Serialize};
use reqwest::Client;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::error::Error as StdError;

#[derive(Debug, Deserialize, Serialize)]
pub struct InstrumentResponse {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub result: Option<Vec<Instrument>>,
    pub error: Option<ErrorResponse>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ErrorResponse {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Instrument {
    pub instrument_name: String,
    // Add other fields as needed
}

const CACHE_FILE: &str = "instrument_cache.json";

pub async fn fetch_instruments(currency: &str, kind: &str) -> Result<Vec<Instrument>, Box<dyn StdError>> {
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

    if let Some(error) = parsed_response.error {
        return Err(format!("API Error: {} (Code: {})", error.message, error.code).into());
    }

    let instruments = parsed_response.result.ok_or_else(|| Box::<dyn StdError>::from("No result in API response"))?;
    
    // Cache the instruments
    cache_instruments(&instruments)?;

    Ok(instruments)
}

fn cache_instruments(instruments: &[Instrument]) -> Result<(), Box<dyn StdError>> {
    let json = serde_json::to_string_pretty(instruments)?;
    fs::write(CACHE_FILE, json)?;
    Ok(())
}

fn load_cached_instruments() -> Result<Vec<Instrument>, Box<dyn StdError>> {
    let json = fs::read_to_string(CACHE_FILE)?;
    let instruments: Vec<Instrument> = serde_json::from_str(&json)?;
    Ok(instruments)
}

pub async fn get_instrument_names(currency: &str, kind: &str) -> Result<Vec<String>, Box<dyn StdError>> {
    let instruments = match fetch_instruments(currency, kind).await {
        Ok(instruments) => instruments,
        Err(e) => {
            println!("Error fetching instruments from API: {}. Trying to load from cache.", e);
            if Path::new(CACHE_FILE).exists() {
                load_cached_instruments()?
            } else {
                return Err("Failed to fetch instruments and no cache available".into());
            }
        }
    };

    Ok(instruments.into_iter().map(|i| i.instrument_name).collect())
}