use crate::agent::{truncate_messages, Agent, AgentResult};
use crate::config::AgentConfig;
use crate::llm::{LlmProvider, Message, ToolDefinition};
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;
use async_trait::async_trait;

pub struct CoderAgent {
    pub config: AgentConfig,
}

impl CoderAgent {
    pub fn new(config: AgentConfig) -> Self {
        CoderAgent { config }
    }
}

#[async_trait]
impl Agent for CoderAgent {
    fn name(&self) -> &str {
        "coder"
    }

    fn system_prompt(&self) -> &str {
        "You are an expert software engineer assistant. \
        You have access to tools to read, write, and edit files, \
        run bash commands, and search code. \
        Be autonomous — complete the task without asking for confirmation. \
        Use tools to implement the requested changes. \
        When the task is complete, provide a brief summary."
    }

    async fn run(
        &self,
        messages: &mut Vec<Message>,
        tools: &ToolRegistry,
        llm: &dyn LlmProvider,
        ctx: &ToolContext,
    ) -> Result<AgentResult> {
        let tool_defs = build_tool_definitions(tools);
        let mut iterations = 0u32;
        let mut tool_calls_total = 0u32;
        loop {
            truncate_messages(messages, 400_000);
            let response = llm.chat_completion(messages, &tool_defs).await?;
            if response.tool_calls.is_none()
                || response
                    .tool_calls
                    .as_ref()
                    .map(|t| t.is_empty())
                    .unwrap_or(true)
            {
                let final_msg = response
                    .content
                    .unwrap_or_else(|| "Task completed.".to_string());
                println!("{}", final_msg);
                return Ok(AgentResult {
                    final_message: final_msg,
                    iterations,
                    tool_calls_total,
                });
            }
            messages.push(Message::assistant(
                response.content.clone(),
                response.tool_calls.clone(),
            ));
            let tool_calls = response.tool_calls.unwrap_or_default();
            let to_execute = tool_calls
                .into_iter()
                .take(self.config.max_tool_calls_per_response as usize);
            for tool_call in to_execute {
                tool_calls_total += 1;
                let args = serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let result = if let Some(tool) = tools.get(&tool_call.function.name) {
                    match tool.execute(args, ctx).await {
                        Ok(r) => r,
                        Err(e) => crate::tools::ToolResult {
                            output: format!("Tool execution error: {}", e),
                            is_error: true,
                        },
                    }
                } else {
                    crate::tools::ToolResult {
                        output: format!("Tool '{}' not found", tool_call.function.name),
                        is_error: true,
                    }
                };
                messages.push(Message::tool(tool_call.id.clone(), result.output));
            }
            iterations += 1;
            if iterations >= self.config.max_iterations {
                let warning = format!(
                    "Reached maximum iterations ({}). Stopping.",
                    self.config.max_iterations
                );
                tracing::warn!("{}", warning);
                return Ok(AgentResult {
                    final_message: warning,
                    iterations,
                    tool_calls_total,
                });
            }
        }
    }
}

/// Build ToolDefinition list from registry (OpenAI format)
fn build_tool_definitions(tools: &ToolRegistry) -> Vec<ToolDefinition> {
    tools
        .list_definitions()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::llm::{FunctionCall, LlmResponse, Message, Role, ToolCall, ToolDefinition};
    use crate::tools::{Tool, ToolContext, ToolRegistry, ToolResult};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    // MockLlmProvider: returns a sequence of canned responses
    struct MockLlm {
        responses: Arc<Mutex<Vec<LlmResponse>>>,
    }
    impl MockLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            MockLlm {
                responses: Arc::new(Mutex::new(responses)),
            }
        }
    }
    #[async_trait]
    impl LlmProvider for MockLlm {
        async fn chat_completion(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse> {
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                return Ok(LlmResponse {
                    content: Some("done".to_string()),
                    tool_calls: None,
                });
            }
            Ok(r.remove(0))
        }
    }

    // MockTool: just echoes back the args
    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo the input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"text":{"type":"string"}}})
        }
        async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult> {
            Ok(ToolResult {
                output: args["text"].as_str().unwrap_or("").to_string(),
                is_error: false,
            })
        }
    }

    fn make_tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
        }
    }

    fn test_config() -> AgentConfig {
        AgentConfig {
            max_iterations: 5,
            max_tool_calls_per_response: 3,
        }
    }

    #[tokio::test]
    async fn test_coder_simple_task() {
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("call1", "echo", r#"{"text":"hello"}"#)]),
            },
            LlmResponse {
                content: Some("Task complete!".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let agent = CoderAgent::new(test_config());
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("say hello"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.iterations, 1);
        assert_eq!(result.tool_calls_total, 1);
        assert!(result.final_message.contains("Task complete"));
    }

    #[tokio::test]
    async fn test_coder_max_iterations() {
        let responses: Vec<LlmResponse> = (0..20)
            .map(|_| LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("call1", "echo", r#"{"text":"loop"}"#)]),
            })
            .collect();
        let llm = MockLlm::new(responses);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let config = AgentConfig {
            max_iterations: 3,
            max_tool_calls_per_response: 1,
        };
        let agent = CoderAgent::new(config);
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("loop forever"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.iterations, 3);
        assert!(result.final_message.contains("maximum iterations"));
    }

    #[tokio::test]
    async fn test_coder_max_tool_calls_per_response() {
        let tool_calls: Vec<ToolCall> = (0..5)
            .map(|i| make_tool_call(&format!("call{}", i), "echo", r#"{"text":"x"}"#))
            .collect();
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(tool_calls),
            },
            LlmResponse {
                content: Some("done".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let config = AgentConfig {
            max_iterations: 5,
            max_tool_calls_per_response: 2,
        };
        let agent = CoderAgent::new(config);
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("many tools"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.tool_calls_total, 2);
    }

    #[tokio::test]
    async fn test_coder_tool_error_recovery() {
        struct ErrorTool;
        #[async_trait::async_trait]
        impl Tool for ErrorTool {
            fn name(&self) -> &str {
                "fail"
            }
            fn description(&self) -> &str {
                "Always fails"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type":"object"})
            }
            async fn execute(&self, _: serde_json::Value, _: &ToolContext) -> Result<ToolResult> {
                Ok(ToolResult {
                    output: "something went wrong".to_string(),
                    is_error: true,
                })
            }
        }
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("e1", "fail", "{}")]),
            },
            LlmResponse {
                content: Some("recovered".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ErrorTool));
        let agent = CoderAgent::new(test_config());
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("test error"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert!(result.final_message.contains("recovered"));
        let tool_result_msg = messages.iter().find(|m| m.role == Role::Tool);
        assert!(tool_result_msg.is_some());
        assert!(tool_result_msg
            .unwrap()
            .content
            .as_ref()
            .unwrap()
            .contains("something went wrong"));
    }

    #[tokio::test]
    async fn test_context_truncation() {
        let long_content = "a".repeat(10_000);
        let mut messages: Vec<Message> = vec![Message::system("system")];
        messages.push(Message::user("first user message"));
        for _ in 0..50 {
            messages.push(Message::user(long_content.clone()));
            messages.push(Message::assistant(Some(long_content.clone()), None));
        }
        let original_len = messages.len();
        truncate_messages(&mut messages, 50_000);
        assert!(
            messages.len() < original_len,
            "Expected truncation but got {} msgs",
            messages.len()
        );
        assert_eq!(messages[0].role, Role::System);
    }
}
