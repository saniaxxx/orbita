//! Dump live MQTT topics: `cargo run -p orbita-home --example mqtt_dump -- HOST [SECONDS]`.

use std::{collections::BTreeMap, time::Duration};

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "192.168.250.110".to_owned());
    let seconds: u64 = args.next().and_then(|value| value.parse().ok()).unwrap_or(6);

    let mut options = MqttOptions::new("orbita-mqtt-dump", host, 1883);
    options.set_keep_alive(Duration::from_secs(10));
    let (client, mut eventloop) = AsyncClient::new(options, 64);
    client.subscribe("#", QoS::AtMostOnce).await?;

    let mut topics: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(publish)))) => {
                topics.insert(publish.topic.clone(), publish.payload.to_vec());
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => eprintln!("mqtt: {error}"),
            Err(_) => break,
        }
    }
    println!("topics: {}", topics.len());
    for (topic, payload) in topics {
        println!("{topic}\t{}", String::from_utf8_lossy(&payload));
    }
    Ok(())
}
