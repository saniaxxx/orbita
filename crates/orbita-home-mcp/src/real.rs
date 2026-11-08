//! Real backend: Wiren Board state over MQTT and commands through
//! `orbita-home`, with confirmation from the state topic.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use orbita_home::{
    Action, ActionRequest, Capability, Change, DeviceView, Home, HomeConfig, HomeError, MqttStatus,
    Risk, SystemClock, mqtt,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    Backend, CapabilityInfo, CommandParams, CommandResult, CommandStatus, Device, EventsPage,
    EventsParams, HomeEvent, SceneParams, now_ms, parse_action, parse_bool_str, room_matches,
};

#[derive(Debug, Clone, Serialize)]
struct AuditEntry {
    request_id: String,
    device: String,
    capability: String,
    status: CommandStatus,
    reason: Option<String>,
    timestamp_ms: u64,
}

#[derive(Debug)]
struct EventLog {
    epoch: u64,
    next_cursor: u64,
    events: VecDeque<HomeEvent>,
}

pub struct RealHome {
    home: Arc<Home>,
    status: Arc<MqttStatus>,
    ack_timeout: Duration,
    events: Arc<Mutex<EventLog>>,
    audit: Mutex<VecDeque<AuditEntry>>,
}

impl RealHome {
    pub async fn start(config_path: &str, ack_timeout: Duration) -> Result<Self, String> {
        let mut config = HomeConfig::from_path(config_path).map_err(|error| error.to_string())?;
        config.apply_env_overrides();
        let (home, status) =
            mqtt::start_with_status(Arc::new(config), Arc::new(SystemClock::new()))
                .await
                .map_err(|error| error.to_string())?;

        // Give the broker a moment to deliver retained state before the event
        // baseline is taken.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !status.connected() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        Ok(Self::new(home, status, ack_timeout))
    }

    pub fn new(home: Arc<Home>, status: Arc<MqttStatus>, ack_timeout: Duration) -> Self {
        let events = Arc::new(Mutex::new(EventLog {
            epoch: now_ms(),
            next_cursor: 1,
            events: VecDeque::new(),
        }));
        let changes = home.subscribe();
        let baseline = current_values(&home);
        spawn_event_log(Arc::clone(&home), Arc::clone(&events), baseline, changes);
        Self {
            home,
            status,
            ack_timeout,
            events,
            audit: Mutex::new(VecDeque::new()),
        }
    }

    async fn accept(&self, params: &CommandParams) -> CommandResult {
        let Some(device) = self
            .home
            .config()
            .devices
            .iter()
            .find(|device| device.id == params.device_id)
        else {
            return self.result(
                CommandStatus::Failed,
                params,
                None,
                Some(format!("unknown device {}", params.device_id)),
            );
        };
        let Some(capability) = device.capability(&params.capability) else {
            return self.result(
                CommandStatus::Failed,
                params,
                None,
                Some(format!(
                    "unknown capability {}/{}",
                    params.device_id, params.capability
                )),
            );
        };
        if capability.risk != Risk::Safe {
            return self.result(
                CommandStatus::Denied,
                params,
                None,
                Some(
                    "dangerous capability is deny-by-default; out-of-band approval is not implemented"
                        .into(),
                ),
            );
        }
        let action = match parse_action(&capability.kind, &params.value) {
            Ok(action) => action,
            Err(reason) => return self.result(CommandStatus::Denied, params, None, Some(reason)),
        };
        let request = ActionRequest::new(
            params.device_id.clone(),
            params.capability.clone(),
            action.clone(),
        );

        // Subscribe before publishing so a fast state report is not lost.
        let mut changes = self.home.subscribe();
        if let Err(error) = self.home.apply(&request, None).await {
            let (status, reason) = map_apply_error(&error);
            return self.result(
                status,
                params,
                self.state_snapshot(&params.device_id),
                Some(reason),
            );
        }

        if self.confirmed(&params.device_id, &params.capability, capability, &action) {
            return self.result(
                CommandStatus::Executed,
                params,
                self.state_snapshot(&params.device_id),
                None,
            );
        }

        let deadline = tokio::time::Instant::now() + self.ack_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, changes.recv()).await {
                Err(_) => break,
                Ok(Ok(Change::Value {
                    device_id,
                    capability: reported,
                    value,
                    ..
                })) if device_id == params.device_id
                    && reported == params.capability
                    && capability.matches_value(&action, &value) =>
                {
                    return self.result(
                        CommandStatus::Executed,
                        params,
                        self.state_snapshot(&params.device_id),
                        None,
                    );
                }
                Ok(Ok(Change::Error {
                    device_id, error, ..
                })) if device_id == params.device_id => {
                    return self.result(
                        CommandStatus::Failed,
                        params,
                        self.state_snapshot(&params.device_id),
                        Some(format!(
                            "device reported an error: {}",
                            error.unwrap_or_else(|| "unknown".into())
                        )),
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(RecvError::Lagged(skipped))) => {
                    tracing::warn!(skipped, device = %params.device_id, "ack stream lagged");
                    if self.confirmed(&params.device_id, &params.capability, capability, &action) {
                        return self.result(
                            CommandStatus::Executed,
                            params,
                            self.state_snapshot(&params.device_id),
                            None,
                        );
                    }
                }
                Ok(Err(RecvError::Closed)) => {
                    return self.result(
                        CommandStatus::Failed,
                        params,
                        self.state_snapshot(&params.device_id),
                        Some("MQTT change stream closed".into()),
                    );
                }
            }
        }

