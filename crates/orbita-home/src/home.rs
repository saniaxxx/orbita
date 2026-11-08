//! Home service: validates actions, enforces local safety limits and publishes
//! commands. Policy (who may do what) belongs to the agent; this service only
//! checks a grant and the device's own capabilities.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::Instant,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use crate::config::HomeConfig;
use crate::model::{
    Action, ActionRequest, Capability, CapabilityView, DeviceConfig, DeviceView, Grant, HomeError,
    Published, Risk,
};

#[async_trait]
pub trait Publisher: Send + Sync {
    async fn publish(&self, topic: &str, payload: &[u8], retain: bool) -> Result<(), HomeError>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn monotonic_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock {
    start: Option<Instant>,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Some(Instant::now()),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn monotonic_ms(&self) -> u64 {
        self.start
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
struct Reported {
    value: String,
    received_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct DeviceState {
    values: HashMap<String, Reported>,
    error: Option<String>,
}

/// A change reported by the MQTT transport.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Value {
        device_id: String,
        capability: String,
        topic: String,
        value: String,
        received_at: DateTime<Utc>,
    },
    Error {
        device_id: String,
        topic: String,
        error: Option<String>,
        received_at: DateTime<Utc>,
    },
}

impl Change {
    pub fn device_id(&self) -> &str {
        match self {
            Self::Value { device_id, .. } | Self::Error { device_id, .. } => device_id,
        }
    }

    pub fn capability(&self) -> Option<&str> {
        match self {
            Self::Value { capability, .. } => Some(capability),
            Self::Error { .. } => None,
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Value { value, .. } => Some(value),
            Self::Error { .. } => None,
        }
    }
}

struct TopicMaps {
    states: HashMap<String, (String, String)>,
    /// One physical Wiren Board device publishes one `meta/error` topic for all
    /// of its controls, so a topic maps to every configured logical device.
    errors: HashMap<String, Vec<String>>,
}

pub struct Home {
    config: Arc<HomeConfig>,
    publisher: Arc<dyn Publisher>,
    clock: Arc<dyn Clock>,
    states: RwLock<HashMap<String, DeviceState>>,
    rate: Mutex<HashMap<String, VecDeque<u64>>>,
    topics: TopicMaps,
    changes: broadcast::Sender<Change>,
}

impl Home {
    pub fn new(
        config: Arc<HomeConfig>,
        publisher: Arc<dyn Publisher>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, HomeError> {
        config.validate()?;
        let mut states = HashMap::new();
        let mut state_topics = HashMap::new();
        let mut error_topics = HashMap::new();

        for device in &config.devices {
            states.insert(device.id.clone(), DeviceState::default());
            if let Some(topic) = &device.meta_error_topic {
                error_topics
                    .entry(topic.clone())
                    .or_insert_with(Vec::new)
                    .push(device.id.clone());
            }
            for capability in &device.capabilities {
                if let Some(topic) = &capability.state_topic {
                    state_topics
                        .insert(topic.clone(), (device.id.clone(), capability.name.clone()));
                }
            }
        }

        let (changes, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            config,
            publisher,
            clock,
            states: RwLock::new(states),
            rate: Mutex::new(HashMap::new()),
            topics: TopicMaps {
                states: state_topics,
                errors: error_topics,
            },
            changes,
        }))
    }

    /// Subscribe to changes reported on MQTT state and `meta/error` topics.
    /// A lagging receiver may see `RecvError::Lagged`; the cache is always
    /// authoritative, so callers can resynchronise with `get()`.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    pub fn config(&self) -> &HomeConfig {
        &self.config
    }

    /// Topics the MQTT transport should subscribe to.
    pub fn subscriptions(&self) -> Vec<String> {
        subscriptions_for(&self.config)
    }

    pub fn ingest(&self, topic: &str, payload: &[u8]) {
        let value = String::from_utf8_lossy(payload).into_owned();
        let received_at = self.clock.now();
        let mut states = self.states.write().unwrap_or_else(|e| e.into_inner());

        if let Some((device_id, capability)) = self.topics.states.get(topic)
            && let Some(state) = states.get_mut(device_id)
        {
            state.values.insert(
                capability.clone(),
                Reported {
                    value: value.clone(),
                    received_at,
                },
            );
            let _ = self.changes.send(Change::Value {
                device_id: device_id.clone(),
                capability: capability.clone(),
                topic: topic.to_owned(),
                value,
                received_at,
            });
            return;
        }

        if let Some(device_ids) = self.topics.errors.get(topic) {
            let error = if value.is_empty() { None } else { Some(value) };
            for device_id in device_ids {
                if let Some(state) = states.get_mut(device_id) {
                    state.error = error.clone();
                    let _ = self.changes.send(Change::Error {
                        device_id: device_id.clone(),
                        topic: topic.to_owned(),
                        error: error.clone(),
                        received_at,
                    });
                }
            }
        }
    }

