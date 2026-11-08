//! End-to-end `/process` contract with a fake core, and the OpenClaw client
//! against a fake Gateway. The `prepare` map is the real one captured from
//! TS quasar.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use async_trait::async_trait;
use axum::{Json, Router, http::HeaderMap, routing::post};
use futures_util::{SinkExt, StreamExt};
use orbita_contracts::{DeviceAction, InboundEvent};
use orbita_station_adapter::{
    AppState, Core, CoreTurn, Identity, OpenClawConfig, OpenClawCore, build_router,
    schedule::{Commitment, Delivery, Notifier, Scheduler},
    server::{EMPTY_REPLY, FALLBACK_REPLY},
};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct FakeCore {
    events: Mutex<Vec<InboundEvent>>,
    reply: Mutex<Option<CoreTurn>>,
}

#[async_trait]
impl Core for FakeCore {
    async fn turn(&self, event: &InboundEvent) -> Result<CoreTurn> {
        self.events.lock().unwrap().push(event.clone());
        match self.reply.lock().unwrap().clone() {
            Some(turn) => Ok(turn),
            None => bail!("core is down"),
        }
    }
}

fn prepare() -> Value {
    serde_json::from_str(include_str!("fixtures/prepare_station.json")).unwrap()
}

#[derive(Default)]
struct FakeScheduler {
    jobs: Mutex<Vec<Commitment>>,
}

#[async_trait]
impl Scheduler for FakeScheduler {
    async fn schedule(&self, commitment: &Commitment) -> Result<String> {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(commitment.clone());
        Ok(format!("job-{}", jobs.len()))
    }
}

#[derive(Default)]
struct FakeNotifier {
    pushes: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Notifier for FakeNotifier {
    async fn push(&self, device: &str, text: &str) -> Result<()> {
        self.pushes
            .lock()
            .unwrap()
            .push((device.to_owned(), text.to_owned()));
        Ok(())
    }
}

fn identity() -> Identity {
    Identity {
        rooms: vec![("duid-kids".into(), "детская".into())],
        ..Default::default()
    }
}

async fn serve_state(state: AppState) -> String {
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(state)).await.unwrap();
    });
    format!("{addr}")
}

async fn serve(core: Arc<FakeCore>) -> String {
    let addr = serve_state(AppState::new(core, identity(), "fake")).await;
    format!("ws://{addr}/process")
}

async fn process(url: &str, prepare: Option<Value>, text: &str) -> Value {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    if let Some(functions) = prepare {
        let message = json!({"type": "prepare", "data": {"directiveFunctions": functions}});
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
    let message = json!({"type": "process", "data": {
        "metadata": {"internal": {"deviceId": "duid-kids"}},
        "text": text,
    }});
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("adapter reply timeout")
            .expect("adapter closed the socket")
            .expect("websocket error");
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(value["type"], "partialResponse");
            assert_eq!(value["data"]["finished"], true);
            return value["data"].clone();
        }
    }
}