        self.result(
            CommandStatus::Failed,
            params,
            self.state_snapshot(&params.device_id),
            Some(format!(
                "no state confirmation for {}/{} within {} ms",
                params.device_id,
                params.capability,
                self.ack_timeout.as_millis()
            )),
        )
    }

    fn confirmed(
        &self,
        device_id: &str,
        capability_name: &str,
        capability: &Capability,
        action: &Action,
    ) -> bool {
        let Ok(view) = self.home.get(device_id) else {
            return false;
        };
        if !view.available {
            return false;
        }
        view.capabilities
            .iter()
            .find(|reported| reported.name == capability_name)
            .and_then(|reported| reported.value.as_deref())
            .is_some_and(|value| capability.matches_value(action, value))
    }

    fn state_snapshot(&self, device_id: &str) -> Option<Value> {
        self.home
            .get(device_id)
            .ok()
            .map(|view| state_values(&self.home, &view))
    }

    fn result(
        &self,
        status: CommandStatus,
        params: &CommandParams,
        state: Option<Value>,
        reason: Option<String>,
    ) -> CommandResult {
        CommandResult {
            status,
            device: params.device_id.clone(),
            capability: params.capability.clone(),
            state,
            reason,
            simulated: false,
        }
    }

    fn audit(&self, params: &CommandParams, result: &CommandResult) {
        tracing::info!(
            request_id = %params.request_id,
            device = %params.device_id,
            capability = %params.capability,
            status = ?result.status,
            reason = ?result.reason,
            "home command"
        );
        let mut audit = self.audit.lock().unwrap_or_else(|e| e.into_inner());
        audit.push_back(AuditEntry {
            request_id: params.request_id.clone(),
            device: params.device_id.clone(),
            capability: params.capability.clone(),
            status: result.status,
            reason: result.reason.clone(),
            timestamp_ms: now_ms(),
        });
        while audit.len() > 1024 {
            audit.pop_front();
        }
    }
}

fn map_apply_error(error: &HomeError) -> (CommandStatus, String) {
    match error {
        HomeError::Denied(_) | HomeError::NeedsGrant(_) | HomeError::RateLimited(_) => {
            (CommandStatus::Denied, error.to_string())
        }
        HomeError::UnknownDevice(_)
        | HomeError::UnknownCapability { .. }
        | HomeError::Publish(_)
        | HomeError::Config(_) => (CommandStatus::Failed, error.to_string()),
    }
}

#[async_trait]
impl Backend for RealHome {
    fn simulated(&self) -> bool {
        false
    }

    async fn devices(&self, room: Option<&str>, kind: Option<&str>) -> Vec<Device> {
        self.home
            .list()
            .into_iter()
            .filter(|view| {
                room.is_none_or(|room| {
                    view.room
                        .as_deref()
                        .is_some_and(|actual| room_matches(actual, room))
                }) && kind.is_none_or(|kind| view.kind.eq_ignore_ascii_case(kind))
            })
            .map(|view| device_from_view(&self.home, view))
            .collect()
    }

    async fn state(&self, device_id: &str) -> Result<Device, String> {
        self.home
            .get(device_id)
            .map(|view| device_from_view(&self.home, view))
            .map_err(|error| error.to_string())
    }

    async fn command(&self, params: &CommandParams) -> CommandResult {
        let result = self.accept(params).await;
        self.audit(params, &result);
        result
    }

    async fn scenes(&self) -> Vec<String> {
        Vec::new()
    }

