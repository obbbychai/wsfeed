use tokio::sync::broadcast;
use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;

#[derive(Clone, Debug)]
pub enum Event {
    OrderBookUpdate(OrderBook),
    PortfolioUpdate(PortfolioData),
    VolatilityUpdate(f64),
}

pub struct EventBucket {
    sender: broadcast::Sender<Event>,
}

impl EventBucket {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        EventBucket { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    pub fn send(&self, event: Event) -> Result<usize, broadcast::error::SendError<Event>> {
        println!("EventBucket: Sending event: {:?}", event);
        let result = self.sender.send(event);
        match &result {
            Ok(receivers) => println!("EventBucket: Event sent to {} receivers", receivers),
            Err(e) => println!("EventBucket: Failed to send event: {:?}", e),
        }
        result
    }
}