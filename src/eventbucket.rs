use crate::order_book::OrderBook;
use crate::portfolio::PortfolioData;
use crate::instrument_names::Instrument;
use crate::oms::Order;





#[derive(Clone, Debug)]
pub enum Event {
    OrderBookUpdate(OrderBook),
    PortfolioUpdate(PortfolioData),
    VolatilityUpdate(f64),
    InstrumentUpdate(Instrument),
    OrderUpdate(Order),
}

pub struct EventBucket {
    sender: tokio::sync::broadcast::Sender<Event>,
}

impl EventBucket {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(capacity);
        EventBucket { sender }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    pub fn send(&self, event: Event) -> Result<usize, tokio::sync::broadcast::error::SendError<Event>> {
       // println!("EventBucket: Sending event: {:?}", event);
        let result = self.sender.send(event);
        result
    }
}