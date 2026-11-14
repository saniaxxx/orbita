//! The agent core as the adapter sees it.
//!
//! [`OpenClawCore`] talks to the OpenClaw Gateway's OpenAI-compatible
//! `/v1/chat/completions`. The adapter offers *client tools* in each request;
//! OpenClaw returns `tool_calls` for them, the adapter executes them and
//! answers with `role: tool` so the agent can finish the turn. Server-side
//! tools (search, TV, home) stay inside OpenClaw via MCP.
//!
//! D-26: volume, player and Bluetooth are device settings handled by the
//! adapter's intent layer; the core never sees them. Time commitments are the
//! core's: `reminder_create` stores a job in the OpenClaw scheduler with
//! delivery to this station or to Telegram.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use orbita_contracts::{DeviceAction, InboundEvent, InputKind};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::schedule::{Commitment, Scheduler, choose_delivery, due_at_local};
use crate::speech::FirstSentence;

/// What the core decided for one turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CoreTurn {
    pub text: String,
    /// Device actions for the interface to render (none are offered today).
    pub actions: Vec<DeviceAction>,
}

#[async_trait]
pub trait Core: Send + Sync {
    async fn turn(&self, event: &InboundEvent) -> Result<CoreTurn>;

    /// Like [`Core::turn`], but may send the first sentence to `first` as soon
    /// as it is ready (D-19). The returned text is still the whole reply and
    /// starts with what was sent. The default never streams.
    async fn turn_streaming(
        &self,
        event: &InboundEvent,
        first: mpsc::UnboundedSender<String>,
    ) -> Result<CoreTurn> {
        drop(first);
        self.turn(event).await
    }
}

/// Shortest first sentence worth sending early: it has to play long enough
/// to hide the synthesis of the rest (~0.9 s per new phrase on the stand).
pub const FIRST_PART_MIN_CHARS: usize = 25;

pub const TOOL_REMINDER_CREATE: &str = "reminder_create";

/// Client tools offered to the core for this turn.
pub fn client_tools(scheduling: bool) -> Vec<Value> {
    if !scheduling {
        return Vec::new();
    }
    vec![json!({"type": "function", "function": {
        "name": TOOL_REMINDER_CREATE,
        "description": "Поставить напоминание или таймер. Укажи либо in_seconds (через сколько секунд), либо at (время HH:MM по Москве). text — о чём напомнить, коротко. channel=telegram, только если пользователь прямо попросил напомнить в телеграм; иначе here (эта колонка).",
        "parameters": {"type": "object", "properties": {
            "in_seconds": {"type": "integer", "minimum": 1, "maximum": 604800},
            "at": {"type": "string", "pattern": "^[0-2]?[0-9]:[0-5][0-9]$"},
            "text": {"type": "string"},
            "channel": {"type": "string", "enum": ["here", "telegram"]}
        }, "required": ["text"]}
    }})]
}

/// Style for answers that will be spoken by TTS on a speaker.
///
/// It must stay the same all day: it goes to the start of the prompt, and any
/// change there throws away the llama prompt cache (8–9k tokens, ~3.5 s of
/// prefill on the stand). The clock goes into the user turn instead, see
/// [`voice_user_message`].
pub fn voice_instructions(now: DateTime<Utc>) -> String {
    format!(
        "Ты отвечаешь голосом через умную колонку; ответ озвучит синтезатор речи. \
         Отвечай по-русски одним-двумя короткими разговорными предложениями. \
         Без списков, markdown, ссылок, эмодзи и названий сайтов. \
         Если искал в интернете — назови сам ответ (например, температуру), а не источники; \
         если ответа в результатах нет, коротко так и скажи. \
         Не заканчивай ответ дежурными вопросами вроде «Чем ещё могу помочь?»: \
         вопрос в конце — только если правда ждёшь ответа, колонка тогда слушает. \
         Таймер или напоминание ставь только вызовом reminder_create; \
         никогда не говори, что поставил или засёк, если не вызвал его. \
         Пометка [сейчас ЧЧ:ММ] в конце реплики — служебное московское время; \
         это не часть реплики: никогда не повторяй и не пересказывай её в ответе. \
         Сегодня {} (Москва).",
        moscow(now).format("%d.%m.%Y")
    )
}