    pub fn list(&self) -> Vec<DeviceView> {
        self.config
            .devices
            .iter()
            .filter_map(|device| self.device_view(device).ok())
            .collect()
    }

    pub fn get(&self, device_id: &str) -> Result<DeviceView, HomeError> {
        let device = self
            .config
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .ok_or_else(|| HomeError::UnknownDevice(device_id.to_owned()))?;
        self.device_view(device)
    }

    /// Apply a validated action. Sensitive and dangerous actions require a
    /// grant from the agent's policy layer.
    pub async fn apply(
        &self,
        request: &ActionRequest,
        grant: Option<&Grant>,
    ) -> Result<Published, HomeError> {
        let device = self
            .config
            .devices
            .iter()
            .find(|device| device.id == request.device_id)
            .ok_or_else(|| HomeError::UnknownDevice(request.device_id.clone()))?;
        let capability =
            device
                .capability(&request.capability)
                .ok_or_else(|| HomeError::UnknownCapability {
                    device: request.device_id.clone(),
                    capability: request.capability.clone(),
                })?;

        if capability.risk.requires_grant() {
            if self.device_unavailable(device) {
                return Err(HomeError::Denied(format!(
                    "{} has no usable state; refusing a {:?} action",
                    device.id, capability.risk
                )));
            }
            let now = self.clock.now();
            let valid = grant.is_some_and(|grant| grant.matches(request, now));
            if !valid {
                return Err(HomeError::NeedsGrant(request.key()));
            }
        }

        capability.validate(&request.action)?;
        if matches!(request.action, Action::Status) {
            return Err(HomeError::Denied(
                "status is a read operation, use get()".to_owned(),
            ));
        }

        self.check_rate(&request.key())?;
        let topic = capability
            .command_topic
            .as_ref()
            .ok_or_else(|| HomeError::Config(format!("{} has no command topic", capability.name)))?
            .clone();
        let payload = capability.payload(&request.action)?;
        self.publisher.publish(&topic, &payload, false).await?;

        Ok(Published {
            device_id: request.device_id.clone(),
            capability: request.capability.clone(),
            topic,
            payload: String::from_utf8_lossy(&payload).into_owned(),
            risk: capability.risk,
        })
    }

    /// Availability for safety checks: `meta/error` or no reported value.
    /// Age is deliberately ignored: Wiren Board keeps state retained and
    /// publishes only on change, so a 30 s quiet period is normal.
    fn device_unavailable(&self, device: &DeviceConfig) -> bool {
        let states = self.states.read().unwrap_or_else(|e| e.into_inner());
        let Some(state) = states.get(&device.id) else {
            return true;
        };
        if state.error.is_some() {
            return true;
        }
        device
            .capabilities
            .iter()
            .filter(|cap| cap.readable)
            .any(|cap| !state.values.contains_key(&cap.name))
    }

    fn check_rate(&self, key: &str) -> Result<(), HomeError> {
        let limit = self.config.rate_limit_per_min;
        if limit == 0 {
            return Ok(());
        }
        let now = self.clock.monotonic_ms();
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        let window = rate.entry(key.to_owned()).or_default();
        while window
            .front()
            .is_some_and(|ts| now.saturating_sub(*ts) >= 60_000)
        {
            window.pop_front();
        }
        if window.len() >= limit as usize {
            return Err(HomeError::RateLimited(key.to_owned()));
        }
        window.push_back(now);
        Ok(())
    }

