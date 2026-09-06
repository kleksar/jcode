use super::*;
use serde_json::json;
use std::sync::Arc;

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo test input"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new(input["text"].as_str().unwrap_or_default()))
    }
}

fn test_context() -> ToolContext {
    ToolContext {
        session_id: "batch-registry-lifetime".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "batch-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: super::super::ToolExecutionMode::Direct,
    }
}

async fn registry_with_batch_and_echo() -> Registry {
    let registry = Registry::empty();
    let mut tools = registry.tools.write().await;
    tools.insert("echo".to_string(), Arc::new(EchoTool));
    tools.insert(
        "batch".to_string(),
        Arc::new(BatchTool::new(registry.downgrade())),
    );
    drop(tools);
    registry
}

#[tokio::test]
async fn registry_tool_map_drops_after_external_owners_are_dropped() {
    let registry = Registry::empty();
    let tools = Arc::downgrade(&registry.tools);

    registry.tools.write().await.insert(
        "batch".to_string(),
        Arc::new(BatchTool::new(registry.downgrade())) as Arc<dyn Tool>,
    );

    drop(registry);

    assert!(
        tools.upgrade().is_none(),
        "BatchTool must not strongly retain the registry tool map that owns it"
    );
}

#[tokio::test]
async fn batch_executes_through_surviving_registry_clone() {
    let registry = registry_with_batch_and_echo().await;
    let surviving_clone = registry.clone();
    drop(registry);

    let output = surviving_clone
        .execute(
            "batch",
            json!({
                "tool_calls": [{
                    "tool": "echo",
                    "intent": "Verify the surviving registry clone",
                    "parameters": {"text": "still alive"}
                }]
            }),
            test_context(),
        )
        .await
        .expect("batch should use the surviving registry clone's tool map");

    assert!(output.output.contains("still alive"));
    assert!(output.output.contains("Completed: 1 succeeded, 0 failed"));
}

#[tokio::test]
async fn batch_fails_cleanly_after_registry_tool_map_is_dropped() {
    let registry = registry_with_batch_and_echo().await;
    let batch = registry
        .tools
        .read()
        .await
        .get("batch")
        .cloned()
        .expect("batch tool should be registered");
    drop(registry);

    let error = batch
        .execute(
            json!({
                "tool_calls": [{
                    "tool": "echo",
                    "intent": "Verify clean teardown",
                    "parameters": {"text": "unreachable"}
                }]
            }),
            test_context(),
        )
        .await
        .expect_err("batch should reject execution after its registry is gone");

    assert_eq!(
        error.to_string(),
        "Batch tool registry is no longer available"
    );
}

struct EchoPayloadTool;

#[async_trait]
impl Tool for EchoPayloadTool {
    fn name(&self) -> &str {
        "echo_payload"
    }