/// The user turn with the current Moscow time appended. The end of the
/// prompt changes every turn anyway, so the clock costs no cache.
pub fn voice_user_message(text: &str, now: DateTime<Utc>) -> String {
    format!("{text} [сейчас {}]", moscow(now).format("%H:%M"))
}

fn moscow(now: DateTime<Utc>) -> DateTime<chrono::FixedOffset> {
    now.with_timezone(&chrono::FixedOffset::east_opt(3 * 3600).expect("valid offset"))
}

/// A validated `reminder_create` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderCall {
    pub due_at: DateTime<Utc>,
    pub text: String,
    pub telegram: bool,
}

/// Parse `reminder_create` arguments; `None` when they make no sense.
pub fn reminder_call(arguments: &str, now: DateTime<Utc>) -> Option<ReminderCall> {
    let arguments: Value = serde_json::from_str(arguments).ok()?;
    let text = arguments.get("text")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let due_at = match (
        arguments.get("in_seconds").and_then(Value::as_u64),
        arguments.get("at").and_then(Value::as_str),
    ) {
        (Some(seconds), _) if (1..=604_800).contains(&seconds) => {
            now + chrono::Duration::seconds(seconds as i64)
        }
        (_, Some(at)) => due_at_local(at, now)?,
        _ => return None,
    };
    Some(ReminderCall {
        due_at,
        text: text.to_owned(),
        telegram: arguments.get("channel").and_then(Value::as_str) == Some("telegram"),
    })
}

#[derive(Debug, Clone)]
pub struct OpenClawConfig {
    /// Gateway base, e.g. `http://127.0.0.1:18789`.
    pub base_url: String,
    pub token: String,
    /// Agent target, `openclaw/default` unless a dedicated agent is set up.
    pub model: String,
    pub timeout: Duration,
    /// Rounds of client tool calls before the turn is cut off.
    pub max_tool_rounds: usize,
}

pub struct OpenClawCore {
    config: OpenClawConfig,
    http: reqwest::Client,
    scheduler: Option<Arc<dyn Scheduler>>,
    telegram_chat: Option<String>,
}

impl OpenClawCore {
    pub fn new(config: OpenClawConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .context("building the OpenClaw HTTP client")?;
        Ok(Self {
            config,
            http,
            scheduler: None,
            telegram_chat: None,
        })
    }

    /// Enable `reminder_create`; `telegram_chat` routes "в телеграм".
    pub fn with_scheduler(
        mut self,
        scheduler: Arc<dyn Scheduler>,
        telegram_chat: Option<String>,
    ) -> Self {
        self.scheduler = Some(scheduler);
        self.telegram_chat = telegram_chat;
        self
    }

    /// Execute one client tool call and return the tool result text.
    async fn run_tool(&self, event: &InboundEvent, name: &str, arguments: &str) -> String {
        if name != TOOL_REMINDER_CREATE {
            tracing::warn!(tool = name, arguments, "unknown client tool call");
            return "error: такого действия нет".to_owned();
        }
        let Some(scheduler) = self.scheduler.as_deref() else {
            return "error: напоминания недоступны".to_owned();
        };
        let Some(call) = reminder_call(arguments, Utc::now()) else {
            tracing::warn!(arguments, "invalid reminder_create arguments");
            return "error: нужно время (in_seconds или at HH:MM) и текст".to_owned();
        };
        let (delivery, note) = choose_delivery(
            call.telegram,
            self.telegram_chat.as_deref(),
            &event.device_id,
        );
        let commitment = Commitment {
            due_at: call.due_at,
            text: format!("Напоминание: {}", call.text),
            delivery,
        };
        match scheduler.schedule(&commitment).await {
            Ok(job) => {
                tracing::info!(%job, due_at = %call.due_at, ?commitment.delivery, "reminder scheduled by the core");
                let moscow = call
                    .due_at
                    .with_timezone(&chrono::FixedOffset::east_opt(3 * 3600).expect("valid offset"));
                format!(
                    "ok: напоминание поставлено на {} по Москве{}",
                    moscow.format("%H:%M"),
                    note.map(|note| format!(". {note}")).unwrap_or_default()
                )
            }
            Err(error) => {
                tracing::warn!(error = format!("{error:#}"), "scheduling failed");
                "error: не удалось поставить напоминание".to_owned()
            }
        }
    }

