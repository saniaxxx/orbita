//! Dialog session logic: prompts, LLM loop, function calls and caching.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    config::Config,
    functions::{
        DirectiveFunctionServer, FunctionEntry, FunctionServer, Functions, ServerKind, State,
        StateEntry, StateServer,
    },
    llm::{LlmMessage, StructuredResponse, parse_llm_response},
    llm_client::LlmClient,
    prompt::PromptGenerator,
    protocol::{Directive, DirectiveFunctionInfo, Metadata, PartialResponseData},
};

pub struct Processor {
    pub config: Config,
    llm: Arc<dyn LlmClient>,
    prompt: Arc<PromptGenerator>,
    function_servers: Vec<Arc<dyn FunctionServer>>,
    state_servers: Vec<Arc<dyn StateServer>>,
    cache: Mutex<ResponseCache>,
}

impl Processor {
    pub fn new(
        config: Config,
        llm: Arc<dyn LlmClient>,
        prompt: Arc<PromptGenerator>,
        function_servers: Vec<Arc<dyn FunctionServer>>,
        state_servers: Vec<Arc<dyn StateServer>>,
    ) -> Self {
        let capacity = config.cache_size;
        Self {
            config,
            llm,
            prompt,
            function_servers,
            state_servers,
            cache: Mutex::new(ResponseCache::new(capacity)),
        }
    }

    pub fn new_session(self: Arc<Self>) -> Session {
        Session {
            processor: self,
            session_id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            is_first_request: true,
            functions: BTreeMap::new(),
            last_functions_text: None,
        }
    }
}

pub struct Session {
    processor: Arc<Processor>,
    session_id: String,
    messages: Vec<LlmMessage>,
    is_first_request: bool,
    functions: Functions,
    last_functions_text: Option<String>,
}

impl Session {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn prepare(
        &mut self,
        directive_functions: BTreeMap<String, DirectiveFunctionInfo>,
    ) -> Result<(), String> {
        self.messages = vec![LlmMessage::system(
            self.processor
                .prompt
                .generate()
                .map_err(|error| error.to_string())?,
        )];

        let directive_server = Arc::new(DirectiveFunctionServer::new(directive_functions));
        let directive_functions = directive_server
            .get_independent_functions()
            .await
            .unwrap_or_default();
        let mut functions = merge_functions(
            directive_server.name(),
            BTreeMap::new(),
            directive_functions,
            directive_server,
        );

        for server in &self.processor.function_servers {
            match server.get_independent_functions().await {
                Ok(server_functions) => {
                    functions = merge_functions(
                        server.name(),
                        functions,
                        server_functions,
                        Arc::clone(server),
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        server = %server.name(),
                        %error,
                        "failed to fetch independent functions"
                    );
                }
            }
        }

        self.functions = functions;
        let text = self
            .processor
            .prompt
            .generate_functions(&self.functions)
            .map_err(|error| error.to_string())?;
        self.last_functions_text = Some(text.clone());
        self.messages.push(LlmMessage::tool(text));
        Ok(())
    }

    pub async fn process(
        &mut self,
        text: &str,
        metadata: &Metadata,
    ) -> Result<Vec<PartialResponseData>, String> {
        self.refresh_functions(metadata).await;

        let text_for_prompt = self
            .processor
            .prompt
            .generate_functions(&self.functions)
            .map_err(|error| error.to_string())?;
        // `prepare` already sent the same list; repeat it only when the functions
        // actually changed (MCP servers may add tools between turns).
        if self.last_functions_text.as_deref() != Some(text_for_prompt.as_str()) {
            self.last_functions_text = Some(text_for_prompt.clone());
            self.messages.push(LlmMessage::tool(text_for_prompt));
        }

        let independent_state = self.get_independent_state().await;
        self.messages.push(LlmMessage::tool(
            self.processor
                .prompt
                .generate_state(&independent_state)
                .map_err(|error| error.to_string())?,
        ));

        let mut state = self.get_state(metadata).await;
        merge_ready_to_use(&mut state, &metadata.ready_to_use);
        self.messages.push(LlmMessage::tool(
            self.processor
                .prompt
                .generate_state(&state)
                .map_err(|error| error.to_string())?,
        ));

        let text = text.trim().to_owned();
        self.messages.push(LlmMessage::user(text.clone()));

        let mut responses = Vec::new();
        let mut cached_response = if self.is_first_request {
            self.processor
                .cache
                .lock()
                .map(|mut cache| cache.get(&text))
                .ok()
                .flatten()
        } else {
            None
        };

        // Upstream loops when a function with a response is called: its result
        // becomes a tool message and the LLM gets one more pass.
        for _ in 0..4 {
            let response_content = match cached_response.take() {
                Some(cached) => {
                    tracing::info!("received answer from cache");
                    cached
                }
                None => self.processor.llm.complete(&self.messages).await?,
            };

            let structured = parse_llm_response(&response_content)
                .map_err(|error| format!("failed to parse LLM response: {error}"))?;
            tracing::info!(answer = %response_content, "received answer from LLM");

            self.messages
                .push(LlmMessage::assistant(response_content.clone()));

            let (directives, response_results) = self.call_functions(&structured, metadata).await;

            if !structured.require_more_input
                && structured.can_cache
                && self.is_first_request
                && response_results.is_empty()
                && let Ok(mut cache) = self.processor.cache.lock()
            {
                cache.put(text.clone(), response_content);
            }

            self.is_first_request = false;

            responses.push(PartialResponseData {
                directives,
                text: structured.text,
                finished: response_results.is_empty(),
                should_listen: structured.require_more_input,
            });

            if response_results.is_empty() {
                break;
            }

            let tool_result = response_results
                .into_iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n");
            self.messages.push(LlmMessage::tool(tool_result));
        }

        Ok(responses)
    }

