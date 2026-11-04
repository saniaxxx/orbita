//! mDNS discovery of Yandex Station devices.
//!
//! Stations advertise `_yandexio._tcp.local.` with TXT records `deviceId` and
//! `platform`. The local Glagol WebSocket listens on the advertised port and
//! uses a self-signed certificate.

use std::{
    collections::BTreeMap,
    net::IpAddr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;

pub const SERVICE_TYPE: &str = "_yandexio._tcp.local.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Station {
    pub device_id: String,
    pub platform: String,
    pub host: IpAddr,
    pub port: u16,
    pub name: Option<String>,
}

impl Station {
    fn from_service(info: &ServiceInfo) -> Option<Self> {
        let properties = info.get_properties();
        let device_id = properties.get_property_val_str("deviceId")?.to_owned();
        let platform = properties
            .get_property_val_str("platform")
            .unwrap_or("unknown")
            .to_owned();

        // A station with several addresses is usually a multi-interface bug;
        // prefer IPv4, which is what the local API is normally reached over.
        let host = info
            .get_addresses()
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| info.get_addresses().iter().next())
            .copied()?;

        Some(Self {
            device_id,
            platform,
            host,
            port: info.get_port(),
            name: Some(info.get_fullname().to_owned()),
        })
    }
}

/// Browse for stations until `timeout` expires and return the unique ones.
pub fn discover(timeout: Duration) -> Result<Vec<Station>> {
    let mdns = ServiceDaemon::new().context("starting mDNS daemon")?;
    let receiver = mdns
        .browse(SERVICE_TYPE)
        .context("browsing for Yandex Station")?;

    let deadline = Instant::now() + timeout;
    let mut found = BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if let Some(station) = Station::from_service(&info) {
                    found.insert(station.device_id.clone(), station);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = mdns.shutdown();
    Ok(found.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_serializes_with_snake_case_fields() {
        let station = Station {
            device_id: "duid-1".to_owned(),
            platform: "yandexmicro".to_owned(),
            host: "192.168.250.115".parse().expect("ip"),
            port: 1961,
            name: None,
        };
        let value = serde_json::to_value(station).expect("json");
        assert_eq!(value["device_id"], "duid-1");
        assert_eq!(value["port"], 1961);
    }
}
