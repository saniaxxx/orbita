//! In-memory backend for shadow runs (`HOME_MCP_SIMULATED=1`) and tests.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

use async_trait::async_trait;
use orbita_home::Risk;
use serde_json::{Value, json};

use crate::{
    Backend, CapabilityInfo, CommandParams, CommandResult, CommandStatus, Device, EventsPage,
    EventsParams, HomeEvent, SceneParams, action_value, now_ms, parse_action, room_matches,
};

#[derive(Debug)]
struct FakeState {
    devices: BTreeMap<String, Device>,
    events: VecDeque<HomeEvent>,
    next_cursor: u64,
    epoch: u64,
}

pub struct FakeHome {
    state: Mutex<FakeState>,
    simulated: bool,
}

impl FakeHome {
    pub fn seeded(simulated: bool) -> Self {
        let now = now_ms();
        let mut devices = BTreeMap::new();
        for (id, room, kind, writable, capabilities, state) in [
            (
                "light_hall",
                "коридор",
                "light",
                vec!["on_off".to_owned()],
                vec![capability("on_off", "bool", Risk::Safe)],
                json!({"on_off": false}),
            ),
            (
                "light_kids",
                "детская",
                "light",
                vec!["on_off".to_owned()],
                vec![capability("on_off", "bool", Risk::Safe)],
                json!({"on_off": false}),
            ),
            (
                "climate_hall",
                "коридор",
                "temperature",
                vec![],
                vec![capability("temperature", "number", Risk::Safe)],
                json!({"temperature": 21.5}),
            ),
            (
                "lock_front",
                "прихожая",
                "lock",
                vec![],
                vec![capability("lock", "bool", Risk::Dangerous)],
                json!({"lock": true}),
            ),
        ] {
            devices.insert(
                id.to_owned(),
                Device {
                    id: id.to_owned(),
                    room: room.to_owned(),
                    kind: kind.to_owned(),
                    state,
                    updated_ms: now,
                    stale: false,
                    available: true,
                    error: None,
                    writable,
                    capabilities,
                },
            );
        }
        Self {
            state: Mutex::new(FakeState {
                devices,
                events: VecDeque::new(),
                next_cursor: 1,
                epoch: now,
            }),
            simulated,
        }
    }
}

fn capability(name: &str, kind: &str, risk: Risk) -> CapabilityInfo {
    CapabilityInfo {
        name: name.to_owned(),
        kind: kind.to_owned(),
        risk,
        min: None,
        max: None,
        unit: None,
    }
}

fn merged_state(current: &Value, capability: &str, typed: Value) -> Value {
    let mut object = current.as_object().cloned().unwrap_or_default();
    object.insert(capability.to_owned(), typed);
    Value::Object(object)
}

fn result(
    status: CommandStatus,
    device_id: &str,
    capability: &str,
    state: Option<Value>,
    reason: Option<String>,
    simulated: bool,
) -> CommandResult {
    CommandResult {
        status,
        device: device_id.to_owned(),
        capability: capability.to_owned(),
        state,
        reason,
        simulated,
    }
}

#[async_trait]
impl Backend for FakeHome {
    fn simulated(&self) -> bool {
        self.simulated
    }