    fn description(&self) -> &str {
        "Returns the requested test payload."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "payload": { "type": "string" } },
            "required": ["payload"]
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new(
            input["payload"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

fn test_context() -> ToolContext {
    ToolContext {
        session_id: "batch-acceptance-test".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-batch".to_string(),
        working_dir: Some(std::env::temp_dir()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: jcode_tool_core::ToolExecutionMode::Direct,
    }
}

#[test]
fn description_includes_parallel_tool_call_example() {
    assert!(BATCH_DESCRIPTION.contains("Run independent tool calls in parallel"));
    assert!(BATCH_DESCRIPTION.contains(r#""tool_calls": ["#));
    assert!(BATCH_DESCRIPTION.contains(r#""tool": "read""#));
    assert!(BATCH_DESCRIPTION.contains(r#""tool": "agentgrep""#));
    assert!(BATCH_DESCRIPTION.contains("predictably small outputs"));
    assert!(BATCH_DESCRIPTION.contains("rerun that subcall directly"));
}

#[test]
fn format_results_preserves_small_outputs_and_errors() {
    let results = vec![
        (0, "read".to_string(), Ok(ToolOutput::new("small output"))),
        (
            1,
            "agentgrep".to_string(),
            Err(anyhow::anyhow!("search failed exactly")),
        ),
    ];

    let (output, successes, errors, failed_tools) = format_batch_results(results);

    assert!(output.contains("small output"));
    assert!(output.contains("Error: search failed exactly"));
    assert!(!output.contains("truncated to the batch output budget"));
    assert_eq!(successes, 1);
    assert_eq!(errors, 1);
    assert_eq!(failed_tools, vec!["agentgrep"]);
}

#[test]
fn format_results_caps_combined_success_payload_and_explains_recovery() {
    let results = vec![
        (
            0,
            "read".to_string(),
            Ok(ToolOutput::new("α".repeat(30_000))),
        ),
        (
            1,
            "bash".to_string(),
            Ok(ToolOutput::new("β".repeat(30_000))),
        ),
    ];

    let (output, successes, errors, _) = format_batch_results(results);

    assert_eq!(
        output.matches('α').count() * "α".len(),
        BATCH_OUTPUT_BUDGET_BYTES / 2
    );
    assert_eq!(
        output.matches('β').count() * "β".len(),
        BATCH_OUTPUT_BUDGET_BYTES / 2
    );
    assert!(output.contains("rerun subcall [1] `read` directly for full output"));
    assert!(output.contains("rerun subcall [2] `bash` directly for full output"));
    assert_eq!(successes, 2);
    assert_eq!(errors, 0);
}

#[test]
fn format_results_reuses_budget_left_by_small_subcalls() {
    let small = "s".repeat(100);
    let results = vec![
        (0, "read".to_string(), Ok(ToolOutput::new(small.clone()))),
        (
            1,
            "bash".to_string(),
            Ok(ToolOutput::new("x".repeat(30_000))),
        ),
    ];

    let (output, _, _, _) = format_batch_results(results);

    assert!(output.contains(&small));
    assert_eq!(
        output.matches('x').count(),
        BATCH_OUTPUT_BUDGET_BYTES - small.len()
    );
}

#[test]
fn format_results_truncates_unicode_on_a_character_boundary() {
    let payload = "🦀".repeat(10_000);
    let results = vec![(0, "read".to_string(), Ok(ToolOutput::new(payload)))];

    let (output, _, _, _) = format_batch_results(results);

    assert!(output.contains("🦀"));
    assert!(output.contains("rerun subcall [1] `read` directly for full output"));
    assert_eq!(
        output.matches('🦀').count() * "🦀".len(),
        BATCH_OUTPUT_BUDGET_BYTES
    );
}

#[tokio::test]
async fn registry_execute_enforces_batch_budget_and_returns_recovery_instructions() {
    let registry = Registry::empty();
    registry
        .register(
            "echo_payload".to_string(),
            std::sync::Arc::new(EchoPayloadTool),
        )
        .await;
    registry
        .register(
            "batch".to_string(),
            std::sync::Arc::new(BatchTool::new(registry.clone())),
        )
        .await;

    let output = registry
        .execute(
            "batch",
            json!({
                "intent": "Exercise the public batch execution path",
                "tool_calls": [
                    {
                        "tool": "echo_payload",
                        "intent": "Return alpha payload",
                        "payload": "α".repeat(30_000)
                    },
                    {
                        "tool": "echo_payload",
                        "intent": "Return beta payload",
                        "payload": "β".repeat(30_000)
                    }
                ]
            }),
            test_context(),
        )
        .await
        .expect("public batch execution should succeed")
        .output;

    let payload_bytes =
        output.matches('α').count() * "α".len() + output.matches('β').count() * "β".len();
    assert_eq!(payload_bytes, BATCH_OUTPUT_BUDGET_BYTES);
    assert_eq!(output.matches("rerun subcall").count(), 2);
    assert!(output.contains("Completed: 2 succeeded, 0 failed"));
}

#[test]
fn test_normalize_flat_params() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "file_path": "file1.txt"},
            {"tool": "read", "file_path": "file2.txt"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 2);
    assert_eq!(parsed.tool_calls[0].tool, "read");
    let params = parsed.tool_calls[0].parameters.as_ref().unwrap();
    assert_eq!(params["file_path"], "file1.txt");
}

#[test]
fn test_normalize_already_nested() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "parameters": {"file_path": "file1.txt"}}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 1);
    let params = parsed.tool_calls[0].parameters.as_ref().unwrap();
    assert_eq!(params["file_path"], "file1.txt");
}

#[test]
fn test_normalize_forwards_top_level_intent_into_nested_parameters() {
    let input = json!({
        "tool_calls": [{
            "tool": "read",
            "intent": "Inspect the batch renderer",
            "parameters": {"file_path": "src/tui/ui_messages.rs"}
        }]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    let params = parsed.tool_calls[0].parameters.as_ref().unwrap();

    assert_eq!(params["intent"], "Inspect the batch renderer");
    assert_eq!(params["file_path"], "src/tui/ui_messages.rs");
}

#[test]
fn test_normalize_name_key_to_tool() {
    let input = json!({
        "tool_calls": [
            {"name": "read", "parameters": {"file_path": "file1.txt"}},
            {"name": "grep", "pattern": "foo", "path": "src/"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 2);
    assert_eq!(parsed.tool_calls[0].tool, "read");
    let params0 = parsed.tool_calls[0].parameters.as_ref().unwrap();
    assert_eq!(params0["file_path"], "file1.txt");
    assert_eq!(parsed.tool_calls[1].tool, "grep");
    let params1 = parsed.tool_calls[1].parameters.as_ref().unwrap();
    assert_eq!(params1["pattern"], "foo");
}

#[test]
fn test_normalize_mixed_tool_and_name_keys() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "parameters": {"file_path": "a.rs"}},
            {"name": "read", "parameters": {"file_path": "b.rs"}},
            {"tool": "grep", "pattern": "test"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 3);
    assert_eq!(parsed.tool_calls[0].tool, "read");
    assert_eq!(parsed.tool_calls[1].tool, "read");
    assert_eq!(parsed.tool_calls[2].tool, "grep");
}

#[test]
fn test_normalize_arguments_aliases_to_parameters() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "arguments": {"file_path": "a.rs"}},
            {"tool": "read", "args": {"file_path": "b.rs"}},
            {"tool": "read", "input": {"file_path": "c.rs"}}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();

    assert_eq!(parsed.tool_calls.len(), 3);
    assert_eq!(
        parsed.tool_calls[0].parameters.as_ref().unwrap()["file_path"],
        "a.rs"
    );
    assert_eq!(
        parsed.tool_calls[1].parameters.as_ref().unwrap()["file_path"],
        "b.rs"
    );
    assert_eq!(
        parsed.tool_calls[2].parameters.as_ref().unwrap()["file_path"],
        "c.rs"
    );
}

#[test]
fn test_schema_only_requires_tool() {
    let registry = Registry {
        tools: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        skills: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::skill::SkillRegistry::default(),
        )),
        compaction: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::compaction::CompactionManager::new(),
        )),
    };
    let schema = BatchTool::new(registry.downgrade()).parameters_schema();

    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["required"],
        // Nested batch entries require `intent` alongside `tool` so every
        // fanned-out call carries a display label, matching the central
        // intent requirement in `ensure_intent_in_schema` (8505080a6).
        json!(["tool", "intent"])
    );
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["additionalProperties"],
        json!(true)
    );
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["properties"]["tool"]["description"],
        json!("Tool name.")
    );
    assert!(schema["properties"]["tool_calls"]["items"]["properties"]["intent"].is_object());
    assert!(schema["properties"]["tool_calls"]["items"]["properties"]["parameters"].is_null());
}

