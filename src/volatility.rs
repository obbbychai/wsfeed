use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use serde_json::Value;
use anyhow::{Result, Context, anyhow};
use crate::auth::{authenticate_with_signature, DeribitConfig};

pub struct VolatilityManager {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    sender: mpsc::Sender<String>,
}

impl VolatilityManager {
    pub async fn new(config: DeribitConfig, sender: mpsc::Sender<String>) -> Result<Self> {
        let url = Url::parse(&config.url).context("Failed to parse URL")?;
        let (ws_stream, _) = connect_async(url).await.context("Failed to connect to WebSocket")?;
        
        let mut manager = VolatilityManager { ws_stream, config, sender };
        manager.authenticate().await.context("Failed to authenticate")?;
        manager.subscribe_to_volatility().await.context("Failed to subscribe to volatility")?;
        
        Ok(manager)
    }

    async fn authenticate(&mut self) -> Result<()> {
        authenticate_with_signature(&mut self.ws_stream, &self.config.client_id, &self.config.client_secret).await
    }

    async fn subscribe_to_volatility(&mut self) -> Result<()> {
        let subscribe_message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "public/subscribe",
            "params": {
                "channels": ["deribit_volatility_index.btc_usd"]
            }
        });

        self.ws_stream.send(Message::Text(subscribe_message.to_string()))
            .await
            .context("Failed to send subscription message")?;
        Ok(())
    }

    pub async fn start_listening(mut self) -> Result<()> {
        while let Some(msg) = self.ws_stream.next().await {
            let msg = msg.context("Failed to receive WebSocket message")?;
            if let Message::Text(text) = msg {
                let value: Value = serde_json::from_str(&text)
                    .context("Failed to parse WebSocket message as JSON")?;
                if let Some(params) = value.get("params") {
                    if let Some(data) = params.get("data") {
                        self.sender.send(data.to_string())
                            .await
                            .context("Failed to send volatility data")?;
                    }
                }
            }
        }
        Ok(())
    }
}