    fn device_view(&self, device: &DeviceConfig) -> Result<DeviceView, HomeError> {
        let states = self.states.read().unwrap_or_else(|e| e.into_inner());
        let state = states
            .get(&device.id)
            .ok_or_else(|| HomeError::Config(format!("no state slot for device {}", device.id)))?;
        let now = self.clock.now();
        let mut capabilities = Vec::with_capacity(device.capabilities.len());
        let mut stale = state.error.is_some();
        let mut any_readable_value = false;

        for capability in &device.capabilities {
            let reported = state.values.get(&capability.name);
            let capability_stale = reported.is_none_or(|reported| {
                let age = now
                    .signed_duration_since(reported.received_at)
                    .num_milliseconds();
                age < 0 || age as u64 > capability.stale_after_ms(self.config.stale_after_ms)
            });
            stale |= capability.readable && capability_stale;
            let available = state.error.is_none() && reported.is_some();
            any_readable_value |= capability.readable && available;
            capabilities.push(CapabilityView {
                name: capability.name.clone(),
                kind: capability.kind.clone(),
                risk: capability.risk,
                readable: capability.readable,
                writable: capability.writable,
                value: reported.map(|reported| reported.value.clone()),
                received_at: reported.map(|reported| reported.received_at),
                stale: capability_stale,
                available,
            });
        }

        let has_readable = device.capabilities.iter().any(|cap| cap.readable);
        Ok(DeviceView {
            id: device.id.clone(),
            room: device.room.clone(),
            kind: device.kind.clone(),
            error: state.error.clone(),
            available: state.error.is_none() && (any_readable_value || !has_readable),
            stale,
            capabilities,
        })
    }
}

/// Convenience constructor for an action on a capability.
pub fn action(device_id: &str, capability: &str, action: Action) -> ActionRequest {
    ActionRequest::new(device_id, capability, action)
}

/// All MQTT topics that carry device state or availability.
pub fn subscriptions_for(config: &HomeConfig) -> Vec<String> {
    let mut topics: Vec<String> = config
        .devices
        .iter()
        .flat_map(|device| {
            device
                .capabilities
                .iter()
                .filter_map(|capability| capability.state_topic.clone())
                .chain(device.meta_error_topic.clone())
        })
        .collect();
    topics.sort();
    topics.dedup();
    topics
}