    async fn complete(&self, event: &InboundEvent, body: &Value) -> Result<Value> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.config.token)
            .header("x-openclaw-session-key", &event.session_key)
            .header("x-openclaw-message-channel", "station")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.context("reading OpenClaw reply")?;
        if !status.is_success() {
            bail!("OpenClaw returned {status}: {}", truncate(&text, 300));
        }
        serde_json::from_str(&text).context("parsing OpenClaw reply")
    }

    /// One streamed round (`stream: true`, SSE). Content deltas go through
    /// `splitter`; the first sentence is sent to `first` once. Returns the
    /// assembled assistant message, as the non-streamed endpoint would.
    async fn complete_streaming(
        &self,
        event: &InboundEvent,
        body: &Value,
        splitter: &mut FirstSentence,
        first: &mpsc::UnboundedSender<String>,
    ) -> Result<Value> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut body = body.clone();
        body["stream"] = json!(true);
        let mut response = self
            .http
            .post(&url)
            .bearer_auth(&self.config.token)
            .header("x-openclaw-session-key", &event.session_key)
            .header("x-openclaw-message-channel", "station")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("OpenClaw returned {status}: {}", truncate(&text, 300));
        }
        let mut stream = SseMessage::default();
        let mut pending = Vec::new();
        while let Some(chunk) = response.chunk().await.context("reading OpenClaw stream")? {
            pending.extend_from_slice(&chunk);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(stream.into_message());
                }
                let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                    tracing::warn!(
                        data = truncate(data, 200),
                        "unparsable OpenClaw stream chunk"
                    );
                    continue;
                };
                if let Some(delta) = stream.apply(&chunk)
                    && let Some(sentence) = splitter.push(&delta)
                {
                    let _ = first.send(sentence);
                }
            }
        }
        Ok(stream.into_message())
    }
}

/// An assistant message assembled from `chat.completion.chunk` deltas.
#[derive(Debug, Default)]
struct SseMessage {
    content: String,
    /// Tool calls by `index`: id, name, accumulated arguments.
    tool_calls: Vec<(String, String, String)>,
}

impl SseMessage {
    /// Apply one chunk; returns its content delta, if any.
    fn apply(&mut self, chunk: &Value) -> Option<String> {
        let delta = chunk.pointer("/choices/0/delta")?;
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(Default::default());
                }
                let slot = &mut self.tool_calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    slot.0 = id.to_owned();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    slot.1.push_str(name);
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    slot.2.push_str(arguments);
                }
            }
        }
        let content = delta.get("content").and_then(Value::as_str)?;
        if content.is_empty() {
            return None;
        }
        self.content.push_str(content);
        Some(content.to_owned())
    }

    fn into_message(self) -> Value {
        let mut message = json!({"role": "assistant", "content": self.content});
        if !self.tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(
                self.tool_calls
                    .into_iter()
                    .map(|(id, name, arguments)| {
                        json!({"id": id, "type": "function",
                               "function": {"name": name, "arguments": arguments}})
                    })
                    .collect(),
            );
        }
        message
    }
}

