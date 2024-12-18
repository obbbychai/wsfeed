use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use url::Url;
use tokio::sync::{mpsc, RwLock};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use crate::oms::OrderManagementSystem;
use crate::auth::{authenticate_with_signature, DeribitConfig, AuthResponse, refresh_token};
use tokio::time::{interval, Duration};

#[derive(Debug, Clone)]
pub enum OrderType {
    Market,
    Limit,
    CancelAll,
    CancelOrder,
    EditOrder,
}

#[derive(Debug, Clone)]
pub struct OrderMessage {
    pub instrument_name: String,
    pub order_type: OrderType,
    pub amount: Option<f64>,
    pub price: Option<f64>,
    pub is_buy: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String,
    pub instrument_name: String,
    pub amount: f64,
    pub price: f64,
    pub direction: String,
    pub timestamp: u64,
}

pub struct OrderHandler {
    ws_stream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    config: DeribitConfig,
    order_receiver: mpsc::Receiver<OrderMessage>,
    oms: Arc<RwLock<OrderManagementSystem>>,
}

impl OrderHandler {
    pub async fn new(
        config: DeribitConfig,
        order_receiver: mpsc::Receiver<OrderMessage>,
        oms: Arc<RwLock<OrderManagementSystem>>,
    ) -> Result<Self> {
        let url = Url::parse(&config.url)?;
        let (ws_stream, _) = connect_async(url).await?;
        
        Ok(OrderHandler {
            ws_stream,
            config,
            order_receiver,
            oms,
        })
    }

    async fn authenticate(&mut self) -> Result<AuthResponse> {
        authenticate_with_signature(
            &mut self.ws_stream,
            &self.config.client_id,
            &self.config.client_secret
        ).await
    }

    async fn refresh_auth_token(&mut self) -> Result<AuthResponse> {
        println!("OrderHandler: Refreshing authentication token");
        refresh_token(&mut self.ws_stream, self.config.refresh_token.as_deref()).await
    }

    pub async fn start_listening(&mut self) -> Result<()> {
        println!("OrderHandler: Starting order handler");
        
        loop {
            match self.listen_internal().await {
                Ok(_) => break,
                Err(e) => {
                    eprintln!("OrderHandler: Error processing orders: {}. Reconnecting...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let url = Url::parse(&self.config.url)?;
                    if let Ok((new_ws_stream, _)) = connect_async(url).await {
                        self.ws_stream = new_ws_stream;
                        if let Err(auth_err) = self.authenticate().await {
                            eprintln!("OrderHandler: Authentication failed during reconnection: {}", auth_err);
                            continue;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn listen_internal(&mut self) -> Result<()> {
        // Initial authentication
        let auth_response = self.authenticate().await?;
        
        // Set up refresh channel
        let (refresh_sender, mut refresh_receiver) = mpsc::channel::<()>(1);
        let refresh_in = Duration::from_secs((auth_response.expires_in as f64 * 0.8) as u64);
        
        // Schedule first token refresh
        self.schedule_token_refresh(refresh_in, refresh_sender.clone());
        
        // Set up heartbeat interval
        let mut interval = interval(Duration::from_secs(15));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Send heartbeat
                    let test_message = json!({
                        "jsonrpc": "2.0",
                        "id": 8212,
                        "method": "public/test",
                        "params": {}
                    });
                    self.send_ws_message(test_message).await?;
                }

                Some(()) = refresh_receiver.recv() => {
                    match self.refresh_auth_token().await {
                        Ok(new_auth) => {
                            let next_refresh = Duration::from_secs((new_auth.expires_in as f64 * 0.8) as u64);
                            self.schedule_token_refresh(next_refresh, refresh_sender.clone());
                            println!("OrderHandler: Successfully refreshed authentication token");
                        }
                        Err(e) => {
                            eprintln!("OrderHandler: Failed to refresh token: {}", e);
                            return Err(anyhow!("Failed to refresh token: {}", e));
                        }
                    }
                }

                Some(order_message) = self.order_receiver.recv() => {
                    if let Err(e) = self.process_order_message(order_message).await {
                        eprintln!("OrderHandler: Error processing order message: {}", e);
                    }
                }

                Some(msg) = self.ws_stream.next() => {
                    match msg {
                        Ok(Message::Ping(data)) => {
                            self.ws_stream.send(Message::Pong(data)).await?;
                        }
                        Ok(Message::Text(_)) => {
                            // Ignore text messages - OMS handles subscriptions
                        }
                        Ok(Message::Close(frame)) => {
                            println!("OrderHandler: Received close frame: {:?}", frame);
                            return Ok(());
                        }
                        Err(e) => {
                            return Err(anyhow!("WebSocket error: {}", e));
                        }
                        _ => {}
                    }
                }
                else => {
                    return Ok(());
                }
            }
        }
    }

    fn schedule_token_refresh(&self, duration: Duration, refresh_sender: mpsc::Sender<()>) {
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if let Err(e) = refresh_sender.send(()).await {
                eprintln!("OrderHandler: Failed to send refresh signal: {}", e);
            }
        });
    }

    async fn process_order_message(&mut self, order_message: OrderMessage) -> Result<()> {
        match order_message.order_type {
            OrderType::Market => {
                if let (Some(is_buy), Some(amount)) = (order_message.is_buy, order_message.amount) {
                    self.create_market_order(&order_message.instrument_name, is_buy, amount).await?;
                } else {
                    return Err(anyhow!("Market order requires is_buy and amount"));
                }
            }
            OrderType::Limit => {
                if let (Some(is_buy), Some(amount), Some(price)) = (
                    order_message.is_buy, 
                    order_message.amount, 
                    order_message.price
                ) {
                    self.create_limit_order(
                        &order_message.instrument_name, 
                        is_buy, 
                        amount, 
                        price
                    ).await?;
                } else {
                    return Err(anyhow!("Limit order requires is_buy, amount, and price"));
                }
            }
            OrderType::CancelAll => {
                self.cancel_all_orders().await?;
            }
            _ => return Err(anyhow!("Order type not implemented")),
        }
        Ok(())
    }

    async fn create_market_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64) -> Result<()> {
        let method = if is_buy { "private/buy" } else { "private/sell" };
        let order_message = json!({
            "jsonrpc": "2.0",
            "id": 5275,
            "method": method,
            "params": {
                "instrument_name": instrument_name,
                "amount": amount,
                "type": "market"
            }
        });

        self.send_ws_message(order_message).await
    }

    async fn create_limit_order(&mut self, instrument_name: &str, is_buy: bool, amount: f64, price: f64) -> Result<()> {
        let method = if is_buy { "private/buy" } else { "private/sell" };
        let adjusted_price = self.adjust_to_tick_size(price);
        let order_message = json!({
            "jsonrpc": "2.0",
            "id": 5276,
            "method": method,
            "params": {
                "instrument_name": instrument_name,
                "amount": amount,
                "type": "limit",
                "price": adjusted_price,
                "post_only": true
            }
        });

        self.send_ws_message(order_message).await
    }

    async fn cancel_all_orders(&mut self) -> Result<()> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "private/cancel_all",
            "params": {}
        });
        self.send_ws_message(request).await
    }

    fn adjust_to_tick_size(&self, price: f64) -> f64 {
        let tick_size = 2.5;
        (price / tick_size).round() * tick_size
    }

    async fn send_ws_message(&mut self, message: Value) -> Result<()> {
        println!("OrderHandler: Sending order: {:?}", message);
        self.ws_stream.send(Message::Text(message.to_string()))
            .await
            .map_err(|e| anyhow!("Failed to send order: {}", e))
    }
}