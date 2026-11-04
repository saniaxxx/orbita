//! MQTT transport for Wiren Board.
//!
//! Commands are always published with `retain = false`: a retained command is
//! replayed after a reconnect and can open a gate or toggle a light again.
//! State and `meta/error` topics are subscribed with QoS1.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use tracing::{error, info, warn};

use crate::home::{Clock, Home, Publisher};
use crate::model::HomeError;

pub struct MqttPublisher {
    client: AsyncClient,
    topics: Vec<String>,
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
            .map_err(|error| HomeError::Publish(format!("publish {topic}: {error}")))
    }
}

/// Connect to the broker, subscribe to the configured state topics and return
/// the home service. The event loop runs in a background task.
pub async fn start(
    config: Arc<crate::config::HomeConfig>,
    clock: Arc<dyn Clock>,
) -> Result<Arc<Home>, HomeError> {
    let (host, port) = parse_url(&config.mqtt.url)?;
    let mut options = MqttOptions::new(&config.mqtt.client_id, host, port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(false);
    if let Some(username) = &config.mqtt.username {
        options.set_credentials(username, config.mqtt.password.clone().unwrap_or_default());
    }
    let (client, eventloop) = AsyncClient::new(options, 100);

    let topics = crate::home::subscriptions_for(&config);
    let publisher = Arc::new(MqttPublisher { client, topics });
    let home = Home::new(config, publisher.clone(), clock)?;

    publisher.subscribe_all().await?;
    let loop_home = Arc::clone(&home);
    tokio::spawn(async move {
        run_event_loop(eventloop, loop_home, publisher).await;
    });
    Ok(home)
}

async fn run_event_loop(mut eventloop: EventLoop, home: Arc<Home>, publisher: Arc<MqttPublisher>) {
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                info!("mqtt connected");
                if let Err(error) = publisher.subscribe_all().await {
                    warn!(%error, "mqtt resubscribe failed");
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                home.ingest(&publish.topic, &publish.payload);
            }
            Ok(_) => {}
            Err(error) => {
                error!(%error, "mqtt event loop error");
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