#[test]
fn test_schema_keeps_flat_generic_subcall_shape() {
    let schema = generic_batch_schema();

    assert!(schema["properties"]["tool_calls"]["description"].is_null());
    assert!(schema["properties"]["tool_calls"]["items"]["description"].is_null());
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["properties"]
            .as_object()
            .map(|props| props.len()),
        Some(2)
    );
    assert!(schema["properties"]["tool_calls"]["items"]["oneOf"].is_null());
}

#[test]
fn subcall_level_accept_large_output_is_forwarded_into_parameters() {
    // Models place the flag beside `tool` rather than inside `parameters`, the
    // same mistake they already make with `intent`. The guard runs per sub-call
    // on that sub-call's parameters, so a flag left at the wrong level is
    // silently dropped and the sub-call withheld again.
    let input = serde_json::json!({
        "tool_calls": [{
            "tool": "agentgrep",
            "accept_large_output": true,
            "parameters": { "query": "x" },
        }]
    });
    let out = super::normalize_batch_input(input);
    assert_eq!(
        out["tool_calls"][0]["parameters"][jcode_tool_core::ACCEPT_LARGE_OUTPUT_KEY],
        serde_json::json!(true),
        "flag beside `tool` must reach the sub-call parameters"
    );
}

#[test]
fn subcall_level_accept_large_output_does_not_override_an_explicit_value() {
    // An explicit `false` inside parameters is a deliberate choice for that one
    // sub-call and must win over a blanket flag beside `tool`.
    let input = serde_json::json!({
        "tool_calls": [{
            "tool": "agentgrep",
            "accept_large_output": true,
            "parameters": { "query": "x", "accept_large_output": false },
        }]
    });
    let out = super::normalize_batch_input(input);
    assert_eq!(
        out["tool_calls"][0]["parameters"][jcode_tool_core::ACCEPT_LARGE_OUTPUT_KEY],
        serde_json::json!(false),
        "explicit per-subcall value must win"
    );
}
