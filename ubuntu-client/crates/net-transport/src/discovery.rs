use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::time::Duration;

/// Discovered RESC host.
#[derive(Debug, Clone)]
pub struct DiscoveredHost {
    pub name: String,
    pub host: String,
    pub port: u16,
}

/// Discover RESC hosts via mDNS.
pub fn discover_host(timeout: Duration) -> Result<Option<DiscoveredHost>> {
    let mdns = ServiceDaemon::new()?;
    let receiver = mdns.browse(protocol::constants::MDNS_SERVICE_TYPE)?;

    log::info!("Searching for RESC host via mDNS ({:?} timeout)...", timeout);

    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        match receiver.recv_timeout(remaining.min(Duration::from_secs(1))) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if let Some(addr) = info.get_addresses().iter().next() {
                    let host = DiscoveredHost {
                        name: info.get_fullname().to_string(),
                        host: addr.to_string(),
                        port: info.get_port(),
                    };
                    log::info!("Found RESC host: {} at {}:{}", host.name, host.host, host.port);
                    let _ = mdns.shutdown();
                    return Ok(Some(host));
                }
            }
            Ok(_) => {} // other events, ignore
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(e) => {
                log::warn!("mDNS recv error: {}", e);
                break;
            }
        }
    }

    let _ = mdns.shutdown();
    Ok(None)
}
