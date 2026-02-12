//! Connection tracking for SeedLink server.
//!
//! Maintains a thread-safe registry of active client connections
//! for INFO CONNECTIONS support.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use seedlink_rs_protocol::ProtocolVersion;

/// Per-connection metadata.
#[derive(Clone, Debug)]
pub(crate) struct ConnectionInfo {
    pub addr: SocketAddr,
    pub connected_at: SystemTime,
    pub protocol_version: ProtocolVersion,
    pub user_agent: Option<String>,
    pub state: String,
}

struct RegistryInner {
    next_id: AtomicU64,
    connections: Mutex<HashMap<u64, ConnectionInfo>>,
}

/// Thread-safe connection registry. Clone is cheap (Arc).
#[derive(Clone)]
pub(crate) struct ConnectionRegistry(Arc<RegistryInner>);

impl ConnectionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self(Arc::new(RegistryInner {
            next_id: AtomicU64::new(1),
            connections: Mutex::new(HashMap::new()),
        }))
    }

    /// Register a new connection. Returns a unique connection ID.
    pub fn register(&self, addr: SocketAddr) -> u64 {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let info = ConnectionInfo {
            addr,
            connected_at: SystemTime::now(),
            protocol_version: ProtocolVersion::V3,
            user_agent: None,
            state: "Connected".to_owned(),
        };
        self.0.connections.lock().unwrap().insert(id, info);
        id
    }

    /// Remove a connection from the registry.
    pub fn unregister(&self, id: u64) {
        self.0.connections.lock().unwrap().remove(&id);
    }

    /// Update connection metadata.
    pub fn update<F>(&self, id: u64, f: F)
    where
        F: FnOnce(&mut ConnectionInfo),
    {
        if let Some(info) = self.0.connections.lock().unwrap().get_mut(&id) {
            f(info);
        }
    }

    /// Take a snapshot of all active connections.
    pub fn snapshot(&self) -> Vec<ConnectionInfo> {
        self.0
            .connections
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Number of active connections.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.0.connections.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    #[test]
    fn register_and_unregister() {
        let reg = ConnectionRegistry::new();
        assert_eq!(reg.count(), 0);

        let id1 = reg.register(addr(1001));
        let id2 = reg.register(addr(1002));
        assert_eq!(reg.count(), 2);
        assert_ne!(id1, id2);

        reg.unregister(id1);
        assert_eq!(reg.count(), 1);

        reg.unregister(id2);
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn update_metadata() {
        let reg = ConnectionRegistry::new();
        let id = reg.register(addr(1001));

        reg.update(id, |info| {
            info.protocol_version = ProtocolVersion::V4;
            info.user_agent = Some("test-client/1.0".to_owned());
            info.state = "Streaming".to_owned();
        });

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].protocol_version, ProtocolVersion::V4);
        assert_eq!(snap[0].user_agent.as_deref(), Some("test-client/1.0"));
        assert_eq!(snap[0].state, "Streaming");
    }

    #[test]
    fn snapshot_returns_all() {
        let reg = ConnectionRegistry::new();
        reg.register(addr(1001));
        reg.register(addr(1002));
        reg.register(addr(1003));

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 3);
    }

    #[test]
    fn unregister_nonexistent_is_noop() {
        let reg = ConnectionRegistry::new();
        reg.unregister(999); // should not panic
        assert_eq!(reg.count(), 0);
    }
}