fn directive_names(data: &Value) -> Vec<String> {
    data["directives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|directive| directive["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn intents_run_locally_without_the_core() {
    let core = Arc::new(FakeCore::default());
    let url = serve(core.clone()).await;

    let louder = process(&url, Some(prepare()), "Алиса, сделай громче, пожалуйста").await;
    assert_eq!(louder["text"], "Делаю громче.");
    assert_eq!(
        directive_names(&louder),
        vec!["smart_speaker_set_sound_volume_louder"]
    );

    let level = process(&url, Some(prepare()), "громкость 4").await;
    assert_eq!(level["directives"][0]["arguments"], json!({"volume": 4}));

    let timer = process(&url, Some(prepare()), "поставь таймер на две минуты").await;
    assert_eq!(timer["text"], "Таймер на 2 минуты.");
    assert_eq!(
        timer["directives"][0]["arguments"],
        json!({"durationSeconds": 120})
    );

    assert!(
        core.events.lock().unwrap().is_empty(),
        "core must not be called"
    );
}

#[tokio::test]
async fn dialogue_goes_to_the_core_with_contract_capabilities() {
    let core = Arc::new(FakeCore::default());
    *core.reply.lock().unwrap() = Some(CoreTurn {
        text: "Привет! Чем помочь?".into(),
        actions: Vec::new(),
    });
    let url = serve(core.clone()).await;

    let data = process(&url, Some(prepare()), "Алиса, привет").await;
    assert_eq!(data["text"], "Привет! Чем помочь?");
    assert!(directive_names(&data).is_empty());

    let events = core.events.lock().unwrap();
    let event = &events[0];
    event.validate().unwrap();
    assert_eq!(event.text, "привет");
    assert_eq!(event.room.as_deref(), Some("детская"));
    assert!(event.session_key.starts_with("station:duid-kids:"));
    assert_eq!(
        event.capabilities.actions,
        vec!["set_volume", "step_volume", "set_timer"]
    );
}

#[tokio::test]
async fn core_device_actions_become_directives() {
    let core = Arc::new(FakeCore::default());
    *core.reply.lock().unwrap() = Some(CoreTurn {
        text: "Поставила таймер для чая.".into(),
        actions: vec![
            DeviceAction::SetTimer {
                seconds: 180,
                label: Some("чай".into()),
            },
            // Not offered by the station: dropped, not forwarded.
            DeviceAction::ShowText { text: "x".into() },
        ],
    });
    let url = serve(core).await;

    let data = process(
        &url,
        Some(prepare()),
        "поставь что-нибудь для чая на три минутки",
    )
    .await;
    assert_eq!(directive_names(&data), vec!["smart_speaker_timer_set"]);
    assert_eq!(
        data["directives"][0]["arguments"],
        json!({"durationSeconds": 180})
    );
}

#[tokio::test]
async fn without_prepare_intents_fall_through_and_errors_degrade() {
    let core = Arc::new(FakeCore::default());
    let url = serve(core.clone()).await;

    // No functions offered: the station cannot change volume, so the core
    // decides (here it is down) and the user hears the fallback phrase.
    let data = process(&url, None, "громче").await;
    assert_eq!(data["text"], FALLBACK_REPLY);
    assert!(directive_names(&data).is_empty());
    assert!(
        core.events.lock().unwrap()[0]
            .capabilities
            .actions
            .is_empty()
    );

    let empty = process(&url, Some(prepare()), "  ").await;
    assert_eq!(empty["text"], EMPTY_REPLY);
    assert_eq!(core.events.lock().unwrap().len(), 1);
}

// --- Time commitments (D-26) -------------------------------------------------

#[tokio::test]
async fn reminders_and_timers_go_to_the_core_scheduler() {
    let core = Arc::new(FakeCore::default());
    let scheduler = Arc::new(FakeScheduler::default());
    let mut state = AppState::new(core.clone(), identity(), "fake");
    state.scheduler = Some(scheduler.clone());
    let url = format!("ws://{}/process", serve_state(state).await);

    let timer = process(&url, Some(prepare()), "поставь таймер на две минуты").await;
    assert_eq!(timer["text"], "Таймер на 2 минуты.");
    assert!(
        directive_names(&timer).is_empty(),
        "no native station timer"
    );

    let reminder = process(
        &url,
        Some(prepare()),
        "Алиса, напомни через 20 минут выключить духовку",
    )
    .await;
    assert_eq!(
        reminder["text"],
        "Напомню через 20 минут. выключить духовку."
    );

    // Telegram asked but not connected: delivered here, and the user is told.
    let telegram = process(
        &url,
        Some(prepare()),
        "напомни через час в телеграм купить хлеб",
    )
    .await;
    assert!(
        telegram["text"]
            .as_str()
            .unwrap()
            .starts_with("Телеграм пока не подключён")
    );

    let jobs = scheduler.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].text, "Таймер на 2 минуты истёк.");
    assert_eq!(jobs[1].text, "Напоминание: выключить духовку");
    assert_eq!(jobs[2].text, "Напоминание: купить хлеб");
    for job in jobs.iter() {
        assert_eq!(
            job.delivery,
            Delivery::Station {
                device: "duid-kids".into()
            }
        );
    }
    assert!(
        core.events.lock().unwrap().is_empty(),
        "no LLM for commitments"
    );
}