impl OpenClawCore {
    /// The client-tool loop; streamed when `first` is given.
    async fn run(
        &self,
        event: &InboundEvent,
        first: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<CoreTurn> {
        let mut splitter = FirstSentence::new(FIRST_PART_MIN_CHARS);
        // A streamed reply is everything the model said across rounds, so it
        // always starts with the part already sent.
        let mut spoken = String::new();
        let tools = client_tools(self.scheduler.is_some());
        let mut messages = Vec::new();
        let now = Utc::now();
        if event.input_kind == InputKind::Voice {
            messages.push(json!({"role": "system", "content": voice_instructions(now)}));
            messages.push(json!({"role": "user", "content": voice_user_message(&event.text, now)}));
        } else {
            messages.push(json!({"role": "user", "content": event.text}));
        }
        let actions = Vec::new();

        for _round in 0..=self.config.max_tool_rounds {
            let mut body = json!({
                "model": self.config.model,
                "messages": messages,
            });
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.clone());
                body["tool_choice"] = json!("auto");
            }
            let message = match first {
                Some(first) => {
                    self.complete_streaming(event, &body, &mut splitter, first)
                        .await?
                }
                None => self
                    .complete(event, &body)
                    .await?
                    .pointer("/choices/0/message")
                    .cloned()
                    .context("OpenClaw reply has no choices[0].message")?,
            };
            let content = message.get("content").and_then(Value::as_str).unwrap_or("");
            spoken.push_str(content);
            let calls = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if calls.is_empty() {
                let text = if first.is_some() { &spoken } else { content };
                return Ok(CoreTurn {
                    text: text.trim().to_owned(),
                    actions,
                });
            }

            messages.push(message.clone());
            for call in &calls {
                let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let result = self.run_tool(event, name, arguments).await;
                messages.push(json!({"role": "tool", "tool_call_id": id, "content": result}));
            }
        }
        bail!(
            "OpenClaw kept calling client tools after {} rounds",
            self.config.max_tool_rounds
        )
    }
}

#[async_trait]
impl Core for OpenClawCore {
    async fn turn(&self, event: &InboundEvent) -> Result<CoreTurn> {
        self.run(event, None).await
    }

    async fn turn_streaming(
        &self,
        event: &InboundEvent,
        first: mpsc::UnboundedSender<String>,
    ) -> Result<CoreTurn> {
        self.run(event, Some(&first)).await
    }
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-08T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn reminder_tool_only_with_a_scheduler_and_never_volume() {
        assert!(client_tools(false).is_empty());
        let names: Vec<_> = client_tools(true)
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, vec![TOOL_REMINDER_CREATE]);
    }

    #[test]
    fn voice_instructions_do_not_change_within_a_day() {
        // A per-minute clock in the system prompt invalidated the whole llama
        // prompt cache on every new minute.
        let morning = now();
        let evening = morning + chrono::Duration::hours(11);
        assert_eq!(voice_instructions(morning), voice_instructions(evening));
        assert!(voice_instructions(morning).contains("08.11.2026"));
        assert_eq!(
            voice_user_message("который час?", morning),
            "который час? [сейчас 12:00]"
        );
    }

    #[test]
    fn reminder_calls_are_validated() {
        let call = reminder_call(r#"{"in_seconds":180,"text":" чай "}"#, now()).unwrap();
        assert_eq!(call.due_at, now() + chrono::Duration::minutes(3));
        assert_eq!(call.text, "чай");
        assert!(!call.telegram);
        // 18:30 Moscow = 15:30 UTC.
        let call = reminder_call(
            r#"{"at":"18:30","text":"позвонить","channel":"telegram"}"#,
            now(),
        )
        .unwrap();
        assert_eq!(call.due_at.to_rfc3339(), "2026-11-08T15:30:00+00:00");
        assert!(call.telegram);
        assert_eq!(reminder_call(r#"{"text":"x"}"#, now()), None);
        assert_eq!(
            reminder_call(r#"{"in_seconds":60,"text":"  "}"#, now()),
            None
        );
        assert_eq!(reminder_call(r#"{"at":"25:99","text":"x"}"#, now()), None);
        assert_eq!(reminder_call("not json", now()), None);
    }
}
