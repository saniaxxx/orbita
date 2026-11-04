//! Android TV Remote protocol v2 over TLS.
//!
//! The TV presents a self-signed certificate, so server verification is
//! disabled; authentication is the client certificate issued during pairing.
//! Messages are length-delimited protobufs.

use std::{
    io::BufReader,
    net::{Ipv4Addr, UdpSocket},
    sync::Arc,
    time::Duration,
};

use prost::Message;
use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

use crate::proto::{
    DIRECTION_SHORT, KEYCODE_POWER, KEYCODE_VOLUME_DOWN, KEYCODE_VOLUME_UP, REQUESTED_FEATURES,
    RemoteAppLinkLaunchRequest, RemoteConfigure, RemoteDeviceInfo, RemoteKeyInject, RemoteMessage,
    RemotePingResponse, RemoteSetActive, RemoteSetVolumeLevel,
};

pub struct TvLink {
    stream: tokio_rustls::client::TlsStream<TcpStream>,
    state: TvState,
    features: i32,
}

#[derive(Debug, Clone, Default)]
pub struct TvState {
    pub is_on: Option<bool>,
    pub volume_level: Option<u32>,
    pub volume_max: Option<u32>,
    pub muted: Option<bool>,
    pub current_app: Option<String>,
    pub device_info: Option<DeviceInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    pub sw_version: String,
}

impl TvState {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push(match self.is_on {
            Some(true) => "включён".to_owned(),
            Some(false) => "выключен".to_owned(),
            None => "состояние неизвестно".to_owned(),
        });
        if let Some(level) = self.volume_level {
            let max = self.volume_max.unwrap_or(100);
            let muted = if self.muted == Some(true) {
                ", без звука"
            } else {
                ""
            };
            parts.push(format!("громкость {level} из {max}{muted}"));
        }
        if let Some(app) = &self.current_app {
            parts.push(format!("приложение {app}"));
        }
        if let Some(info) = &self.device_info {
            let name = [info.manufacturer.as_str(), info.model.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !name.is_empty() {
                parts.push(name);
            }
        }
        parts.join(", ")
    }
}

impl TvLink {
    pub async fn open(device: &crate::config::TvDevice) -> Result<Self, String> {
        let mut link = Self::connect(device).await?;
        link.handshake().await?;
        Ok(link)
    }