/// Kept for callers that need the risk of a capability without a request.
pub fn risk_of(device: &DeviceConfig, capability: &str) -> Option<Risk> {
    device
        .capability(capability)
        .map(|capability: &Capability| capability.risk)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::config::MqttConfig;
    use crate::model::Capability;

    #[derive(Default)]
    struct FakePublisher {
        published: Mutex<Vec<(String, Vec<u8>, bool)>>,
    }

    #[async_trait]
    impl Publisher for FakePublisher {
        async fn publish(
            &self,
            topic: &str,
            payload: &[u8],
            retain: bool,
        ) -> Result<(), HomeError> {
            self.published
                .lock()
                .unwrap()
                .push((topic.to_owned(), payload.to_vec(), retain));
            Ok(())
        }
    }

    struct FakeClock {
        now: DateTime<Utc>,
        ms: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: DateTime::parse_from_rfc3339("2026-11-03T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, millis: u64) {
            self.ms.fetch_add(millis, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            self.now + chrono::Duration::milliseconds(self.ms.load(Ordering::SeqCst) as i64)
        }

        fn monotonic_ms(&self) -> u64 {
            self.ms.load(Ordering::SeqCst)
        }
    }

    fn capability(name: &str, risk: Risk) -> Capability {
        Capability {
            name: name.to_owned(),
            kind: "bool".to_owned(),
            min: None,
            max: None,
            unit: None,
            values: vec![],
            readable: true,
            writable: true,
            risk,
            state_topic: Some(format!("/devices/wb/controls/{name}")),
            command_topic: Some(format!("/devices/wb/controls/{name}/on")),
            payload_on: None,
            payload_off: None,
            stale_after_ms: None,
            feedback: None,
        }
    }

    fn config(risk: Risk, rate_limit_per_min: u32) -> HomeConfig {
        HomeConfig {
            mqtt: MqttConfig {
                url: "mqtt://127.0.0.1:1883".to_owned(),
                client_id: "test".to_owned(),
                username: None,
                password: None,
            },
            devices: vec![DeviceConfig {
                id: "light".to_owned(),
                room: Some("living".to_owned()),
                kind: "light".to_owned(),
                integration: "wirenboard".to_owned(),
                external_id: "wb/1".to_owned(),
                aliases: vec![],
                meta_error_topic: Some("/devices/wb/meta/error".to_owned()),
                capabilities: vec![capability("on_off", risk)],
            }],
            stale_after_ms: 30_000,
            rate_limit_per_min,
        }
    }

    fn home(
        risk: Risk,
        rate_limit_per_min: u32,
    ) -> (Arc<Home>, Arc<FakePublisher>, Arc<FakeClock>) {
        let publisher = Arc::new(FakePublisher::default());
        let clock = Arc::new(FakeClock::new());
        let home = Home::new(
            Arc::new(config(risk, rate_limit_per_min)),
            publisher.clone(),
            clock.clone(),
        )
        .unwrap();
        (home, publisher, clock)
    }

    #[tokio::test]
    async fn safe_action_publishes_without_retain() {
        let (home, publisher, _) = home(Risk::Safe, 60);
        let result = home
            .apply(&action("light", "on_off", Action::OnOff { on: true }), None)
            .await
            .unwrap();
        assert_eq!(result.topic, "/devices/wb/controls/on_off/on");
        assert_eq!(result.payload, "1");
        let published = publisher.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert!(!published[0].2);
    }

    #[tokio::test]
    async fn dangerous_action_needs_valid_grant() {
        let (home, _, clock) = home(Risk::Dangerous, 60);
        home.ingest("/devices/wb/controls/on_off", b"0");
        let request = action("light", "on_off", Action::OnOff { on: true });
        assert!(matches!(
            home.apply(&request, None).await,
            Err(HomeError::NeedsGrant(_))
        ));

        let grant = Grant {
            device_id: "light".to_owned(),
            capability: "on_off".to_owned(),
            action_hash: request.key(),
            expires_at: clock.now() + chrono::Duration::seconds(30),
            nonce: "n".to_owned(),
        };
        assert!(home.apply(&request, Some(&grant)).await.is_ok());
    }

    #[tokio::test]
    async fn dangerous_action_is_denied_without_a_reported_state() {
        let (home, _, clock) = home(Risk::Dangerous, 60);
        let request = action("light", "on_off", Action::OnOff { on: true });
        let grant = Grant {
            device_id: "light".to_owned(),
            capability: "on_off".to_owned(),
            action_hash: request.key(),
            expires_at: clock.now() + chrono::Duration::seconds(30),
            nonce: "n".to_owned(),
        };
        assert!(matches!(
            home.apply(&request, Some(&grant)).await,
            Err(HomeError::Denied(_))
        ));
    }

    #[tokio::test]
    async fn old_but_reported_state_does_not_block_a_granted_action() {
        let (home, _, clock) = home(Risk::Dangerous, 60);
        home.ingest("/devices/wb/controls/on_off", b"0");
        clock.advance(120_000);
        let view = home.get("light").unwrap();
        assert!(view.stale);
        assert!(view.available);

        let request = action("light", "on_off", Action::OnOff { on: true });
        let grant = Grant {
            device_id: "light".to_owned(),
            capability: "on_off".to_owned(),
            action_hash: request.key(),
            expires_at: clock.now() + chrono::Duration::seconds(30),
            nonce: "n".to_owned(),
        };
        assert!(home.apply(&request, Some(&grant)).await.is_ok());
    }

    #[tokio::test]
    async fn ingest_notifies_subscribers() {
        let (home, _, _) = home(Risk::Safe, 60);
        let mut changes = home.subscribe();
        home.ingest("/devices/wb/controls/on_off", b"1");
        let change = changes.try_recv().unwrap();
        assert_eq!(change.device_id(), "light");
        assert_eq!(change.capability(), Some("on_off"));
        assert_eq!(change.value(), Some("1"));
    }

    #[tokio::test]
    async fn rate_limit_is_enforced() {
        let (home, _, clock) = home(Risk::Safe, 1);
        let request = action("light", "on_off", Action::OnOff { on: true });
        assert!(home.apply(&request, None).await.is_ok());
        assert!(matches!(
            home.apply(&request, None).await,
            Err(HomeError::RateLimited(_))
        ));
        clock.advance(60_001);
        assert!(home.apply(&request, None).await.is_ok());
    }

    #[test]
    fn state_is_read_and_becomes_stale() {
        let (home, _, clock) = home(Risk::Safe, 60);
        home.ingest("/devices/wb/controls/on_off", b"1");
        let view = home.get("light").unwrap();
        assert_eq!(view.capabilities[0].value.as_deref(), Some("1"));
        assert!(!view.capabilities[0].stale);
        assert!(view.capabilities[0].available);
        assert!(view.available);

        clock.advance(31_000);
        let view = home.get("light").unwrap();
        assert!(view.capabilities[0].stale);
        assert!(view.capabilities[0].available);
        assert!(view.available);
        assert!(view.stale);
    }

    #[test]
    fn meta_error_marks_device_stale() {
        let (home, _, _) = home(Risk::Safe, 60);
        home.ingest("/devices/wb/controls/on_off", b"0");
        home.ingest("/devices/wb/meta/error", b"r");
        let view = home.get("light").unwrap();
        assert_eq!(view.error.as_deref(), Some("r"));
        assert!(view.stale);
        home.ingest("/devices/wb/meta/error", b"");
        assert!(!home.get("light").unwrap().stale);
    }
}
