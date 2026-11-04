//! Agent request handling: reminders, device fast paths and LLM dialogue.
//!
//! The agent core is channel-independent: it returns an [`Outcome`] with text
//! and semantic actions. A channel adapter turns actions into its protocol
//! (directives for the station).

use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;
use tracing::warn;

use crate::action::{
    Action, Outcome, SessionContext, SessionKey, VolumeChange, normalize_function_name,
};
use crate::db::Store;
use crate::fastpath;
use crate::llm::{LlmClient, Message};
use crate::mcp::McpRegistry;
use crate::reminder;

pub struct Agent {
    store: Arc<Store>,
    llm: Arc<dyn LlmClient>,
    system_prompt: String,
    history_limit: usize,
    mcp: Option<Arc<McpRegistry>>,
    home: Option<Arc<crate::home::HomeService>>,
}

impl Agent {
    pub fn new(
        store: Arc<Store>,
        llm: Arc<dyn LlmClient>,
        system_prompt: impl Into<String>,
        history_limit: usize,
    ) -> Self {
        Self {
            store,
            llm,
            system_prompt: system_prompt.into(),
            history_limit,
            mcp: None,
            home: None,
        }
    }

    pub fn with_mcp(mut self, mcp: Arc<McpRegistry>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    pub fn with_home(mut self, home: Arc<crate::home::HomeService>) -> Self {
        self.home = Some(home);
        self
    }

    pub fn home(&self) -> Option<&Arc<crate::home::HomeService>> {
        self.home.as_ref()
    }

    /// Warm the provider connection and its prompt cache before the first
    /// real utterance (the first call after a restart can take seconds).
    pub async fn warm_up(&self) {
        let messages = [
            Message::system("Ответь одним словом."),
            Message::user("ping"),
        ];
        match self.llm.complete(&messages).await {
            Ok(_) => tracing::info!("llm warmup done"),
            Err(error) => tracing::warn!(%error, "llm warmup failed"),
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub async fn process(
        &self,
        text: &str,
        session: &SessionKey,
        context: &SessionContext,
    ) -> Result<Outcome, String> {
        self.process_at(text, session, Utc::now(), context).await
    }

    pub async fn process_at(
        &self,
        text: &str,
        session: &SessionKey,
        now: DateTime<Utc>,
        context: &SessionContext,
    ) -> Result<Outcome, String> {
        let text = strip_wake_word(text);
        if text.is_empty() {
            // ASR produced nothing (cold start, noise): answer politely instead
            // of failing the dialog.
            return Ok(Outcome::text("Я вас не расслышала. Повторите, пожалуйста."));
        }

        let lower = text.to_lowercase();
        // TV commands are deterministic and must not fall through to the
        // home fast path: «включи телевизор в детской» used to toggle the
        // children's room light because it contains «включи» and «детская».
        if let Some(mcp) = self.mcp.as_deref()
            && mcp.has_tool("tv_state")
            && let Some(intent) = parse_tv_intent(&lower)
        {
            let (tool, arguments) = intent.call(text);
            let started = std::time::Instant::now();
            return match mcp.call(tool, &arguments).await {
                Ok(reply) => {
                    tracing::info!(tool, ms = started.elapsed().as_millis(), "tv fast path");
                    Ok(Outcome::text(reply))
                }
                Err(error) => {
                    tracing::warn!(%error, tool, "tv fast path failed");
                    Ok(Outcome::text(
                        "Не получилось: телевизор недоступен.".to_owned(),
                    ))
                }
            };
        }

        if let Some(home) = self.home.as_deref()
            && !mentions_non_light_device(&lower)
        {
            if asks_about_lights(&lower) {
                return Ok(Outcome::text(home.status_line()));
            }
            if let Some(command) = home.parse(text) {
                return match home.execute(&command).await {
                    Ok(crate::home::HomeOutcome::Done(reply))
                    | Ok(crate::home::HomeOutcome::NeedsConfirmation { reply }) => {
                        Ok(Outcome::text(reply))
                    }
                    Err(error) => {
                        tracing::warn!(%error, "home command failed");
                        Ok(Outcome::text(format!("Не получилось: {error}")))
                    }
                };
            }
        }

        if let Some(parsed) = reminder::parse(text, now) {
            self.store.insert_task(
                "reminder",
                &parsed.text,
                Some(&session.principal),
                parsed.due_at,
            )?;
            self.store.audit(
                "reminder.created",
                &format!(
                    "session={} due={}",
                    session.history_id(),
                    parsed.due_at.to_rfc3339()
                ),
            )?;
            return Ok(Outcome::text(parsed.reply));
        }

        // Deterministic device commands: no LLM, immediate reaction.
        if let Some(action) = fastpath::action(text)
            && action_supported(&action, context)
        {
            return Ok(Outcome::text(action_reply(&action)).with_action(action));
        }

        let history_id = session.history_id();

        // Search intents take a deterministic route: providers are not reliable
        // at choosing tools on their own, so the agent calls search itself and
        // the model only words the answer.
        if let Some(mcp) = self.mcp.as_deref()
            && mcp.has_tool("search_search")
            && is_search_intent(text)
        {
            let started = std::time::Instant::now();
            match mcp
                .call("search_search", &serde_json::json!({"query": text}))
                .await
            {
                Ok(results) => {
                    tracing::info!(
                        ms = started.elapsed().as_millis(),
                        bytes = results.len(),
                        "search fast path"
                    );
                    let mut messages =
                        vec![Message::system(self.system_prompt_with_time(now, context))];
                    // Search answers are self-contained: a short history saves
                    // a lot of provider latency.
                    for message in self.store.messages(&history_id, 4)? {
                        messages.push(Message {
                            role: message.role,
                            content: message.content,
                        });
                    }
                    messages.push(Message::user(text));
                    messages.push(Message::tool(format!(
                        "Результаты поиска (используй их в ответе, не говори, что нет доступа к интернету):
{results}"
                    )));
                    let llm_started = std::time::Instant::now();
                    let raw = self.llm.complete(&messages).await?;
                    tracing::info!(
                        pass = "search-answer",
                        ms = llm_started.elapsed().as_millis(),
                        messages = messages.len(),
                        "llm pass"
                    );
                    let (reply, actions) = parse_reply(&raw, context, None);
                    let reply = strip_source_note(reply.trim());
                    if reply.is_empty() {
                        return Err("LLM returned an empty answer after search".to_owned());
                    }
                    self.store.append_message(&history_id, "user", text)?;
                    self.store
                        .append_message(&history_id, "assistant", &reply)?;
                    self.store
                        .audit("dialogue.search", &format!("session={history_id}"))?;
                    return Ok(Outcome {
                        text: reply,
                        actions,
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "search fast path failed");
                }
            }
        }

        let mut messages = vec![Message::system(self.system_prompt_with_time(now, context))];
        for message in self.store.messages(&history_id, self.history_limit)? {
            messages.push(Message {
                role: message.role,
                content: message.content,
            });
        }
        messages.push(Message::user(text));

        let mut reply = String::new();
        let mut actions = Vec::new();
        let mut completed = false;
        // A tool result goes back to the model; repeat at most three times so
        // a confused model cannot loop forever.
        for pass in 0..4 {
            let llm_started = std::time::Instant::now();
            let raw = self.llm.complete(&messages).await?;
            tracing::info!(
                pass,
                ms = llm_started.elapsed().as_millis(),
                messages = messages.len(),
                "llm pass"
            );
            let (text, parsed_actions) = parse_reply(&raw, context, self.mcp.as_deref());
            let mut tool_calls = Vec::new();
            let mut channel_actions = Vec::new();
            for action in parsed_actions {
                match action {
                    Action::ToolCall { .. } => tool_calls.push(action),
                    other => channel_actions.push(other),
                }
            }

            if tool_calls.is_empty() {
                reply = text;
                actions = channel_actions;
                completed = true;
                break;
            }

            messages.push(Message::assistant(raw));
            let mut results = Vec::new();
            for action in tool_calls {
                let Action::ToolCall { name, arguments } = action else {
                    unreachable!("tool_calls only contains ToolCall");
                };
                let started = std::time::Instant::now();
                let home_result = match self.home.as_deref() {
                    Some(home) => home.call_tool(&name, &arguments).await,
                    None => None,
                };
                let result = match home_result {
                    Some(result) => result,
                    None => match self.mcp.as_ref() {
                        Some(mcp) => mcp.call(&name, &arguments).await,
                        None => Err("MCP is not configured".to_owned()),
                    },
                };
                match &result {
                    Ok(value) => tracing::info!(
                        tool = %name,
                        ms = started.elapsed().as_millis(),
                        bytes = value.to_string().len(),
                        "mcp tool call"
                    ),
                    Err(error) => tracing::warn!(
                        tool = %name,
                        ms = started.elapsed().as_millis(),
                        %error,
                        "mcp tool call failed"
                    ),
                }
                results.push(match result {
                    Ok(value) => format!("{name}: {value}"),
                    Err(error) => format!("{name}: ошибка: {error}"),
                });
            }
            messages.push(Message::tool(results.join("\n")));
        }
        if !completed {
            return Err("LLM kept calling tools without a final answer".to_owned());
        }

        let reply = strip_source_note(reply.trim());
        if reply.is_empty() && actions.is_empty() {
            return Err("LLM returned an empty answer".to_owned());
        }

        self.store.append_message(&history_id, "user", text)?;
        if !reply.is_empty() {
            self.store
                .append_message(&history_id, "assistant", &reply)?;
        }
        self.store.audit(
            "dialogue",
            &format!("session={history_id} actions={}", actions.len()),
        )?;
        Ok(Outcome {
            text: reply,
            actions,
        })
    }

    fn system_prompt_with_time(&self, now: DateTime<Utc>, context: &SessionContext) -> String {
        let offset = FixedOffset::east_opt(3 * 60 * 60).expect("Moscow offset");
        let moscow = now.with_timezone(&offset);
        let mut prompt = format!(
            "{}\nСейчас {} (Europe/Moscow).",
            self.system_prompt,
            moscow.format("%d.%m.%Y %H:%M")
        );
        if !context.functions.is_empty() {
            prompt.push('\n');
            prompt.push_str(&crate::action::function_prompt(context));
        }
        if let Some(mcp) = self.mcp.as_deref() {
            let tools = mcp.prompt();
            if !tools.is_empty() {
                prompt.push('\n');
                prompt.push_str(&tools);
            }
        }
        if let Some(home) = self.home.as_deref() {
            prompt.push('\n');
            prompt.push_str(&home.prompt());
        }
        prompt
    }
}

/// Questions about changing facts go to search without asking the model.
fn is_search_intent(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "погод",
        "прогноз",
        "температур",
        "новост",
        "курс ",
        "курса",
        "доллар",
        "евро",
        "цен",
        "стоимост",
        "пробк",
        "что случилось",
    ]
    .iter()
    .any(|word| text.contains(word))
}

/// Drop the wake word the ASR keeps at the start ("Алиса, ..."): the station
/// address is channel noise, and the model behaves better without it.
/// Drop a trailing "Источник: ..." note: it is not meant to be spoken.
fn strip_source_note(text: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let regex = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\s*источник[^.!?]*[.!?]?\s*$").expect("source note regex")
    });
    regex.replace(text, "").trim().to_owned()
}

fn strip_wake_word(text: &str) -> &str {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    for form in ["алиса", "алекса"] {
        if let Some(rest) = lower.strip_prefix(form) {
            let rest = rest.trim_start_matches([' ', ',', '!', '.', ':', ';']);
            let offset = trimmed.len() - rest.len();
            if offset > 0 && !rest.is_empty() {
                return trimmed[offset..].trim();
            }
        }
    }
    trimmed
}

/// Does the utterance ask for the current light state?
///
/// ASR often turns «какой свет включён?» into the statement «Свет включён.»
/// (observed on the stand), so a bare state verb without a command verb also
/// counts as a status question.
fn asks_about_lights(lower: &str) -> bool {
    if lower.contains("какой свет")
        || lower.contains("какие свет")
        || lower.contains("что включено")
        || lower.contains("статус света")
    {
        return true;
    }
    lower.contains("свет")
        && ["включён", "включен", "выключен", "выключён", "горит"]
            .iter()
            .any(|word| lower.contains(word))
}

/// Does the utterance mention a TV at all?
fn mentions_tv(lower: &str) -> bool {
    lower.split(|c: char| !c.is_alphanumeric()).any(|word| {
        word == "тв"
            || word.starts_with("телевизор")
            || word.starts_with("телик")
            || word.starts_with("телек")
            || word.starts_with("сбер")
            || word == "tcl"
            || word == "тцл"
    })
}

/// Does the utterance mention a device that is not a light?
///
/// The home fast path is intentionally simple (verb + room alias), so it must
/// not run when the user talks about another actuator: «включи телевизор в
/// детской» once toggled the children's room light. Unknown devices fall back
/// to the LLM, which is safer than a blind light command.
fn mentions_non_light_device(lower: &str) -> bool {
    if mentions_tv(lower) {
        return true;
    }
    lower.split(|c: char| !c.is_alphanumeric()).any(|word| {
        word.starts_with("вытяжк")
            || word.starts_with("приточк")
            || word.starts_with("вентиляц")
            || word.starts_with("вентилятор")
            || word.starts_with("калорифер")
            || word.starts_with("увлажн")
            || word.starts_with("котел")
            || word.starts_with("котёл")
            || word.starts_with("замок")
            || word.starts_with("замк")
            || word.starts_with("ворот")
            || word.starts_with("калитк")
            || word.starts_with("розетк")
            || word.starts_with("штор")
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvIntent {
    Power(bool),
    Volume(&'static str),
    SetVolume(u32),
    App(String),
    Input(String),
    State,
}

impl TvIntent {
    fn call(&self, text: &str) -> (&'static str, serde_json::Value) {
        let device = text.trim();
        match self {
            Self::Power(on) => ("tv_power", serde_json::json!({"device": device, "on": on})),
            Self::Volume(action) => (
                "tv_volume",
                serde_json::json!({"device": device, "action": action}),
            ),
            Self::SetVolume(level) => (
                "tv_volume",
                serde_json::json!({"device": device, "action": "set", "level": level}),
            ),
            Self::App(app) => ("tv_app", serde_json::json!({"device": device, "app": app})),
            Self::Input(input) => (
                "tv_input",
                serde_json::json!({"device": device, "input": input}),
            ),
            Self::State => ("tv_state", serde_json::json!({"device": device})),
        }
    }
}

/// Parse a TV command without the LLM. The whole utterance is passed as the
/// device reference, so the TV server can pick the room itself.
fn parse_tv_intent(lower: &str) -> Option<TvIntent> {
    if !mentions_tv(lower) {
        return None;
    }
    for (needle, app) in [
        ("youtube", "youtube"),
        ("ютуб", "youtube"),
        ("netflix", "netflix"),
        ("нетфликс", "netflix"),
        ("kinopoisk", "kinopoisk"),
        ("кинопоиск", "kinopoisk"),
        ("ivi", "ivi"),
        ("иви", "ivi"),
        ("okko", "okko"),
        ("окко", "okko"),
    ] {
        if lower.contains(needle) {
            return Some(TvIntent::App(app.to_owned()));
        }
    }
    for number in 1..=4 {
        if lower.contains(&format!("hdmi{number}")) || lower.contains(&format!("hdmi {number}")) {
            return Some(TvIntent::Input(format!("hdmi{number}")));
        }
    }
    if (lower.contains("громк") || lower.contains("звук"))
        && let Some(level) = volume_level(lower)
    {
        return Some(TvIntent::SetVolume(level));
    }
    if lower.contains("громче") || lower.contains("погромче") {
        return Some(TvIntent::Volume("up"));
    }
    if lower.contains("тише") || lower.contains("потише") {
        return Some(TvIntent::Volume("down"));
    }
    if lower.contains("какой")
        || lower.contains("что с")
        || lower.contains("состояни")
        || lower.contains("включён")
        || lower.contains("включен")
        || lower.contains("какая громкость")
        || lower.contains("сколько громкость")
    {
        return Some(TvIntent::State);
    }
    if lower.contains("выключ") || lower.contains("выруб") || lower.contains("погаси")
    {
        return Some(TvIntent::Power(false));
    }
    if lower.contains("включ") || lower.contains("вруб") {
        return Some(TvIntent::Power(true));
    }
    if lower.contains("громк") || lower.contains("звук") {
        return Some(TvIntent::State);
    }
    None
}

fn volume_level(lower: &str) -> Option<u32> {
    let index = lower.find("громк").or_else(|| lower.find("звук"))?;
    let digits: String = lower[index..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok().map(|level| level.min(100))
}

/// Whether the channel offered the function this action needs.
fn action_supported(action: &Action, context: &SessionContext) -> bool {
    match action {
        Action::SetVolume(VolumeChange::Louder) => context.has("set_sound_volume_louder"),
        Action::SetVolume(VolumeChange::Quieter) => context.has("set_sound_volume_quieter"),
        Action::SetVolume(VolumeChange::Set(_)) => context.has("set_sound_volume"),
        Action::SetTimer { .. } => context.has("timer_set"),
        Action::Directive { name, .. } => context.has(normalize_function_name(name)),
        // Fast paths never produce server tools; they are checked against the
        // MCP registry when the LLM reply is parsed.
        Action::ToolCall { .. } => false,
    }
}

fn action_reply(action: &Action) -> String {
    match action {
        Action::SetVolume(VolumeChange::Louder) => "Делаю громче.".to_owned(),
        Action::SetVolume(VolumeChange::Quieter) => "Делаю тише.".to_owned(),
        Action::SetVolume(VolumeChange::Set(level)) => format!("Ставлю громкость {level}."),
        Action::SetTimer { seconds, .. } => {
            format!("Таймер {}.", crate::russian::duration_phrase(*seconds))
        }
        Action::Directive { .. } | Action::ToolCall { .. } => String::new(),
    }
}

/// Take the answer text and function calls out of the model reply.
///
/// llama.cpp runs with a JSON grammar (D-6), so the reply is always a JSON
/// object; a plain-text reply is passed through for tests and other backends.
fn parse_reply(
    raw: &str,
    context: &SessionContext,
    mcp: Option<&McpRegistry>,
) -> (String, Vec<Action>) {
    let cleaned = strip_code_fences(raw.trim());

    // Our JSON object shape, or an OpenAI-style role/content object.
    if let Some(object) = serde_json::from_str::<Value>(&cleaned)
        .ok()
        .and_then(|value| value.as_object().cloned())
    {
        let text = ["text", "reply", "content", "answer", "message", "response"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .unwrap_or("");
        let mut actions = object
            .get("function_calls")
            .and_then(Value::as_array)
            .map(|calls| calls_to_actions(calls, context, mcp))
            .unwrap_or_default();
        if text.is_empty() && actions.is_empty() && !object.contains_key("function_calls") {
            let strings: Vec<&str> = object.values().filter_map(Value::as_str).collect();
            if strings.len() == 1 {
                return (strings[0].to_owned(), Vec::new());
            }
            return (cleaned, Vec::new());
        }
        if text.is_empty() && actions.is_empty() {
            return (String::new(), Vec::new());
        }
        return (sanitize_text(text), std::mem::take(&mut actions));
    }

    // Some providers emit `function_calls: [...]` without the JSON object wrapper.
    if let Some((text, actions)) = parse_loose_calls(&cleaned, context, mcp) {
        return (sanitize_text(&text), actions);
    }

    (sanitize_text(&cleaned), Vec::new())
}

fn strip_code_fences(text: &str) -> String {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text);
    text.trim().trim_end_matches("```").trim().to_owned()
}

fn calls_to_actions(
    calls: &[Value],
    context: &SessionContext,
    mcp: Option<&McpRegistry>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    for call in calls {
        let Some(name) = call.get("name").and_then(Value::as_str) else {
            continue;
        };
        let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
        if context.has(normalize_function_name(name)) {
            actions.push(Action::Directive {
                name: name.to_owned(),
                arguments,
            });
        } else if mcp.is_some_and(|mcp| mcp.has_tool(name))
            || matches!(name, "home_list" | "home_get" | "home_set")
        {
            actions.push(Action::ToolCall {
                name: name.to_owned(),
                arguments,
            });
        } else {
            warn!(%name, "model called an unknown function; dropping it");
        }
    }
    actions
}

/// Handle `function_calls: [...]`, a bare JSON array of calls, or text followed
/// by such a block (the block must not be spoken).
fn parse_loose_calls(
    text: &str,
    context: &SessionContext,
    mcp: Option<&McpRegistry>,
) -> Option<(String, Vec<Action>)> {
    let lower = text.to_lowercase();
    let marker = lower.find("function_calls:");
    let (prefix, rest) = match marker {
        Some(index) => (
            &text[..index],
            text[index + "function_calls:".len()..].trim(),
        ),
        None => {
            let rest = text.trim();
            if rest.starts_with('[') {
                ("", rest)
            } else {
                return None;
            }
        }
    };
    let array_text = rest.split(']').next().map(|part| format!("{part}]"))?;
    let calls: Vec<Value> = serde_json::from_str(&array_text).ok()?;
    let actions = calls_to_actions(&calls, context, mcp);
    Some((prefix.trim().to_owned(), actions))
}

/// Never speak raw JSON or a tool-call block if it slipped through.
fn sanitize_text(text: &str) -> String {
    let lower = text.to_lowercase();
    let cut = ["function_calls:", "```json"]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .or_else(|| {
            let trimmed = text.trim_start();
            trimmed
                .starts_with('{')
                .then_some(text.len() - trimmed.len())
        });
    match cut {
        Some(index) => text[..index].trim().to_owned(),
        None => text.trim().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::action::FunctionSpec;
    use crate::llm::Message;

    struct FakeLlm {
        calls: AtomicUsize,
        seen: Mutex<Vec<Message>>,
        reply: String,
    }

    impl FakeLlm {
        fn new(reply: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                reply: reply.to_owned(),
            }
        }
    }

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn complete(&self, messages: &[Message]) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().extend_from_slice(messages);
            Ok(self.reply.clone())
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-03T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn agent(reply: &str) -> (Agent, Arc<Store>, Arc<FakeLlm>) {
        let store = Arc::new(Store::open(":memory:").unwrap());
        let llm = Arc::new(FakeLlm::new(reply));
        (
            Agent::new(Arc::clone(&store), llm.clone(), "system", 10),
            store,
            llm,
        )
    }

    fn station_context() -> SessionContext {
        SessionContext {
            functions: [
                "set_sound_volume_louder",
                "set_sound_volume_quieter",
                "set_sound_volume",
                "timer_set",
            ]
            .iter()
            .map(|name| FunctionSpec {
                name: format!("smart_speaker_{name}"),
                description: String::new(),
                arguments: json!({}),
            })
            .collect(),
        }
    }

    fn key() -> SessionKey {
        SessionKey::station("duid")
    }

    #[tokio::test]
    async fn reminder_bypasses_llm_and_stores_task() {
        let (agent, store, llm) = agent("не должен вызываться");
        let outcome = agent
            .process_at(
                "напомни через 20 минут выключить духовку",
                &key(),
                now(),
                &SessionContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.text, "Напомню через 20 минут. выключить духовку.");
        assert!(outcome.actions.is_empty());
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        let due = store
            .due_tasks(now() + chrono::Duration::minutes(21), 10)
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].device_id.as_deref(), Some("duid"));
    }

    #[tokio::test]
    async fn volume_fast_path_skips_llm() {
        let (agent, _, llm) = agent("не должен вызываться");
        let outcome = agent
            .process_at("громче", &key(), now(), &station_context())
            .await
            .unwrap();
        assert_eq!(
            outcome.actions,
            vec![Action::SetVolume(VolumeChange::Louder)]
        );
        assert_eq!(outcome.text, "Делаю громче.");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn timer_fast_path_returns_native_action() {
        let (agent, _, llm) = agent("не должен вызываться");
        let outcome = agent
            .process_at(
                "поставь таймер на 5 минут",
                &key(),
                now(),
                &station_context(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.actions,
            vec![Action::SetTimer {
                seconds: 300,
                label: None
            }]
        );
        assert_eq!(outcome.text, "Таймер на 5 минут.");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fast_path_does_not_fire_without_the_function() {
        let (agent, _, llm) = agent("{\"text\": \"Привет.\"}");
        let outcome = agent
            .process_at("громче", &key(), now(), &SessionContext::default())
            .await
            .unwrap();
        assert_eq!(outcome.text, "Привет.");
        assert!(outcome.actions.is_empty());
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dialogue_is_persisted_and_replayed() {
        let (agent, store, llm) = agent("{\"text\": \"ответ\"}");
        let first = agent
            .process_at("привет", &key(), now(), &SessionContext::default())
            .await
            .unwrap();
        assert_eq!(first.text, "ответ");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);

        let second = agent
            .process_at("а теперь?", &key(), now(), &SessionContext::default())
            .await
            .unwrap();
        assert_eq!(second.text, "ответ");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
        let messages = store.messages("duid", 10).unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "привет");
        assert_eq!(messages[1].content, "ответ");
        let seen = llm.seen.lock().unwrap();
        assert!(seen.iter().any(|message| message.content == "привет"));
        assert!(seen.iter().any(|message| message.content == "а теперь?"));
    }

    #[tokio::test]
    async fn empty_llm_answer_is_an_error() {
        let (agent, _, _) = agent("   ");
        assert!(
            agent
                .process_at("привет", &key(), now(), &SessionContext::default())
                .await
                .is_err()
        );
    }

    #[test]
    fn strips_the_wake_word() {
        assert_eq!(strip_wake_word("Алиса, какая погода?"), "какая погода?");
        assert_eq!(strip_wake_word("алиса! громче"), "громче");
        assert_eq!(strip_wake_word("Алиса"), "Алиса");
        assert_eq!(strip_wake_word("привет"), "привет");
    }

    #[test]
    fn unwraps_llm_json() {
        let context = SessionContext::default();
        assert_eq!(
            parse_reply("{\"reply\": \"Привет.\"}", &context, None).0,
            "Привет."
        );
        assert_eq!(
            parse_reply(
                "{\"role\": \"assistant\", \"content\": \"Привет!\"}",
                &context,
                None
            )
            .0,
            "Привет!"
        );
        assert_eq!(parse_reply("{\"text\": \"Да.\"}", &context, None).0, "Да.");
        assert_eq!(parse_reply("{\"a\": \"одно\"}", &context, None).0, "одно");
        assert_eq!(
            parse_reply("просто текст", &context, None).0,
            "просто текст"
        );
        assert_eq!(parse_reply("{\"a\": 1}", &context, None).0, "{\"a\": 1}");
    }

    #[test]
    fn parses_loose_function_calls() {
        let context = station_context();
        let (text, actions) = parse_reply(
            "function_calls: [{\"name\": \"timer_set\", \"arguments\": {\"durationSeconds\": 60}}]",
            &context,
            None,
        );
        assert!(text.is_empty());
        assert_eq!(
            actions,
            vec![Action::Directive {
                name: "timer_set".to_owned(),
                arguments: json!({"durationSeconds": 60}),
            }]
        );
    }

    #[test]
    fn strips_fences_and_json_tails() {
        let context = SessionContext::default();
        assert_eq!(
            parse_reply("```json\n{\"text\": \"Привет.\"}\n```", &context, None).0,
            "Привет."
        );
        assert_eq!(
            parse_reply(
                "Стих готов. function_calls: [{\"name\": \"x\", \"arguments\": {}}]",
                &context,
                None
            )
            .0,
            "Стих готов."
        );
        assert_eq!(sanitize_text("{\"a\": 1}"), "");
    }

    #[test]
    fn keeps_only_offered_function_calls() {
        let context = station_context();
        let (text, actions) = parse_reply(
            &json!({
                "text": "Ставлю таймер.",
                "function_calls": [
                    {"name": "timer_set", "arguments": {"durationSeconds": 60}},
                    {"name": "alarm_add", "arguments": {"time": "07:00"}}
                ]
            })
            .to_string(),
            &context,
            None,
        );
        assert_eq!(text, "Ставлю таймер.");
        assert_eq!(
            actions,
            vec![Action::Directive {
                name: "timer_set".to_owned(),
                arguments: json!({"durationSeconds": 60}),
            }]
        );
    }

    #[test]
    fn parses_tv_intents_without_the_llm() {
        assert_eq!(
            parse_tv_intent("включи телевизор в гостиной"),
            Some(TvIntent::Power(true))
        );
        assert_eq!(
            parse_tv_intent("выключи телевизор"),
            Some(TvIntent::Power(false))
        );
        assert_eq!(
            parse_tv_intent("какой телевизор включён"),
            Some(TvIntent::State)
        );
        assert_eq!(
            parse_tv_intent("какая громкость на телевизоре"),
            Some(TvIntent::State)
        );
        assert_eq!(
            parse_tv_intent("сделай громче телевизор"),
            Some(TvIntent::Volume("up"))
        );
        assert_eq!(
            parse_tv_intent("громкость 30 на телевизоре"),
            Some(TvIntent::SetVolume(30))
        );
        assert_eq!(
            parse_tv_intent("включи ютуб на телевизоре"),
            Some(TvIntent::App("youtube".to_owned()))
        );
        assert_eq!(
            parse_tv_intent("переключи телевизор на hdmi2"),
            Some(TvIntent::Input("hdmi2".to_owned()))
        );
    }

    #[test]
    fn light_commands_are_not_tv_intents() {
        assert!(parse_tv_intent("включи свет в детской").is_none());
        assert!(!mentions_tv("включи свет в детской"));
        assert!(mentions_tv("включи телевизор в детской"));
    }

    #[test]
    fn non_light_devices_bypass_the_home_fast_path() {
        assert!(mentions_non_light_device("включи телевизор в детской"));
        assert!(mentions_non_light_device("включи вытяжку в санузле"));
        assert!(mentions_non_light_device("открой замок"));
        assert!(!mentions_non_light_device("включи свет в детской"));
        assert!(!mentions_non_light_device("выключи кухню"));
    }

    #[test]
    fn recognizes_light_status_questions_and_statements() {
        for text in [
            "какой свет включен",
            "какие светильники включены",
            "что включено",
            "статус света",
            "свет включён",
            "свет выключен",
            "свет горит",
        ] {
            assert!(asks_about_lights(text), "{text}");
        }
        for text in [
            "включи свет в спальне",
            "выключи свет",
            "зажги свет на кухне",
            "погаси свет",
            "яркость 40 в спальне",
            "какой сегодня день",
        ] {
            assert!(!asks_about_lights(text), "{text}");
        }
    }

    #[test]
    fn system_prompt_lists_the_functions() {
        let (agent, _, _) = agent("x");
        let prompt = agent.system_prompt_with_time(now(), &station_context());
        assert!(prompt.contains("set_sound_volume_louder"));
        assert!(prompt.contains("function_calls"));
    }
}