    async fn connect(device: &crate::config::TvDevice) -> Result<Self, String> {
        if !device.is_remote_v2() {
            return Err(format!(
                "телевизор {} не поддерживает Remote v2 (protocol={})",
                device.id, device.protocol
            ));
        }
        let cert = device
            .cert
            .as_deref()
            .ok_or_else(|| format!("у телевизора {} нет cert", device.id))?;
        let key = device
            .key
            .as_deref()
            .ok_or_else(|| format!("у телевизора {} нет key", device.id))?;
        let tls = tls_config(cert, key)?;
        let address = (device.host.as_str(), device.api_port);
        let tcp = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(address))
            .await
            .map_err(|_| format!("таймаут подключения к {}", device.host))?
            .map_err(|error| format!("подключение к {}: {error}", device.host))?;
        let _ = tcp.set_nodelay(true);
        let connector = TlsConnector::from(Arc::new(tls));
        let server_name =
            ServerName::try_from("androidtv.invalid").map_err(|error| error.to_string())?;
        let stream =
            tokio::time::timeout(Duration::from_secs(5), connector.connect(server_name, tcp))
                .await
                .map_err(|_| format!("таймаут TLS с {}", device.host))?
                .map_err(|error| format!("TLS с {}: {error}", device.host))?;
        Ok(Self {
            stream,
            state: TvState::default(),
            features: 0,
        })
    }

    async fn handshake(&mut self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        loop {
            let message = tokio::time::timeout_at(deadline, self.read_message())
                .await
                .map_err(|_| "телевизор не начал сессию".to_owned())??;
            if let Some(configure) = message.remote_configure {
                self.features = REQUESTED_FEATURES & configure.code1;
                if let Some(info) = configure.device_info {
                    self.state.device_info = Some(DeviceInfo {
                        manufacturer: info.vendor,
                        model: info.model,
                        sw_version: info.app_version,
                    });
                }
                let reply = RemoteMessage {
                    remote_configure: Some(RemoteConfigure {
                        code1: self.features,
                        device_info: Some(RemoteDeviceInfo {
                            unknown1: 1,
                            unknown2: "1".to_owned(),
                            package_name: "atvremote".to_owned(),
                            app_version: "1.0.0".to_owned(),
                            ..Default::default()
                        }),
                    }),
                    ..Default::default()
                };
                self.send(&reply).await?;
                continue;
            }
            if message.remote_set_active.is_some() {
                let reply = RemoteMessage {
                    remote_set_active: Some(RemoteSetActive {
                        active: self.features,
                    }),
                    ..Default::default()
                };
                self.send(&reply).await?;
                continue;
            }
            if let Some(ping) = message.remote_ping_request {
                let reply = RemoteMessage {
                    remote_ping_response: Some(RemotePingResponse { val1: ping.val1 }),
                    ..Default::default()
                };
                self.send(&reply).await?;
                continue;
            }
            if let Some(start) = message.remote_start {
                self.state.is_on = Some(start.started);
                return Ok(());
            }
            if let Some(error) = message.remote_error {
                return Err(format!("телевизор вернул ошибку: {error:?}"));
            }
            self.apply(message);
        }
    }

    fn apply(&mut self, message: RemoteMessage) {
        if let Some(volume) = message.remote_set_volume_level {
            self.apply_volume(volume);
        }
        if let Some(ime) = message.remote_ime_key_inject
            && let Some(info) = ime.app_info
            && !info.app_package.is_empty()
        {
            self.state.current_app = Some(info.app_package);
        }
        if let Some(start) = message.remote_start {
            self.state.is_on = Some(start.started);
        }
    }

    fn apply_volume(&mut self, volume: RemoteSetVolumeLevel) {
        self.state.volume_level = Some(volume.volume_level);
        self.state.volume_max = Some(volume.volume_max);
        self.state.muted = Some(volume.volume_muted);
    }

    async fn read_message(&mut self) -> Result<RemoteMessage, String> {
        let length = self.read_varint().await?;
        if length > 1_048_576 {
            return Err(format!("слишком длинное сообщение телевизора: {length}"));
        }
        let mut buffer = vec![0u8; length as usize];
        self.stream
            .read_exact(&mut buffer)
            .await
            .map_err(|error| format!("чтение от телевизора: {error}"))?;
        RemoteMessage::decode(buffer.as_slice())
            .map_err(|error| format!("protobuf от телевизора: {error}"))
    }

    async fn read_varint(&mut self) -> Result<u64, String> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let mut byte = [0u8; 1];
            self.stream
                .read_exact(&mut byte)
                .await
                .map_err(|error| format!("чтение длины от телевизора: {error}"))?;
            result |= u64::from(byte[0] & 0x7f) << shift;
            if byte[0] & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 64 {
                return Err("некорректная длина protobuf".to_owned());
            }
        }
    }

    async fn send(&mut self, message: &RemoteMessage) -> Result<(), String> {
        let bytes = message.encode_length_delimited_to_vec();
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|error| format!("запись в телевизор: {error}"))?;
        self.stream
            .flush()
            .await
            .map_err(|error| format!("flush в телевизор: {error}"))
    }

    pub(crate) async fn drain_for(&mut self, duration: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + duration;
        loop {
            match tokio::time::timeout_at(deadline, self.read_message()).await {
                Ok(Ok(message)) => self.apply(message),
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(()),
            }
        }
    }

    pub fn state(&self) -> TvState {
        self.state.clone()
    }

    pub async fn send_key(&mut self, key_code: i32) -> Result<(), String> {
        self.send(&RemoteMessage {
            remote_key_inject: Some(RemoteKeyInject {
                key_code,
                direction: DIRECTION_SHORT,
            }),
            ..Default::default()
        })
        .await?;
        self.drain_for(Duration::from_millis(250)).await
    }

    pub async fn send_app_link(&mut self, link: &str) -> Result<(), String> {
        self.send(&RemoteMessage {
            remote_app_link_launch_request: Some(RemoteAppLinkLaunchRequest {
                app_link: link.to_owned(),
            }),
            ..Default::default()
        })
        .await?;
        self.drain_for(Duration::from_millis(500)).await
    }

    pub async fn set_power(&mut self, on: bool) -> Result<TvState, String> {
        if let Some(current) = self.state.is_on
            && current == on
        {
            return Ok(self.state());
        }
        self.send(&RemoteMessage {
            remote_key_inject: Some(RemoteKeyInject {
                key_code: KEYCODE_POWER,
                direction: DIRECTION_SHORT,
            }),
            ..Default::default()
        })
        .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            match tokio::time::timeout_at(deadline, self.read_message()).await {
                Ok(Ok(message)) => {
                    self.apply(message);
                    if self.state.is_on == Some(on) {
                        return Ok(self.state());
                    }
                }
                Ok(Err(error)) => {
                    if !on {
                        // Going to standby closes the remote connection.
                        self.state.is_on = Some(false);
                        return Ok(self.state());
                    }
                    return Err(error);
                }
                Err(_) => {
                    return self
                        .state
                        .is_on
                        .map(|current| {
                            let mut state = self.state();
                            state.is_on = Some(current);
                            state
                        })
                        .ok_or_else(|| "телевизор не подтвердил состояние питания".to_owned());
                }
            }
        }
    }

    pub async fn volume_step(&mut self, up: bool) -> Result<TvState, String> {
        self.send_key(if up {
            KEYCODE_VOLUME_UP
        } else {
            KEYCODE_VOLUME_DOWN
        })
        .await?;
        self.drain_for(Duration::from_millis(600)).await?;
        Ok(self.state())
    }

    pub async fn set_volume(&mut self, target: u32) -> Result<TvState, String> {
        let Some(current) = self.state.volume_level else {
            return Err("телевизор не сообщил текущую громкость".to_owned());
        };
        let max = self.state.volume_max.unwrap_or(100);
        let target = target.min(max);
        if target == current {
            return Ok(self.state());
        }
        let delta = target.abs_diff(current);
        if delta > 50 {
            return Err(format!(
                "слишком большая разница громкости: {current} → {target}"
            ));
        }
        let up = target > current;
        for _ in 0..delta {
            self.send(&key_message(if up {
                KEYCODE_VOLUME_UP
            } else {
                KEYCODE_VOLUME_DOWN
            }))
            .await?;
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        self.drain_for(Duration::from_millis(1200)).await?;
        Ok(self.state())
    }
}

