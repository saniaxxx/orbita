//! Minimal fake MQTT broker: a home with more topics than the client request
//! channel must still ingest state.
//!
//! Regression: subscribing before the event loop and again in the ConnAck
//! handler doubled the queued requests. Above half the channel capacity the
//! handler blocked queueing subscriptions, never polled incoming PUBLISH and
//! every device stayed without a value.

use std::{sync::Arc, time::Duration};

use orbita_home::{Capability, DeviceConfig, HomeConfig, MqttConfig, Risk, SystemClock, mqtt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const DEVICES: usize = 60;

fn config(url: String) -> HomeConfig {
    let devices = (0..DEVICES)
        .map(|index| DeviceConfig {
            id: format!("light{index}"),
            room: None,
            kind: "light".to_owned(),
            integration: "wirenboard".to_owned(),
            external_id: String::new(),
            aliases: Vec::new(),
            meta_error_topic: None,
            capabilities: vec![Capability {
                name: "on_off".to_owned(),
                kind: "bool".to_owned(),
                min: None,
                max: None,
                unit: None,
                values: Vec::new(),
                readable: true,
                writable: true,
                risk: Risk::Safe,
                state_topic: Some(format!("/devices/wb/controls/{index}")),
                command_topic: Some(format!("/devices/wb/controls/{index}/on")),
                payload_on: None,
                payload_off: None,
                stale_after_ms: None,
                feedback: None,
            }],
        })
        .collect();

    HomeConfig {
        mqtt: MqttConfig {
            url,
            client_id: "fake-broker-test".to_owned(),
            username: None,
            password: None,
        },
        devices,
        stale_after_ms: 30_000,
        rate_limit_per_min: 60,
    }
}

async fn read_packet(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 1];
    stream.read_exact(&mut header).await?;
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        remaining += (byte[0] & 0x7f) as usize * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0u8; remaining];
    stream.read_exact(&mut body).await?;
    Ok((header[0], body))
}

fn publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload);
    let mut packet = vec![0x30];
    let mut remaining = body.len();
    loop {
        let mut byte = (remaining % 128) as u8;
        remaining /= 128;
        if remaining > 0 {
            byte |= 0x80;
        }
        packet.push(byte);
        if remaining == 0 {
            break;
        }
    }
    packet.extend_from_slice(&body);
    packet
}

async fn serve(mut stream: TcpStream, expected: usize) {
    let mut subscribed = 0usize;
    let mut published = false;
    while let Ok((header, body)) = read_packet(&mut stream).await {
        match header >> 4 {
            1 => {
                // CONNECT -> CONNACK, no session.
                let _ = stream.write_all(&[0x20, 0x02, 0x00, 0x00]).await;
            }
            8 => {
                // SUBSCRIBE: packet id, then (len, topic, qos) entries.
                let packet_id = [body[0], body[1]];
                let mut cursor = 2;
                while cursor + 2 <= body.len() {
                    let len = u16::from_be_bytes([body[cursor], body[cursor + 1]]) as usize;
                    cursor += 2;
                    if cursor + len + 1 > body.len() {
                        break;
                    }
                    cursor += len;
                    let _qos = body[cursor];
                    cursor += 1;
                    subscribed += 1;
                }
                let suback = [0x90, 0x03, packet_id[0], packet_id[1], 0x01];
                let _ = stream.write_all(&suback).await;
                if !published && subscribed >= expected {
                    published = true;
                    let _ = stream
                        .write_all(&publish("/devices/wb/controls/0", b"1"))
                        .await;
                }
            }
            12 => {
                // PINGREQ -> PINGRESP
                let _ = stream.write_all(&[0xd0, 0x00]).await;
            }
            14 => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn many_subscriptions_still_ingest_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let broker = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve(stream, DEVICES).await;
    });

    let config = Arc::new(config(format!("mqtt://{address}")));
    let (home, _status) = mqtt::start_with_status(config, Arc::new(SystemClock::new()))
        .await
        .expect("mqtt start");
    assert_eq!(home.subscriptions().len(), DEVICES);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if home
            .get("light0")
            .ok()
            .and_then(|view| view.capabilities[0].value.clone())
            .as_deref()
            == Some("1")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "state was not ingested within 3 s; subscriptions={}",
            home.subscriptions().len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    broker.abort();
}
