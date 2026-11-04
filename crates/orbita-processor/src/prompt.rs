//! Handlebars prompt generation, matching the upstream processor.

use std::path::Path;

use handlebars::Handlebars;
use serde_json::json;

use crate::functions::{Functions, State};

pub struct PromptGenerator {
    handlebars: Handlebars<'static>,
}

impl PromptGenerator {
    pub fn new(
        system_template: &str,
        functions_template: &str,
        state_template: &str,
    ) -> Result<Self, handlebars::TemplateError> {
        let mut handlebars = Handlebars::new();
        handlebars.register_escape_fn(handlebars::no_escape);
        handlebars.register_template_string("system", system_template)?;
        handlebars.register_template_string("functions", functions_template)?;
        handlebars.register_template_string("state", state_template)?;
        Ok(Self { handlebars })
    }

    pub fn from_files(
        system_path: impl AsRef<Path>,
        functions_path: impl AsRef<Path>,
        state_path: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        let system = std::fs::read_to_string(system_path)?;
        let functions = std::fs::read_to_string(functions_path)?;
        let state = std::fs::read_to_string(state_path)?;
        Ok(Self::new(&system, &functions, &state)?)
    }

    pub fn generate(&self) -> Result<String, handlebars::RenderError> {
        self.handlebars.render("system", &json!({}))
    }

    pub fn generate_functions(
        &self,
        functions: &Functions,
    ) -> Result<String, handlebars::RenderError> {
        self.handlebars.render(
            "functions",
            &json!({"functionsText": functions_text(functions)}),
        )
    }

    pub fn generate_state(&self, state: &State) -> Result<String, handlebars::RenderError> {
        self.handlebars
            .render("state", &json!({"stateText": state_text(state)}))
    }
}

fn functions_text(functions: &Functions) -> String {
    let mut text = String::new();
    for (name, function) in functions {
        let schema = serde_json::to_string(&function.info.arguments).unwrap_or_default();
        text.push_str(&format!(
            "{name} ({}): has response: {}, JSON schema: {schema}\n",
            function.info.description, function.info.has_response
        ));
    }
    text
}

fn state_text(state: &State) -> String {
    let mut text = String::new();
    for (name, entry) in state {
        text.push_str(&format!(
            "{name} ({}): {}\n",
            entry.description, entry.value
        ));
    }
    text.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use serde_json::json;

    use super::*;
    use crate::functions::{FunctionEntry, FunctionInfo, ServerKind};
    use crate::protocol::DirectiveFunctionInfo;

    struct NoopServer;

    #[async_trait::async_trait]
    impl crate::functions::FunctionServer for NoopServer {
        fn name(&self) -> String {
            "noop".to_owned()
        }
        fn kind(&self) -> ServerKind {
            ServerKind::Remote
        }
        async fn call_function(
            &self,
            _session_id: &str,
            _metadata: &serde_json::Value,
            _name: &str,
            _arguments: &serde_json::Value,
        ) -> Result<String, String> {
            Ok(String::new())
        }
    }

    #[test]
    fn renders_functions_and_state() {
        let generator = PromptGenerator::new(
            "system",
            "functions: {{functionsText}}",
            "state: {{stateText}}",
        )
        .unwrap();

        let server = Arc::new(NoopServer);
        let functions = BTreeMap::from([(
            "set_lights".to_owned(),
            FunctionEntry {
                info: FunctionInfo {
                    arguments: json!({"type": "object"}),
                    description: "Turn lights on or off".to_owned(),
                    has_response: false,
                },
                server,
            },
        )]);
        let functions_text = generator.generate_functions(&functions).unwrap();
        assert!(functions_text.contains(
            "set_lights (Turn lights on or off): has response: false, JSON schema: {\"type\":\"object\"}"
        ));

        let state = BTreeMap::from([(
            "date_time".to_owned(),
            crate::functions::StateEntry {
                description: "now".to_owned(),
                value: "02-11-2026 12:00:00".to_owned(),
            },
        )]);
        assert_eq!(
            generator.generate_state(&state).unwrap(),
            "state: date_time (now): 02-11-2026 12:00:00"
        );
    }

    #[test]
    fn directive_info_is_reusable() {
        let info = DirectiveFunctionInfo {
            arguments: json!({}),
            description: "x".to_owned(),
        };
        assert_eq!(info.description, "x");
    }
}