fn key_message(key_code: i32) -> RemoteMessage {
    RemoteMessage {
        remote_key_inject: Some(RemoteKeyInject {
            key_code,
            direction: DIRECTION_SHORT,
        }),
        ..Default::default()
    }
}

/// Send a Wake-on-LAN magic packet. Harmless when the TV is already awake.
pub fn wake_on_lan(mac: &str, host: &str, broadcast: Option<&str>) -> Result<(), String> {
    let octets: Vec<u8> = mac
        .split([':', '-'])
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .map_err(|error| format!("некорректный MAC {mac}: {error}"))?;
    if octets.len() != 6 {
        return Err(format!("некорректный MAC {mac}"));
    }
    let mut packet = vec![0xffu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&octets);
    }
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| format!("WoL socket: {error}"))?;
    socket
        .set_broadcast(true)
        .map_err(|error| format!("WoL broadcast: {error}"))?;
    let mut targets = vec![host.to_owned()];
    if let Some(broadcast) = broadcast {
        targets.push(broadcast.to_owned());
    }
    targets.push(Ipv4Addr::BROADCAST.to_string());
    // Ports 7 and 9 are both used by WoL implementations; repeat each packet
    // because the first one can be lost while the NIC is waking up.
    for target in &targets {
        for port in [7u16, 9u16] {
            for _ in 0..2 {
                let _ = socket.send_to(&packet, (target.as_str(), port));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

fn tls_config(cert: &str, key: &str) -> Result<ClientConfig, String> {
    let mut cert_reader = BufReader::new(
        std::fs::File::open(cert).map_err(|error| format!("открытие {cert}: {error}"))?,
    );
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("чтение сертификата {cert}: {error}"))?;
    if certs.is_empty() {
        return Err(format!("в {cert} нет сертификата"));
    }
    let mut key_reader = BufReader::new(
        std::fs::File::open(key).map_err(|error| format!("открытие {key}: {error}"))?,
    );
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|error| format!("чтение ключа {key}: {error}"))?
        .ok_or_else(|| format!("в {key} нет приватного ключа"))?;
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("TLS versions: {error}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_client_auth_cert(certs, key)
        .map_err(|error| format!("клиентский сертификат: {error}"))
}