#[tokio::test]
async fn telegram_reminders_are_announced_when_connected() {
    let scheduler = Arc::new(FakeScheduler::default());
    let mut state = AppState::new(Arc::new(FakeCore::default()), identity(), "fake");
    state.scheduler = Some(scheduler.clone());
    state.telegram_chat = Some("42".into());
    let url = format!("ws://{}/process", serve_state(state).await);

    let data = process(
        &url,
        None,
        "напомни в телеграм через 5 минут позвонить маме",
    )
    .await;
    assert_eq!(data["text"], "Напомню через 5 минут. позвонить маме.");
    let jobs = scheduler.jobs.lock().unwrap();
    assert_eq!(
        jobs[0].delivery,
        Delivery::Telegram {
            chat_id: "42".into()
        }
    );
}

#[tokio::test]
async fn outbound_requires_the_key_and_pushes_the_summary() {
    let notifier = Arc::new(FakeNotifier::default());
    let mut state = AppState::new(Arc::new(FakeCore::default()), identity(), "fake");
    state.notifier = Some(notifier.clone());
    state.outbound_key = Some("secret".into());
    let base = format!("http://{}", serve_state(state).await);
    let client = reqwest::Client::new();
    let body = json!({"jobId": "j1", "action": "finished", "status": "ok",
        "summary": "Напоминание: проверить чай"});

    let wrong = client
        .post(format!("{base}/outbound?device=duid-kids&key=nope"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);
    assert!(notifier.pushes.lock().unwrap().is_empty());

    let right = client
        .post(format!("{base}/outbound?device=duid-kids&key=secret"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 200);
    assert_eq!(
        notifier.pushes.lock().unwrap().as_slice(),
        &[(
            "duid-kids".to_owned(),
            "Напоминание: проверить чай".to_owned()
        )]
    );
}

// --- OpenClaw client against a fake Gateway ---------------------------------

#[derive(Default)]
struct Gateway {
    requests: Mutex<Vec<(HeaderMap, Value)>>,
}

async fn fake_gateway(gateway: Arc<Gateway>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let gateway = gateway.clone();
            async move {
                let round = {
                    let mut requests = gateway.requests.lock().unwrap();
                    requests.push((headers, body));
                    requests.len()
                };
                Json(if round == 1 {
                    json!({"choices": [{"finish_reason": "tool_calls", "message": {
                        "role": "assistant", "content": "",
                        "tool_calls": [{"id": "call_1", "type": "function", "function": {
                            "name": "reminder_create",
                            "arguments": "{\"in_seconds\": 300, \"text\": \"достать пиццу\"}"
                        }}]
                    }}]})
                } else {
                    json!({"choices": [{"finish_reason": "stop", "message": {
                        "role": "assistant", "content": "Напомню через пять минут."
                    }}]})
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn openclaw_client_tools_round_trip() {
    let gateway = Arc::new(Gateway::default());
    let base_url = fake_gateway(gateway.clone()).await;
    let scheduler = Arc::new(FakeScheduler::default());
    let core = OpenClawCore::new(OpenClawConfig {
        base_url,
        token: "secret".into(),
        model: "openclaw/default".into(),
        timeout: Duration::from_secs(5),
        max_tool_rounds: 2,
    })
    .unwrap()
    .with_scheduler(scheduler.clone(), None);

    let identity = Identity::default();
    let functions = orbita_station_adapter::StationFunctions::from_prepare(&prepare()).unwrap();
    let event = orbita_station_adapter::server::inbound_event(
        &identity,
        &functions,
        "duid-1",
        "через пять минуток скажи достать пиццу",
    );
    let started = chrono::Utc::now();
    let turn = core.turn(&event).await.unwrap();
    assert_eq!(turn.text, "Напомню через пять минут.");
    assert!(turn.actions.is_empty());

    let jobs = scheduler.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].text, "Напоминание: достать пиццу");
    assert_eq!(
        jobs[0].delivery,
        Delivery::Station {
            device: "duid-1".into()
        }
    );
    let delay = (jobs[0].due_at - started).num_seconds();
    assert!((299..=301).contains(&delay), "due in ~300 s, got {delay}");

    let requests = gateway.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let (headers, first) = &requests[0];
    assert_eq!(headers["authorization"], "Bearer secret");
    assert!(
        headers["x-openclaw-session-key"]
            .to_str()
            .unwrap()
            .starts_with("station:duid-1:")
    );
    assert_eq!(first["model"], "openclaw/default");
    // Voice turns carry the spoken-style instructions first.
    assert_eq!(first["messages"][0]["role"], "system");
    assert_eq!(first["messages"][1]["role"], "user");
    let tools: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    // D-26: no volume tools; the only client tool is the scheduler.
    assert_eq!(tools, vec!["reminder_create"]);
    // The follow-up answers the client tool call so the agent can finish.
    let (_, second) = &requests[1];
    let messages = second["messages"].as_array().unwrap();
    let tool_message = messages.last().unwrap();
    assert_eq!(tool_message["role"], "tool");
    assert_eq!(tool_message["tool_call_id"], "call_1");
    assert!(tool_message["content"].as_str().unwrap().starts_with("ok:"));
}

// --- Streaming (D-19) ---------------------------------------------------------

/// A core that says its first sentence early, then finishes later.
struct StreamingCore {
    first: &'static str,
    full: &'static str,
}

#[async_trait]
impl Core for StreamingCore {
    async fn turn(&self, _event: &InboundEvent) -> Result<CoreTurn> {
        Ok(CoreTurn {
            text: self.full.into(),
            actions: Vec::new(),
        })
    }

    async fn turn_streaming(
        &self,
        _event: &InboundEvent,
        first: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<CoreTurn> {
        first.send(self.first.into()).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.turn(_event).await
    }
}

/// All `partialResponse` messages of one utterance, up to the finished one.
async fn process_all(url: &str, text: &str) -> Vec<(Value, Duration)> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let message = json!({"type": "process", "data": {
        "metadata": {"internal": {"deviceId": "duid-kids"}}, "text": text,
    }});
    let started = std::time::Instant::now();
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
    let mut out = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("adapter reply timeout")
            .expect("adapter closed the socket")
            .expect("websocket error");
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(value["type"], "partialResponse");
            let finished = value["data"]["finished"] == true;
            out.push((value["data"].clone(), started.elapsed()));
            if finished {
                return out;
            }
        }
    }
}

#[tokio::test]
async fn streamed_core_reply_sends_the_first_sentence_early() {
    let core = Arc::new(StreamingCore {
        first: "Я голосовой ассистент на колонке.",
        full: "Я голосовой ассистент на колонке. Умею **искать** и включать мультики.",
    });
    let mut state = AppState::new(core, identity(), "fake");
    state.stream_replies = true;
    let url = format!("ws://{}/process", serve_state(state).await);
    let parts = process_all(&url, "Алиса, ты кто?").await;
    assert_eq!(parts.len(), 2, "{parts:?}");
    assert_eq!(parts[0].0["finished"], false);
    assert_eq!(parts[0].0["text"], "Я голосовой ассистент на колонке.");
    assert!(parts[0].0.get("shouldListen").is_none());
    // The first part does not wait for the core to finish.
    assert!(parts[0].1 < Duration::from_millis(120), "{:?}", parts[0].1);
    assert_eq!(parts[1].0["finished"], true);
    assert_eq!(parts[1].0["shouldListen"], false);
    // Only the rest, without markup, and nothing repeated.
    assert_eq!(parts[1].0["text"], "Умею искать и включать мультики.");
}

#[tokio::test]
async fn without_streaming_the_reply_is_one_message() {
    let core = Arc::new(StreamingCore {
        first: "Я голосовой ассистент на колонке.",
        full: "Я голосовой ассистент на колонке. Умею искать.",
    });
    let url = format!(
        "ws://{}/process",
        serve_state(AppState::new(core, identity(), "fake")).await
    );
    let parts = process_all(&url, "ты кто?").await;
    assert_eq!(parts.len(), 1);
    assert_eq!(
        parts[0].0["text"],
        "Я голосовой ассистент на колонке. Умею искать."
    );
}

#[tokio::test]
async fn intents_are_never_streamed() {
    let core = Arc::new(StreamingCore {
        first: "не должно прозвучать",
        full: "не должно прозвучать",
    });
    let mut state = AppState::new(core, identity(), "fake");
    state.stream_replies = true;
    let url = format!("ws://{}/process", serve_state(state).await);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    for message in [
        json!({"type": "prepare", "data": {"directiveFunctions": prepare()}}),
        json!({"type": "process", "data": {"metadata": {"internal": {}}, "text": "сделай громче"}}),
    ] {
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }
    let first = loop {
        if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
            break serde_json::from_str::<Value>(text.as_str()).unwrap();
        }
    };
    assert_eq!(first["data"]["finished"], true);
    assert_eq!(
        directive_names(&first["data"]),
        vec!["smart_speaker_set_sound_volume_louder"]
    );
}

/// A Gateway that streams SSE: round 1 a client tool call, round 2 text.
async fn streaming_gateway(rounds: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let rounds = rounds.clone();
            async move {
                assert_eq!(body["stream"], true);
                let round = {
                    let mut rounds = rounds.lock().unwrap();
                    rounds.push(body);
                    rounds.len()
                };
                let chunk = |delta: Value, finish: Value| {
                    format!(
                        "data: {}\n\n",
                        json!({"object": "chat.completion.chunk",
                               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
                    )
                };
                let mut sse = chunk(json!({"role": "assistant"}), Value::Null);
                if round == 1 {
                    sse += &chunk(
                        json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                                "function": {"name": "reminder_create", "arguments": ""}}]}),
                        Value::Null,
                    );
                    sse += &chunk(
                        json!({"tool_calls": [{"index": 0, "function": {
                            "arguments": "{\"in_seconds\": 60, \"text\": \"чай\"}"}}]}),
                        Value::Null,
                    );
                    sse += &chunk(json!({}), json!("tool_calls"));
                } else {
                    for token in [
                        "Поставила",
                        " напоминание",
                        " через",
                        " минуту.",
                        " Не",
                        " забудь",
                        " про",
                        " чай.",
                    ] {
                        sse += &chunk(json!({"content": token}), Value::Null);
                    }
                    sse += &chunk(json!({}), json!("stop"));
                }
                sse += "data: [DONE]\n\n";
                ([("content-type", "text/event-stream")], sse)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn openclaw_streaming_parses_sse_with_client_tools() {
    let rounds = Arc::new(Mutex::new(Vec::new()));
    let scheduler = Arc::new(FakeScheduler::default());
    let core = OpenClawCore::new(OpenClawConfig {
        base_url: streaming_gateway(rounds.clone()).await,
        token: "secret".into(),
        model: "openclaw/default".into(),
        timeout: Duration::from_secs(5),
        max_tool_rounds: 2,
    })
    .unwrap()
    .with_scheduler(scheduler.clone(), None);
    let functions = orbita_station_adapter::StationFunctions::from_prepare(&prepare()).unwrap();
    let event = orbita_station_adapter::server::inbound_event(
        &Identity::default(),
        &functions,
        "duid-1",
        "напомни про чай через минуту",
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let turn = core.turn_streaming(&event, tx).await.unwrap();
    assert_eq!(
        turn.text,
        "Поставила напоминание через минуту. Не забудь про чай."
    );
    assert_eq!(
        rx.recv().await.as_deref(),
        Some("Поставила напоминание через минуту.")
    );
    assert_eq!(rx.recv().await, None);
    assert_eq!(scheduler.jobs.lock().unwrap().len(), 1);
    let rounds = rounds.lock().unwrap();
    assert_eq!(rounds.len(), 2);
    let tool_message = rounds[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(tool_message["role"], "tool");
    assert_eq!(tool_message["tool_call_id"], "call_1");
}

#[tokio::test]
async fn a_closing_question_keeps_the_dialog_open_for_the_answer() {
    let core = Arc::new(FakeCore::default());
    *core.reply.lock().unwrap() = Some(CoreTurn {
        text: "Жил-был кот. Хочешь ещё сказку?".into(),
        actions: Vec::new(),
    });
    let url = serve(core.clone()).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut replies = Vec::new();
    for text in ["расскажи сказку", "да, давай"] {
        let message = json!({"type": "process", "data": {
            "metadata": {"internal": {"deviceId": "duid-kids"}}, "text": text,
        }});
        socket
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
        let reply = loop {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("adapter reply timeout")
                .expect("adapter closed the socket")
                .expect("websocket error");
            if let Message::Text(text) = message {
                break serde_json::from_str::<Value>(text.as_str()).unwrap();
            }
        };
        replies.push(reply);
    }
    assert_eq!(replies[0]["data"]["shouldListen"], true);
    assert_eq!(replies[1]["data"]["finished"], true);
    // The answer reached the core on the same processor connection.
    let events = core.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].text, "да, давай");
}
