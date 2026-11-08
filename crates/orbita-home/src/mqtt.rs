//! MQTT transport for Wiren Board.
//!
//! Commands are always published with `retain = false`: a retained command is
//! replayed after a reconnect and can open a gate or toggle a light again.
//! State and `meta/error` topics are subscribed with QoS1.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use tracing::{error, info, warn};

use crate::home::{Clock, Home, Publisher};
use crate::model::HomeError;

#[derive(Debug, Default)]
pub struct MqttStatus {
    connected: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl MqttStatus {
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn connected_now(&self) {
        self.connected.store(true, Ordering::SeqCst);
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn note_error(&self, error: impl Into<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.into());
    }

    fn disconnected(&self, error: impl Into<String>) {
        self.connected.store(false, Ordering::SeqCst);
        self.note_error(error);
    }
}

pub struct MqttPublisher {
    client: AsyncClient,
    topics: Vec<String>,
    status: Arc<MqttStatus>,
}

impl MqttPublisher {
    async fn subscribe_all(&self) -> Result<(), HomeError> {
        for topic in &self.topics {
            self.client
                .subscribe(topic, QoS::AtLeastOnce)
                .await
                .map_err(|error| HomeError::Publish(format!("subscribe {topic}: {error}")))?;
        }
        Ok(())
    }
}

#[async_trait]
impl Publisher for MqttPublisher {
    async fn publish(&self, topic: &str, payload: &[u8], retain: bool) -> Result<(), HomeError> {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload.to_vec())
            .await
            .map_err(|error| {
                let message = format!("publish {topic}: {error}");
                self.status.note_error(message.clone());
                HomeError::Publish(message)
            })
    }
}

/// Start the service and keep the broker connection status for health
/// reporting.
pub async fn start_with_status(
    config: Arc<crate::config::HomeConfig>,
    clock: Arc<dyn Clock>,
) -> Result<(Arc<Home>, Arc<MqttStatus>), HomeError> {
    let (host, port) = parse_url(&config.mqtt.url)?;
    let mut options = MqttOptions::new(&config.mqtt.client_id, host, port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(false);
    if let Some(username) = &config.mqtt.username {
        options.set_credentials(username, config.mqtt.password.clone().unwrap_or_default());
    }

    let topics = crate::home::subscriptions_for(&config);
    // The event loop subscribes on every ConnAck and is also the only
    // consumer of the request channel. Size the channel for the whole topic
    // list, otherwise a large home blocks inside the ConnAck handler while
    // queueing subscriptions and never polls incoming state.
    let capacity = topics.len().max(100) + 16;
    let (client, eventloop) = AsyncClient::new(options, capacity);

    let status = Arc::new(MqttStatus::default());
    let publisher = Arc::new(MqttPublisher {
        client,
        topics,
        status: Arc::clone(&status),
    });
    let home = Home::new(config, publisher.clone(), clock)?;

    // Do not subscribe before the loop starts: the ConnAck handler already
    // does, and an extra round would double the queued requests on the first
    // connection.
    let loop_home = Arc::clone(&home);
    tokio::spawn(async move {
        run_event_loop(eventloop, loop_home, publisher).await;
    });
    Ok((home, status))
}

/// Connect to the broker, subscribe to the configured state topics and return
/// the home service. The event loop runs in a background task.
pub async fn start(
    config: Arc<crate::config::HomeConfig>,
    clock: Arc<dyn Clock>,
) -> Result<Arc<Home>, HomeError> {
    start_with_status(config, clock).await.map(|(home, _)| home)
}

async fn run_event_loop(mut eventloop: EventLoop, home: Arc<Home>, publisher: Arc<MqttPublisher>) {
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("mqtt connected");
                publisher.status.connected_now();
                if let Err(error) = publisher.subscribe_all().await {
                    warn!(%error, "mqtt resubscribe failed");
                    publisher.status.note_error(format!("subscribe: {error}"));
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                home.ingest(&publish.topic, &publish.payload);
            }
            Ok(Event::Incoming(Packet::Disconnect)) => {
                warn!("mqtt broker disconnected");
                publisher.status.disconnected("broker disconnected");
            }
            Ok(_) => {}
            Err(error) => {
                error!(%error, "mqtt event loop error");
                publisher.status.disconnected(error.to_string());
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

fn parse_url(url: &str) -> Result<(String, u16), HomeError> {
    let rest = url
        .strip_prefix("mqtt://")
        .or_else(|| url.strip_prefix("tcp://"))
        .unwrap_or(url);
    let (host, port) = rest
        .rsplit_once(':')
        .ok_or_else(|| HomeError::Config(format!("mqtt url needs host:port, got {url}")))?;
    let port = port
        .parse()
        .map_err(|_| HomeError::Config(format!("invalid mqtt port in {url}")))?;
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mqtt_urls() {
        assert_eq!(
            parse_url("mqtt://127.0.0.1:1883").unwrap(),
            ("127.0.0.1".to_owned(), 1883)
        );
        assert_eq!(
            parse_url("tcp://broker:1884").unwrap(),
            ("broker".to_owned(), 1884)
        );
        assert!(parse_url("mqtt://localhost").is_err());
    }
}
