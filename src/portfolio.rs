use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error as StdError;
use url::Url;
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};

use crate::auth::{authenticate_with_signature, DeribitConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioData {
    pub maintenance_margin: f64,
    pub delta_total: f64,
    pub options_session_rpl: f64,
    pub futures_session_rpl: f64,
    pub delta_total_map: std::collections::HashMap<String, f64>,
    pub session_upl: f64,
    pub fee_balance: f64,
    pub estimated_liquidation_ratio: Option<f64>,
    pub initial_margin: f64,
    pub options_gamma_map: std::collections::HashMap<String, f64>,
    pub futures_pl: f64,
    pub currency: String,
    pub options_value: f64,
    pub projected_maintenance_margin: f64,
    pub options_vega: f64,
    pub session_rpl: f64,
    pub futures_session_upl: f64,
    pub options_session_upl: f64,
    pub cross_collateral_enabled: bool,
    pub options_theta: f64,
    pub margin_model: String,
    pub options_delta: f64,
    pub options_pl: f64,
    pub options_vega_map: std::collections::HashMap<String, f64>,
    pub balance: f64,
    pub additional_reserve: f64,
    pub estimated_liquidation_ratio_map: Option<std::collections::HashMap<String, f64>>,
    pub projected_initial_margin: f64,
    pub available_funds: f64,
    pub spot_reserve: f64,
    pub projected_delta_total: f64,
    pub portfolio_margining_enabled: bool,
    pub total_pl: f64,
    pub margin_balance: f64,
    pub options_theta_map: std::collections::HashMap<String, f64>,
    pub available_withdrawal_funds: f64,
    pub equity: f64,
    pub options_gamma: f64,
}

pub struct PortfolioManager {
    ws_stream: Arc<Mutex<Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>>>,
    config: DeribitConfig,
    portfolio_data: Arc<RwLock<Option<PortfolioData>>>,
}

impl PortfolioManager {
    pub async fn new(config: DeribitConfig) -> Result<Self, Box<dyn StdError>> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        Ok(PortfolioManager {
            ws_stream: Arc::new(Mutex::new(Some(ws_stream))),
            config,
            portfolio_data: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn start_listening(self: Arc<Self>) -> Result<(), Box<dyn StdError>> {
        let mut ws_stream_guard = self.ws_stream.lock().await;
        if let Some(mut ws_stream) = ws_stream_guard.take() {
            authenticate_with_signature(&mut ws_stream, &self.config.client_id, &self.config.client_secret).await?;

            let subscribe_message = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "private/subscribe",
                "params": {
                    "channels": ["user.portfolio.btc"]
                }
            });

            ws_stream.send(Message::Text(subscribe_message.to_string())).await?;

            while let Some(msg) = ws_stream.next().await {
                let msg = msg?;
                if let Message::Text(text) = msg {
                    let value: Value = serde_json::from_str(&text)?;
                    if let Some(params) = value.get("params") {
                        if let Some(data) = params.get("data") {
                            let portfolio_data: PortfolioData = serde_json::from_value(data.clone())?;
                            let mut write_lock = self.portfolio_data.write().await;
                            *write_lock = Some(portfolio_data.clone());
                            drop(write_lock);
                            println!("Updated portfolio data: {:?}", portfolio_data);
                        }
                    }
                }
            }
        } else {
            return Err("WebSocket stream is not available".into());
        }
        Ok(())
    }

    pub async fn get_portfolio_data(&self) -> Option<PortfolioData> {
        self.portfolio_data.read().await.clone()
    }

    pub async fn update(&self, data: Value) -> Result<(), Box<dyn StdError>> {
        println!("Raw portfolio data: {}", serde_json::to_string_pretty(&data)?);
        
        let portfolio_data: PortfolioData = match serde_json::from_value(data.clone()) {
            Ok(data) => data,
            Err(e) => {
                println!("Error parsing portfolio data: {:?}", e);
                println!("Problematic JSON: {}", serde_json::to_string_pretty(&data)?);
                return Err(Box::new(e));
            }
        };

        let mut write_lock = self.portfolio_data.write().await;
        *write_lock = Some(portfolio_data);
        Ok(())
    }

    pub async fn get_available_funds(&self) -> Option<f64> {
        self.portfolio_data.read().await.as_ref().map(|data| data.available_funds)
    }
}