    async fn scene_run(&self, params: &SceneParams) -> CommandResult {
        CommandResult {
            status: CommandStatus::Denied,
            device: String::new(),
            capability: String::new(),
            state: None,
            reason: Some(format!(
                "scene {} is not configured; wb-rules scenes are not exposed yet",
                params.name
            )),
            simulated: false,
        }
    }

    async fn events(&self, params: &EventsParams) -> EventsPage {
        let log = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if params.epoch.is_some_and(|epoch| epoch != log.epoch) {
            return EventsPage {
                epoch: log.epoch,
                cursor_reset: true,
                events: Vec::new(),
                next_cursor: log.next_cursor,
            };
        }
        let limit = params.limit.unwrap_or(50).min(200);
        let events: Vec<_> = log
            .events
            .iter()
            .filter(|event| event.cursor > params.cursor)
            .take(limit)
            .cloned()
            .collect();
        EventsPage {
            epoch: log.epoch,
            cursor_reset: false,
            next_cursor: events
                .last()
                .map(|event| event.cursor)
                .unwrap_or(params.cursor),
            events,
        }
    }

    async fn health(&self) -> Value {
        let devices = self.home.list();
        let available = devices.iter().filter(|device| device.available).count();
        let log = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let audit = self.audit.lock().unwrap_or_else(|e| e.into_inner());
        let connected = self.status.connected();
        json!({
            "status": if connected { "ok" } else { "degraded" },
            "backend": "mqtt",
            "broker": if connected { "connected" } else { "disconnected" },
            "rules": "external",
            "devices": devices.len(),
            "available": available,
            "epoch": log.epoch,
            "next_cursor": log.next_cursor,
            "ack_timeout_ms": self.ack_timeout.as_millis(),
            "audit_entries": audit.len(),
            "last_audit": audit.back(),
            "last_error": self.status.last_error(),
        })
    }
}

fn device_from_view(home: &Home, view: DeviceView) -> Device {
    let config = home
        .config()
        .devices
        .iter()
        .find(|device| device.id == view.id);
    let mut state = Map::new();
    let mut capabilities = Vec::with_capacity(view.capabilities.len());
    let mut writable = Vec::new();
    let mut updated_ms = 0;

    for reported in &view.capabilities {
        let capability = config.and_then(|device| device.capability(&reported.name));
        if reported.writable {
            writable.push(reported.name.clone());
        }
        if let Some(raw) = &reported.value {
            state.insert(reported.name.clone(), capability_value(capability, raw));
        }
        if let Some(at) = reported.received_at {
            updated_ms = updated_ms.max(at.timestamp_millis().max(0) as u64);
        }
        capabilities.push(CapabilityInfo {
            name: reported.name.clone(),
            kind: reported.kind.clone(),
            risk: reported.risk,
            min: capability.and_then(|capability| capability.min),
            max: capability.and_then(|capability| capability.max),
            unit: capability.and_then(|capability| capability.unit.clone()),
        });
    }

    Device {
        id: view.id,
        room: view.room.unwrap_or_default(),
        kind: view.kind,
        state: Value::Object(state),
        updated_ms,
        stale: view.stale,
        available: view.available,
        error: view.error,
        writable,
        capabilities,
    }
}

fn state_values(home: &Home, view: &DeviceView) -> Value {
    let config = home
        .config()
        .devices
        .iter()
        .find(|device| device.id == view.id);
    let mut state = Map::new();
    for reported in &view.capabilities {
        if let Some(raw) = &reported.value {
            let capability = config.and_then(|device| device.capability(&reported.name));
            state.insert(reported.name.clone(), capability_value(capability, raw));
        }
    }
    Value::Object(state)
}

fn capability_value(capability: Option<&Capability>, raw: &str) -> Value {
    let Some(capability) = capability else {
        return Value::String(raw.to_owned());
    };
    match capability.kind.as_str() {
        "bool" => bool_value(capability, raw)
            .map(Value::Bool)
            .unwrap_or_else(|| Value::String(raw.to_owned())),
        "number" => raw
            .parse::<f64>()
            .map(|value| json!(value))
            .unwrap_or_else(|_| Value::String(raw.to_owned())),
        _ => Value::String(raw.to_owned()),
    }
}

fn bool_value(capability: &Capability, raw: &str) -> Option<bool> {
    let raw = raw.trim();
    let on = capability.payload_on.as_deref().unwrap_or("1");
    let off = capability.payload_off.as_deref().unwrap_or("0");
    if raw == on {
        return Some(true);
    }
    if raw == off {
        return Some(false);
    }
    parse_bool_str(raw)
}