    async fn refresh_functions(&mut self, metadata: &Metadata) {
        for server in &self.processor.function_servers {
            match server
                .get_functions(&self.session_id, &metadata.internal)
                .await
            {
                Ok(server_functions) => {
                    self.functions = merge_functions(
                        server.name(),
                        std::mem::take(&mut self.functions),
                        server_functions,
                        Arc::clone(server),
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        server = %server.name(),
                        %error,
                        "failed to fetch dependent functions"
                    );
                }
            }
        }
    }

    async fn get_independent_state(&self) -> State {
        let mut result = State::new();
        for server in &self.processor.state_servers {
            match server.get_independent_state().await {
                Ok(state) => merge_state(server.name(), &mut result, state),
                Err(error) => tracing::warn!(
                    server = %server.name(),
                    %error,
                    "failed to fetch independent state"
                ),
            }
        }
        result
    }

    async fn get_state(&self, metadata: &Metadata) -> State {
        let mut result = State::new();
        for server in &self.processor.state_servers {
            match server.get_state(&self.session_id, &metadata.internal).await {
                Ok(state) => merge_state(server.name(), &mut result, state),
                Err(error) => tracing::warn!(
                    server = %server.name(),
                    %error,
                    "failed to fetch state"
                ),
            }
        }
        result
    }

    async fn call_functions(
        &self,
        structured: &StructuredResponse,
        metadata: &Metadata,
    ) -> (Vec<Directive>, Vec<(String, String)>) {
        let mut directives = Vec::new();
        let mut response_results = Vec::new();

        for call in &structured.function_calls {
            let Some(function) = self.functions.get(&call.name) else {
                tracing::warn!(name = %call.name, "tried to call a non-existent function");
                continue;
            };

            if function.server.kind() == ServerKind::Directive {
                directives.push(Directive {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
                continue;
            }

            if function.info.has_response && call.schedule.is_some() {
                tracing::warn!(
                    name = %call.name,
                    "function with a response cannot be scheduled"
                );
                continue;
            }

            if function.info.has_response {
                let value = function
                    .server
                    .call_function(
                        &self.session_id,
                        &metadata.internal,
                        &call.name,
                        &call.arguments,
                    )
                    .await
                    .unwrap_or_else(|error| format!("Error: {error}"));
                response_results.push((call.name.clone(), value));
            } else if let Some(schedule) = call.schedule {
                let server = Arc::clone(&function.server);
                let session_id = self.session_id.clone();
                let name = call.name.clone();
                let arguments = call.arguments.clone();
                let internal = metadata.internal.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(schedule).await;
                    if let Err(error) = server
                        .call_function(&session_id, &internal, &name, &arguments)
                        .await
                    {
                        tracing::warn!(%name, %error, "scheduled function failed");
                    }
                });
            } else if let Err(error) = function
                .server
                .call_function(
                    &self.session_id,
                    &metadata.internal,
                    &call.name,
                    &call.arguments,
                )
                .await
            {
                tracing::warn!(name = %call.name, %error, "function failed");
            }
        }

        (directives, response_results)
    }
}

fn merge_functions(
    server_name: String,
    mut target: Functions,
    functions: crate::functions::FunctionMap,
    server: Arc<dyn FunctionServer>,
) -> Functions {
    for (name, info) in functions {
        if target.contains_key(&name) {
            tracing::warn!(
                server = %server_name,
                function = %name,
                "duplicate function entry"
            );
            continue;
        }
        target.insert(
            name,
            FunctionEntry {
                info,
                server: Arc::clone(&server),
            },
        );
    }
    target
}

fn merge_state(server_name: String, target: &mut State, state: State) {
    for (name, entry) in state {
        if target.contains_key(&name) {
            tracing::warn!(server = %server_name, state = %name, "duplicate state entry");
            continue;
        }
        target.insert(name, entry);
    }
}

fn merge_ready_to_use(state: &mut State, ready_to_use: &Value) {
    let Some(object) = ready_to_use.as_object() else {
        return;
    };
    for (name, entry) in object {
        if state.contains_key(name) {
            continue;
        }
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let value = match entry.get("value") {
            Some(Value::String(value)) => value.clone(),
            Some(value) => value.to_string(),
            None => String::new(),
        };
        state.insert(name.clone(), StateEntry { description, value });
    }
}

struct ResponseCache {
    capacity: usize,
    entries: HashMap<String, String>,
    order: VecDeque<String>,
}

impl ResponseCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<String> {
        let value = self.entries.get(key)?.clone();
        self.touch(key);
        Some(value)
    }

    fn put(&mut self, key: String, value: String) {
        if self.entries.insert(key.clone(), value).is_some() {
            self.touch(&key);
        } else {
            self.order.push_back(key);
        }
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
        }
        self.order.push_back(key.to_owned());
    }
}
