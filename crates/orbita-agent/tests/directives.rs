//! Directive-layer contract tests.
//!
//! These tests exercise the channel-independent actions, the deterministic
//! fast paths and the station adapter against the real `prepare` map captured
//! from TS quasar (18 functions).
#![allow(dead_code)]

#[path = "../src/action.rs"]
mod action;
#[path = "../src/fastpath.rs"]
mod fastpath;

#[path = "../src/channel/station.rs"]
mod station;

use action::{Action, SessionContext, VolumeChange};
use fastpath::{action, timer, volume};
use serde_json::{Value, json};
use station::StationSession;

fn prepare() -> Value {
    serde_json::from_str(include_str!("fixtures/prepare_station.json")).unwrap()
}

#[test]
fn parses_the_real_station_prepare_map() {
    let session = StationSession::from_prepare(&prepare()).unwrap();
    assert_eq!(session.len(), 18);
    assert_eq!(session.context().functions.len(), 18);
    assert!(session.has("timer_set"));
    assert!(session.has("smart_speaker_timer_set"));
    assert!(session.has("set_sound_volume"));
    assert!(session.has("player_play"));
    assert!(!session.has("alarm_delete"));
}

#[test]
fn maps_volume_and_timer_actions_to_offered_directives() {
    let session = StationSession::from_prepare(&prepare()).unwrap();

    let louder = session
        .directive(&Action::SetVolume(VolumeChange::Louder))
        .unwrap();
    assert_eq!(louder.name, "smart_speaker_set_sound_volume_louder");
    assert_eq!(louder.arguments, json!({}));

    let quieter = session
        .directive(&Action::SetVolume(VolumeChange::Quieter))
        .unwrap();
    assert_eq!(quieter.name, "smart_speaker_set_sound_volume_quieter");

    let set = session
        .directive(&Action::SetVolume(VolumeChange::Set(4)))
        .unwrap();
    assert_eq!(set.name, "smart_speaker_set_sound_volume");
    assert_eq!(set.arguments, json!({"volume": 4}));

    let timer = session
        .directive(&Action::SetTimer {
            seconds: 300,
            label: None,
        })
        .unwrap();
    assert_eq!(timer.name, "smart_speaker_timer_set");
    assert_eq!(timer.arguments, json!({"durationSeconds": 300}));
}

#[test]
fn drops_unoffered_functions_and_missing_required_arguments() {
    let session = StationSession::from_prepare(&prepare()).unwrap();

    assert!(
        session
            .directive(&Action::Directive {
                name: "alarm_delete".into(),
                arguments: json!({"id": "x"}),
            })
            .is_none()
    );
    assert!(
        session
            .directive(&Action::Directive {
                name: "smart_speaker_set_sound_volume".into(),
                arguments: json!({}),
            })
            .is_none()
    );
    assert!(
        session
            .directive(&Action::Directive {
                name: "bluetooth".into(),
                arguments: json!({"enable": true}),
            })
            .is_some()
    );
}

#[test]
fn no_schedule_field_leaves_the_agent() {
    let session = StationSession::from_prepare(&prepare()).unwrap();
    let directive = session
        .directive(&Action::SetTimer {
            seconds: 60,
            label: None,
        })
        .unwrap();
    let value = serde_json::to_value(&directive).unwrap();
    assert!(value.get("schedule").is_none());
    assert_eq!(value["name"], "smart_speaker_timer_set");
}

#[test]
fn fastpath_reads_volume_commands() {
    assert_eq!(volume("громче"), Some(VolumeChange::Louder));
    assert_eq!(volume("Сделай погромче!"), Some(VolumeChange::Louder));
    assert_eq!(volume("потише"), Some(VolumeChange::Quieter));
    assert_eq!(volume("убавь громкость"), Some(VolumeChange::Quieter));
    assert_eq!(volume("громкость 5"), Some(VolumeChange::Set(5)));
    assert_eq!(
        volume("установи громкость на 7"),
        Some(VolumeChange::Set(7))
    );
    assert_eq!(volume("громкость 50"), Some(VolumeChange::Set(10)));
    assert_eq!(volume("напомни через 5 минут громче"), None);
    assert_eq!(volume("привет"), None);
}

#[test]
fn fastpath_reads_timers() {
    assert_eq!(
        timer("поставь таймер на 5 минут"),
        Some(Action::SetTimer {
            seconds: 300,
            label: None
        })
    );
    assert_eq!(
        timer("таймер 30 секунд"),
        Some(Action::SetTimer {
            seconds: 30,
            label: None
        })
    );
    assert_eq!(
        timer("таймер на час"),
        Some(Action::SetTimer {
            seconds: 3600,
            label: None
        })
    );
    assert_eq!(
        timer("таймер на 2 минуты 30 секунд"),
        Some(Action::SetTimer {
            seconds: 150,
            label: None
        })
    );
    assert_eq!(timer("отмени таймер"), None);
    assert_eq!(timer("напомни через 5 минут"), None);
    assert_eq!(timer("привет"), None);
}

#[test]
fn fastpath_action_prefers_volume_and_leaves_dialogue_alone() {
    assert_eq!(
        action("громкость 3"),
        Some(Action::SetVolume(VolumeChange::Set(3)))
    );
    assert_eq!(
        action("таймер на 10 минут"),
        Some(Action::SetTimer {
            seconds: 600,
            label: None
        })
    );
    assert_eq!(action("расскажи сказку"), None);
}

#[test]
fn session_context_lists_offered_functions() {
    let session = StationSession::from_prepare(&prepare()).unwrap();
    let context: SessionContext = session.context();
    assert!(context.has("timer_set"));
    assert!(!context.has("alarm_delete"));
    let prompt = action::function_prompt(&context);
    assert!(prompt.contains("timer_set"));
    assert!(prompt.contains("set_sound_volume"));
    assert!(prompt.contains("function_calls"));
}