fn value_for(home: &Home, device_id: &str, capability_name: &str, raw: &str) -> Value {
    let capability = home
        .config()
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .and_then(|device| device.capability(capability_name));
    capability_value(capability, raw)
}

fn current_values(home: &Home) -> HashMap<(String, String), String> {
    let mut values = HashMap::new();
    for device in home.list() {
        for capability in device.capabilities {
            if let Some(value) = capability.value {
                values.insert((device.id.clone(), capability.name), value);
            }
        }
    }
    values
}

fn spawn_event_log(
    home: Arc<Home>,
    log: Arc<Mutex<EventLog>>,
    mut last: HashMap<(String, String), String>,
    mut changes: tokio::sync::broadcast::Receiver<Change>,
) {
    tokio::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(Change::Value {
                    device_id,
                    capability,
                    value,
                    received_at,
                    ..
                }) => {
                    let key = (device_id.clone(), capability.clone());
                    if last.get(&key).is_some_and(|previous| previous == &value) {
                        continue;
                    }
                    last.insert(key, value.clone());
                    let event = HomeEvent {
                        cursor: 0,
                        kind: "home.state_changed".into(),
                        device: device_id.clone(),
                        capability: capability.clone(),
                        value: value_for(&home, &device_id, &capability, &value),
                        timestamp_ms: received_at.timestamp_millis().max(0) as u64,
                    };
                    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                    let event = HomeEvent {
                        cursor: log.next_cursor,
                        ..event
                    };
                    log.next_cursor += 1;
                    log.events.push_back(event);
                    while log.events.len() > 1024 {
                        log.events.pop_front();
                    }
                }
                Ok(Change::Error { .. }) => {}
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "home event log lagged; resynchronising");
                    last = current_values(&home);
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use orbita_home::{
        Capability, Clock, DeviceConfig, HomeConfig, HomeError, MqttConfig, Publisher, SystemClock,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::{EventsParams, parse_action};

    #[derive(Default)]
    struct LoopbackPublisher {
        home: StdMutex<Option<Arc<Home>>>,
        published: StdMutex<Vec<(String, Vec<u8>, bool)>>,
        ingest: bool,
        delay: Duration,
    }

    #[async_trait]
    impl Publisher for LoopbackPublisher {
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
            if !self.ingest {
                return Ok(());
            }
            let state_topic = topic.strip_suffix("/on").unwrap_or(topic).to_owned();
            let raw = String::from_utf8_lossy(payload).into_owned();
            let raw = if raw.parse::<f64>().is_ok() && !raw.contains('.') {
                format!("{raw}.0")
            } else {
                raw
            };
            let home = self.home.lock().unwrap().clone();
            let delay = self.delay;
            if let Some(home) = home {
                tokio::spawn(async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    home.ingest(&state_topic, raw.as_bytes());
                });
            }
            Ok(())
        }
    }

    fn config(risk: Risk, kind: &str) -> HomeConfig {
        let (name, state_topic, command_topic, min, max) = if kind == "number" {
            (
                "brightness",
                "/devices/wb/controls/light/brightness",
                "/devices/wb/controls/light/brightness",
                Some(0.0),
                Some(100.0),
            )
        } else {
            (
                "on_off",
                "/devices/wb/controls/light",
                "/devices/wb/controls/light/on",
                None,
                None,
            )
        };
        HomeConfig {
            mqtt: MqttConfig {
                url: "mqtt://127.0.0.1:1883".to_owned(),
                client_id: "test".to_owned(),
                username: None,
                password: None,
            },
            devices: vec![DeviceConfig {
                id: "light".to_owned(),
                room: Some("кухня".to_owned()),
                kind: "light".to_owned(),
                integration: "wirenboard".to_owned(),
                external_id: "wb/1".to_owned(),
                aliases: vec![],
                meta_error_topic: Some("/devices/wb/meta/error".to_owned()),
                capabilities: vec![Capability {
                    name: name.to_owned(),
                    kind: kind.to_owned(),
                    min,
                    max,
                    unit: None,
                    values: vec![],
                    readable: true,
                    writable: true,
                    risk,
                    state_topic: Some(state_topic.to_owned()),
                    command_topic: Some(command_topic.to_owned()),
                    payload_on: None,
                    payload_off: None,
                    stale_after_ms: None,
                    feedback: None,
                }],
            }],
            stale_after_ms: 30_000,
            rate_limit_per_min: 60,
        }
    }

    fn setup(
        risk: Risk,
        kind: &str,
        ingest: bool,
        ack_timeout: Duration,
    ) -> (Arc<RealHome>, Arc<LoopbackPublisher>, Arc<Home>) {
        let publisher = Arc::new(LoopbackPublisher {
            ingest,
            delay: Duration::from_millis(20),
            ..Default::default()
        });
        let home = Home::new(
            Arc::new(config(risk, kind)),
            publisher.clone(),
            Arc::new(SystemClock::new()) as Arc<dyn Clock>,
        )
        .unwrap();
        *publisher.home.lock().unwrap() = Some(Arc::clone(&home));
        let real = Arc::new(RealHome::new(
            Arc::clone(&home),
            Arc::new(MqttStatus::default()),
            ack_timeout,
        ));
        (real, publisher, home)
    }

    fn command(capability: &str, value: Value, request_id: &str) -> CommandParams {
        CommandParams {
            device_id: "light".into(),
            capability: capability.into(),
            value,
            request_id: request_id.into(),
            confirm: None,
        }
    }

    #[tokio::test]
    async fn bool_command_is_confirmed_by_the_state_topic() {
        let (real, publisher, _) = setup(Risk::Safe, "bool", true, Duration::from_millis(500));
        let result = real.command(&command("on_off", json!(true), "r1")).await;
        assert_eq!(result.status, CommandStatus::Executed, "{result:?}");
        assert_eq!(result.state.unwrap()["on_off"], json!(true));
        assert_eq!(publisher.published.lock().unwrap().len(), 1);
        assert!(!publisher.published.lock().unwrap()[0].2);
    }

    #[tokio::test]
    async fn numbers_are_confirmed_across_formatting() {
        let (real, _, _) = setup(Risk::Safe, "number", true, Duration::from_millis(500));
        let result = real.command(&command("brightness", json!(42), "r2")).await;
        assert_eq!(result.status, CommandStatus::Executed, "{result:?}");
        assert_eq!(result.state.unwrap()["brightness"], json!(42.0));
    }

    #[tokio::test]
    async fn missing_state_report_times_out() {
        let (real, _, _) = setup(Risk::Safe, "bool", false, Duration::from_millis(50));
        let result = real.command(&command("on_off", json!(true), "r3")).await;
        assert_eq!(result.status, CommandStatus::Failed);
        assert!(
            result
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no state confirmation"))
        );
    }

    #[tokio::test]
    async fn dangerous_capabilities_are_denied_before_publishing() {
        let (real, publisher, _) = setup(Risk::Dangerous, "bool", true, Duration::from_millis(500));
        let mut params = command("on_off", json!(true), "r4");
        params.confirm = Some("approved".into());
        let result = real.command(&params).await;
        assert_eq!(result.status, CommandStatus::Denied);
        assert!(publisher.published.lock().unwrap().is_empty());

        // There is no grant channel through MCP: confirm="approved" is ignored.
    }

    #[tokio::test]
    async fn health_reports_the_mqtt_backend() {
        let (real, _, _) = setup(Risk::Safe, "bool", false, Duration::from_millis(50));
        let health = real.health().await;
        assert_eq!(health["backend"], "mqtt");
        assert_eq!(health["broker"], "disconnected");
        assert_eq!(health["devices"], 1);
    }

    #[tokio::test]
    async fn mqtt_changes_become_deduplicated_events() {
        let (real, _, home) = setup(Risk::Safe, "bool", false, Duration::from_millis(50));
        home.ingest("/devices/wb/controls/light", b"1");
        tokio::time::sleep(Duration::from_millis(20)).await;
        home.ingest("/devices/wb/controls/light", b"1");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let page = real
            .events(&EventsParams {
                cursor: 0,
                epoch: None,
                limit: None,
            })
            .await;
        assert_eq!(page.events.len(), 1, "{page:?}");
        assert_eq!(page.events[0].value, json!(true));
        assert_eq!(page.next_cursor, 1);
    }

    #[test]
    fn parses_boolean_values_from_tool_calls() {
        for value in [json!(true), json!("on"), json!("1"), json!(1)] {
            assert!(parse_action("bool", &value).is_ok(), "{value}");
        }
        for value in [json!(false), json!("off"), json!("0"), json!(0)] {
            assert!(parse_action("bool", &value).is_ok(), "{value}");
        }
        assert!(parse_action("bool", &json!("maybe")).is_err());
    }
}
