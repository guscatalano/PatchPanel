//! The set of agents currently holding a connection.
//!
//! This is the only piece of portal state that is deliberately not persisted:
//! "who is connected right now" is a property of the process, and rebuilding it
//! from scratch on restart is correct — agents reconnect within seconds.

use dashmap::DashMap;
use pp_proto::{AgentId, ServerMsg};
use tokio::sync::mpsc;

#[derive(Default)]
pub struct Hub {
    senders: DashMap<AgentId, mpsc::UnboundedSender<ServerMsg>>,
}

impl Hub {
    pub fn new() -> Self {
        Hub::default()
    }

    /// Register a connection. Returns the receiver the writer task drains and
    /// a handle to identify this connection at disconnect time.
    ///
    /// A second connection from the same agent replaces the first: the usual
    /// cause is a reconnect the portal has not yet noticed, and the newer
    /// socket is the one that works.
    pub fn connect(
        &self,
        id: AgentId,
    ) -> (
        mpsc::UnboundedSender<ServerMsg>,
        mpsc::UnboundedReceiver<ServerMsg>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        if self.senders.insert(id, tx.clone()).is_some() {
            tracing::info!(%id, "replaced an existing connection");
        }
        (tx, rx)
    }

    /// Drop a connection, but only if it is still the one we registered.
    pub fn disconnect(&self, id: AgentId, sender: &mpsc::UnboundedSender<ServerMsg>) {
        // Without this check, a slow teardown of an old socket would evict the
        // new one that just replaced it.
        self.senders
            .remove_if(&id, |_, current| current.same_channel(sender));
    }

    pub fn send(&self, id: AgentId, msg: ServerMsg) -> bool {
        match self.senders.get(&id) {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }

    /// Push to every connected agent. Returns how many accepted the message.
    pub fn broadcast(&self, msg: &ServerMsg) -> usize {
        self.senders
            .iter()
            .filter(|entry| entry.value().send(msg.clone()).is_ok())
            .count()
    }

    pub fn is_connected(&self, id: AgentId) -> bool {
        self.senders.contains_key(&id)
    }

    pub fn connected(&self) -> Vec<AgentId> {
        self.senders.iter().map(|e| *e.key()).collect()
    }
}