    async fn devices(&self, room: Option<&str>, kind: Option<&str>) -> Vec<Device> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .devices
            .values()
            .filter(|device| {
                room.is_none_or(|room| room_matches(&device.room, room))
                    && kind.is_none_or(|kind| device.kind.eq_ignore_ascii_case(kind))
            })
            .cloned()
            .collect()
    }

    async fn state(&self, device_id: &str) -> Result<Device, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .devices
            .get(device_id)
            .cloned()
            .ok_or_else(|| format!("неизвестное устройство {device_id}"))
    }

    async fn command(&self, params: &CommandParams) -> CommandResult {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(device) = state.devices.get(&params.device_id) else {
            return result(
                CommandStatus::Failed,
                &params.device_id,
                &params.capability,
                None,
                Some("unknown device".into()),
                self.simulated,
            );
        };
        let Some(capability) = device
            .capabilities
            .iter()
            .find(|capability| capability.name == params.capability)
        else {
            return result(
                CommandStatus::Failed,
                &params.device_id,
                &params.capability,
                None,
                Some("unknown capability".into()),
                self.simulated,
            );
        };
        if capability.risk != Risk::Safe {
            return result(
                CommandStatus::Denied,
                &params.device_id,
                &params.capability,
                None,
                Some("dangerous capability is deny-by-default".into()),
                self.simulated,
            );
        }
        if !device
            .writable
            .iter()
            .any(|item| item == &params.capability)
        {
            return result(
                CommandStatus::Denied,
                &params.device_id,
                &params.capability,
                None,
                Some("capability is not writable".into()),
                self.simulated,
            );
        }
        let kind = capability.kind.clone();
        let action = match parse_action(&kind, &params.value) {
            Ok(action) => action,
            Err(reason) => {
                return result(
                    CommandStatus::Denied,
                    &params.device_id,
                    &params.capability,
                    None,
                    Some(reason),
                    self.simulated,
                );
            }
        };
        let typed = action_value(&action);
        let Some(device) = state.devices.get_mut(&params.device_id) else {
            return result(
                CommandStatus::Failed,
                &params.device_id,
                &params.capability,
                None,
                Some("unknown device".into()),
                self.simulated,
            );
        };
        let merged = merged_state(&device.state, &params.capability, typed.clone());
        if self.simulated {
            return result(
                CommandStatus::Simulated,
                &params.device_id,
                &params.capability,
                Some(merged),
                None,
                true,
            );
        }
        let timestamp_ms = now_ms();
        device.state = merged.clone();
        device.updated_ms = timestamp_ms;
        device.stale = false;
        let event = HomeEvent {
            cursor: state.next_cursor,
            kind: "home.state_changed".into(),
            device: params.device_id.clone(),
            capability: params.capability.clone(),
            value: typed,
            timestamp_ms,
        };
        state.next_cursor += 1;
        state.events.push_back(event);
        while state.events.len() > 1024 {
            state.events.pop_front();
        }
        result(
            CommandStatus::Executed,
            &params.device_id,
            &params.capability,
            Some(merged),
            None,
            false,
        )
    }

    async fn scenes(&self) -> Vec<String> {
        Vec::new()
    }

    async fn scene_run(&self, params: &SceneParams) -> CommandResult {
        result(
            CommandStatus::Denied,
            "",
            "",
            None,
            Some(format!(
                "scene {} is not configured; wb-rules scenes are not exposed yet",
                params.name
            )),
            self.simulated,
        )
    }

    async fn events(&self, params: &EventsParams) -> EventsPage {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if params.epoch.is_some_and(|epoch| epoch != state.epoch) {
            return EventsPage {
                epoch: state.epoch,
                cursor_reset: true,
                events: Vec::new(),
                next_cursor: state.next_cursor,
            };
        }
        let limit = params.limit.unwrap_or(50).min(200);
        let events: Vec<_> = state
            .events
            .iter()
            .filter(|event| event.cursor > params.cursor)
            .take(limit)
            .cloned()
            .collect();
        EventsPage {
            epoch: state.epoch,
            cursor_reset: false,
            next_cursor: events
                .last()
                .map(|event| event.cursor)
                .unwrap_or(params.cursor),
            events,
        }
    }

    async fn health(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let available = state
            .devices
            .values()
            .filter(|device| device.available)
            .count();
        json!({
            "status": "ok",
            "backend": "fake",
            "broker": "fake",
            "rules": "fake",
            "devices": state.devices.len(),
            "available": available,
            "epoch": state.epoch,
            "next_cursor": state.next_cursor,
        })
    }
}
