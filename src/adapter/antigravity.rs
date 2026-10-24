use std::io::{Read, Seek, SeekFrom};

use serde_json::Value;

use crate::event::{AgentEvent, AgentEventKind, EventAdapter};
use crate::tmux::ANTIGRAVITY_AGENT;
use crate::tool_name::CanonicalTool;

use super::{EventBase, HookRegistration, json_str, json_value_or_null, optional_str};

pub struct AntigravityAdapter;

/// How much of the transcript tail is read per hook fire. Turn steps are one
/// JSON line each and tool outputs dominate the file, so this covers any
/// realistic prompt-to-Stop distance while bounding the per-event I/O.
const TRANSCRIPT_TAIL_BYTES: u64 = 512 * 1024;

/// Antigravity payloads carry a `workspacePaths` array (first entry is the
/// workspace root) and a `conversationId` instead of Claude-style `cwd` /
/// `session_id` — verified against real CLI 1.4.2 payloads. There is no
/// `permission_mode`, worktree object, or subagent id.
fn base(input: &Value) -> EventBase {
    EventBase::new(input, ANTIGRAVITY_AGENT)
        .with_cwd(first_workspace_path(input))
        .with_session_id(optional_str(input, "conversationId"))
}

/// Antigravity mounts workspaces as a list; the first one anchors repo
/// grouping the way a single `cwd` does for the other agents.
fn first_workspace_path(input: &Value) -> String {
    input
        .get("workspacePaths")
        .and_then(|v| v.as_array())
        .and_then(|paths| paths.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

impl AntigravityAdapter {
    /// Single source of truth for Antigravity CLI hook wiring. Verified
    /// against the official hook docs (antigravity.google/docs/hooks/) and
    /// real `agy` 1.4.2 payloads: the supported trigger set is
    /// `SessionStart`, `PreToolUse`, `PostToolUse`, `PreInvocation`,
    /// `PostInvocation`, `Stop`.
    ///
    /// Caveats:
    /// - `PreInvocation` fires before *every* model call in a turn, not
    ///   just the first, and carries no prompt text. It still maps to
    ///   `user-prompt-submit`: the prompt is recovered from the tail of the
    ///   transcript the payload points at (see `transcript_prompt`), and an
    ///   empty prompt degrades to a status-only update.
    /// - `PreToolUse` / `PostInvocation` are supported upstream but not
    ///   wired: PreToolUse's payload cannot distinguish an approval
    ///   prompt from an auto-approved call, and PostInvocation adds
    ///   nothing the Stop hook does not already cover.
    /// - Hook stdout is parsed by agy as the hook response (an invalid
    ///   payload aborts the run), so unlike Codex the Stop arm must print
    ///   nothing — `response` stays `None`.
    pub const HOOK_REGISTRATIONS: &'static [HookRegistration] = &[
        HookRegistration {
            trigger: "SessionStart",
            matcher: None,
            kind: AgentEventKind::SessionStart,
        },
        HookRegistration {
            trigger: "PreInvocation",
            matcher: None,
            kind: AgentEventKind::UserPromptSubmit,
        },
        HookRegistration {
            trigger: "PostToolUse",
            matcher: None,
            kind: AgentEventKind::ActivityLog,
        },
        HookRegistration {
            trigger: "Stop",
            matcher: None,
            kind: AgentEventKind::Stop,
        },
    ];
}

/// Antigravity tool names are lowercase snake_case (`run_command`,
/// `write_to_file`, …); normalize the common ones onto the canonical
/// Claude-style vocabulary so the activity log's label strategies
/// (command/file extraction) fire. Unknown names
/// pass through unchanged, exactly like the Hermes adapter.
fn normalize_tool_name(raw: &str) -> String {
    let canonical = match raw {
        "run_command" => CanonicalTool::Bash,
        "view_file" => CanonicalTool::Read,
        "write_to_file" => CanonicalTool::Write,
        "replace_file_content" | "multi_replace_file_content" => CanonicalTool::Edit,
        "find_by_name" => CanonicalTool::Glob,
        "grep_search" => CanonicalTool::Grep,
        "search_web" => CanonicalTool::WebSearch,
        "read_url_content" => CanonicalTool::WebFetch,
        "invoke_subagent" => CanonicalTool::Agent,
        "send_message" => CanonicalTool::SendMessage,
        "ask_question" => CanonicalTool::AskUserQuestion,
        other => return other.to_string(),
    };
    canonical.as_str().to_string()
}

/// Antigravity tool arguments use PascalCase keys (`CommandLine`,
/// `TargetFile`, …); the label extractor wants Claude-style lowercase
/// keys. Canonical keys are added alongside the originals so raw-payload
/// consumers still see them (same convention as the Hermes adapter).
fn normalize_tool_input(tool_name: &str, input: Value) -> Value {
    let mut input = input;
    let Value::Object(map) = &mut input else {
        return input;
    };
    let alias = |map: &mut serde_json::Map<String, Value>, from: &str, to: &str| {
        if !map.contains_key(to)
            && let Some(value) = map.get(from).cloned()
        {
            map.insert(to.to_string(), value);
        }
    };
    match tool_name {
        "run_command" => alias(map, "CommandLine", "command"),
        "view_file" => alias(map, "AbsolutePath", "file_path"),
        "write_to_file" | "replace_file_content" | "multi_replace_file_content" => {
            alias(map, "TargetFile", "file_path");
        }
        "find_by_name" => alias(map, "Pattern", "pattern"),
        "grep_search" => alias(map, "Query", "pattern"),
        "search_web" => {}
        "read_url_content" => alias(map, "Url", "url"),
        "send_message" => alias(map, "Recipient", "to"),
        "ask_question" => {}
        _ => {}
    }
    input
}

/// `PostToolUse` reports tool failures through a top-level `error` string;
/// surface it as an object so response-shaped consumers see the failure
/// the way Claude payloads present one.
fn tool_response(input: &Value) -> Value {
    let error = json_str(input, "error");
    if error.is_empty() {
        Value::Null
    } else {
        serde_json::json!({ "error": error })
    }
}

/// Read the last [`TRANSCRIPT_TAIL_BYTES`] of `path`, aligned to the first
/// complete line. Every agy hook payload carries `transcriptPath`; the
/// transcript is the only source for prompt and response text, which no
/// hook payload includes. Returns "" when the path is empty or the file is
/// missing/unreadable — callers degrade to the previous no-text behavior.
fn transcript_tail(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let mut tail = String::from_utf8_lossy(&buf).into_owned();
    // Mid-file starts begin inside a line; drop the partial first line so
    // `lines()` only sees complete JSON records.
    if start > 0
        && let Some(first_newline) = tail.find('\n')
    {
        tail.drain(..=first_newline);
    }
    tail
}

/// Content of the last transcript step whose `type` matches, parsed
/// line-by-line from the tail. Lines that fail to parse, name a different
/// step type, or carry no content are skipped — the transcript is an
/// upstream implementation detail and must degrade, never error.
fn last_step_content(tail: &str, step_type: &str) -> Option<String> {
    let mut found = None;
    for line in tail.lines() {
        // Cheap pre-filter: one substring check per line instead of a JSON
        // parse per line on transcripts with hundreds of steps. The bare
        // type name is enough of a filter — false positives only cost a
        // failed parse + type check.
        if !line.contains(step_type) {
            continue;
        }
        let Ok(step) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if step.get("type").and_then(|v| v.as_str()) != Some(step_type) {
            continue;
        }
        let content = step.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }
        found = Some(content.to_string());
    }
    found
}

/// The user's prompt for the turn, from the newest `USER_INPUT` transcript
/// step. agy wraps it in a `<USER_REQUEST>` envelope followed by an
/// `<ADDITIONAL_METADATA>` block (timestamp, cwd, …) — only the envelope's
/// inner text is the prompt as typed.
fn transcript_prompt(input: &Value) -> String {
    let raw = last_step_content(
        &transcript_tail(json_str(input, "transcriptPath")),
        "USER_INPUT",
    )
    .unwrap_or_default();
    match raw.split_once("<USER_REQUEST>\n") {
        Some((_, rest)) => match rest.split_once("\n</USER_REQUEST>") {
            Some((prompt, _)) => prompt.to_string(),
            None => rest.to_string(),
        },
        None => raw,
    }
}

/// The final assistant response, from the last `PLANNER_RESPONSE` step with
/// content. `GENERIC` steps are tool-result renderings, and tool-call turns
/// carry `tool_calls` without content — both are skipped, leaving the last
/// visible model message, which is what the `▷ ...` response preview shows.
fn transcript_response(input: &Value) -> String {
    last_step_content(
        &transcript_tail(json_str(input, "transcriptPath")),
        "PLANNER_RESPONSE",
    )
    .unwrap_or_default()
}

impl EventAdapter for AntigravityAdapter {
    fn parse(&self, event_name: &str, input: &Value) -> Option<AgentEvent> {
        match event_name {
            "session-start" => Some(base(input).session_start(String::new())),
            // PreInvocation carries no prompt text; recover it from the
            // transcript — see the caveat on HOOK_REGISTRATIONS.
            "user-prompt-submit" => Some(base(input).user_prompt_submit(transcript_prompt(input))),
            "stop" => {
                // Normal completions end with e.g. `model_stop` /
                // `NO_TOOL_CALL`; a turn that died from a system error
                // carries reason `error` and/or a populated `error` field.
                // Anything else is a completed turn.
                let reason = json_str(input, "terminationReason");
                let error = json_str(input, "error");
                if reason == "error" || !error.is_empty() {
                    let detail = if error.is_empty() { reason } else { error };
                    return Some(base(input).stop_failure(detail.to_string()));
                }
                // The Stop payload carries no assistant message; pull the
                // final response from the transcript. agy parses hook stdout
                // as the response — nothing may be printed, so unlike Codex
                // there is no synthesized marker (response: None).
                let last_message = transcript_response(input);
                Some(base(input).stop(last_message, None))
            }
            "activity-log" => {
                let tool_call = json_value_or_null(input, "toolCall");
                let raw_name = json_str(&tool_call, "name");
                if raw_name.is_empty() {
                    return None;
                }
                Some(AgentEvent::ActivityLog {
                    tool_name: normalize_tool_name(raw_name),
                    tool_input: normalize_tool_input(
                        raw_name,
                        json_value_or_null(&tool_call, "args"),
                    ),
                    tool_response: tool_response(input),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Full SessionStart payload captured from agy 1.4.2 in print mode.
    fn session_start_payload() -> Value {
        json!({
            "conversationId": "ce7a82bf-3a73-463e-88b9-b7b8966e9809",
            "workspacePaths": ["/tmp/opencode/agy-probe"],
            "transcriptPath": "~/.gemini/antigravity-cli/brain/ce7a82bf/.system_generated/logs/transcript.jsonl",
            "artifactDirectoryPath": "~/.gemini/antigravity-cli/brain/ce7a82bf",
            "modelName": "gemini-3.8-flash-high"
        })
    }

    #[test]
    fn hook_registrations_match_parse_arms() {
        super::super::assert_table_drift_free(
            "antigravity",
            AntigravityAdapter::HOOK_REGISTRATIONS,
        );
    }

    #[test]
    fn session_start_extracts_workspace_and_conversation() {
        let event = AntigravityAdapter
            .parse("session-start", &session_start_payload())
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionStart {
                agent: ANTIGRAVITY_AGENT.into(),
                cwd: "/tmp/opencode/agy-probe".into(),
                permission_mode: "".into(),
                source: "".into(),
                worktree: None,
                agent_id: None,
                session_id: Some("ce7a82bf-3a73-463e-88b9-b7b8966e9809".into()),
            }
        );
    }

    #[test]
    fn session_start_missing_fields_default_to_empty() {
        let event = AntigravityAdapter
            .parse("session-start", &json!({}))
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionStart {
                agent: "antigravity".into(),
                cwd: "".into(),
                permission_mode: "".into(),
                source: "".into(),
                worktree: None,
                agent_id: None,
                session_id: None,
            }
        );
    }

    #[test]
    fn user_prompt_submit_has_no_prompt_text() {
        // PreInvocation fires per model invocation with no prompt text;
        // the handler treats the empty prompt as status-only.
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({
                    "conversationId": "conv-1",
                    "workspacePaths": ["/tmp/w"],
                    "invocationNum": 3,
                    "initialNumSteps": 7
                }),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::UserPromptSubmit {
                agent: ANTIGRAVITY_AGENT.into(),
                cwd: "/tmp/w".into(),
                permission_mode: "".into(),
                prompt: "".into(),
                worktree: None,
                agent_id: None,
                session_id: Some("conv-1".into()),
            }
        );
    }

    #[test]
    fn stop_is_a_plain_completed_turn() {
        // Real payload: normal completions carry a non-error reason and an
        // empty `error` string.
        let event = AntigravityAdapter
            .parse(
                "stop",
                &json!({
                    "executionNum": 0,
                    "terminationReason": "NO_TOOL_CALL",
                    "error": "",
                    "fullyIdle": true,
                    "conversationId": "conv-2",
                    "workspacePaths": ["/tmp/w"]
                }),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::Stop {
                agent: ANTIGRAVITY_AGENT.into(),
                cwd: "/tmp/w".into(),
                permission_mode: "".into(),
                last_message: "".into(),
                // agy parses hook stdout as the response — nothing may be
                // printed, so unlike Codex there is no synthesized marker.
                response: None,
                worktree: None,
                agent_id: None,
                session_id: Some("conv-2".into()),
            }
        );
    }

    #[test]
    fn stop_with_error_termination_maps_to_stop_failure() {
        let event = AntigravityAdapter
            .parse(
                "stop",
                &json!({
                    "terminationReason": "error",
                    "error": "socket disconnect: broken pipe",
                    "conversationId": "conv-3",
                    "workspacePaths": ["/tmp/w"]
                }),
            )
            .unwrap();
        match event {
            AgentEvent::StopFailure { error, .. } => {
                assert_eq!(error, "socket disconnect: broken pipe");
            }
            other => panic!("expected StopFailure, got {:?}", other),
        }
    }

    #[test]
    fn stop_with_non_error_reason_stays_a_stop() {
        // Only the documented `error` reason (or a populated `error`
        // field) is a failure; real non-error reasons include
        // `NO_TOOL_CALL` and `model_stop`.
        for reason in ["NO_TOOL_CALL", "model_stop", "max_steps_exceeded"] {
            let event = AntigravityAdapter
                .parse("stop", &json!({"terminationReason": reason, "error": ""}))
                .unwrap();
            assert!(matches!(event, AgentEvent::Stop { .. }), "{reason}");
        }
    }

    #[test]
    fn stop_failure_not_a_separate_event_name() {
        // The Stop trigger produces both variants based on payload content;
        // there is no upstream `stop-failure` trigger.
        assert!(
            AntigravityAdapter
                .parse("stop-failure", &json!({}))
                .is_none()
        );
    }

    #[test]
    fn activity_log_normalizes_run_command() {
        // Real PostToolUse payload for run_command.
        let event = AntigravityAdapter
            .parse(
                "activity-log",
                &json!({
                    "toolCall": {
                        "name": "run_command",
                        "args": {
                            "CommandLine": "ls -la",
                            "Cwd": "/tmp/w",
                            "WaitMsBeforeAsync": 5000
                        }
                    },
                    "stepIdx": 2,
                    "error": ""
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog {
                tool_name,
                tool_input,
                ..
            } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(
                    tool_input.get("command").and_then(|v| v.as_str()),
                    Some("ls -la")
                );
                // Original keys survive alongside the canonical alias.
                assert_eq!(
                    tool_input.get("CommandLine").and_then(|v| v.as_str()),
                    Some("ls -la")
                );
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn activity_log_normalizes_file_tools() {
        let event = AntigravityAdapter
            .parse(
                "activity-log",
                &json!({
                    "toolCall": {
                        "name": "write_to_file",
                        "args": {
                            "TargetFile": "/tmp/w/probe.txt",
                            "CodeContent": "hello\n",
                            "Overwrite": true
                        }
                    },
                    "error": ""
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog {
                tool_name,
                tool_input,
                ..
            } => {
                assert_eq!(tool_name, "Write");
                assert_eq!(
                    tool_input.get("file_path").and_then(|v| v.as_str()),
                    Some("/tmp/w/probe.txt")
                );
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn activity_log_normalizes_research_tools() {
        for (raw, canonical) in [
            ("grep_search", "Grep"),
            ("search_web", "WebSearch"),
            ("read_url_content", "WebFetch"),
            ("find_by_name", "Glob"),
        ] {
            let event = AntigravityAdapter
                .parse(
                    "activity-log",
                    &json!({"toolCall": {"name": raw, "args": {}}, "error": ""}),
                )
                .unwrap();
            match event {
                AgentEvent::ActivityLog { tool_name, .. } => assert_eq!(tool_name, canonical),
                other => panic!("expected ActivityLog, got {:?}", other),
            }
        }
    }

    #[test]
    fn activity_log_unknown_tool_passes_through() {
        let event = AntigravityAdapter
            .parse(
                "activity-log",
                &json!({"toolCall": {"name": "generate_image", "args": {"Prompt": "cat"}}, "error": ""}),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_name, .. } => assert_eq!(tool_name, "generate_image"),
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn activity_log_tool_failure_surfaces_error() {
        let event = AntigravityAdapter
            .parse(
                "activity-log",
                &json!({
                    "toolCall": {"name": "run_command", "args": {"CommandLine": "make"}},
                    "error": "exit status 1"
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_response, .. } => {
                assert_eq!(
                    tool_response.get("error").and_then(|v| v.as_str()),
                    Some("exit status 1")
                );
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn activity_log_without_tool_call_rejected() {
        assert!(
            AntigravityAdapter
                .parse("activity-log", &json!({}))
                .is_none()
        );
        assert!(
            AntigravityAdapter
                .parse("activity-log", &json!({"toolCall": {"args": {}}}))
                .is_none()
        );
    }

    #[test]
    fn user_prompt_submit_extracts_prompt_from_transcript() {
        // Fixture mirrors the real transcript_full.jsonl record shape.
        let dir = std::env::temp_dir().join("agy_adapter_prompt");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nCreate e2e.txt with hello, then run ls -la\\n</USER_REQUEST>\\n<ADDITIONAL_METADATA>\\nThe current local time is 4pm\\n</ADDITIONAL_METADATA>\"}\n",
                "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"content\":\"plan text\"}\n"
            ),
        )
        .unwrap();
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({
                    "conversationId": "conv-1",
                    "workspacePaths": ["/tmp/w"],
                    "transcriptPath": path.to_string_lossy()
                }),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => {
                // Envelope and metadata block are stripped; the inner text
                // is the prompt as typed.
                assert_eq!(prompt, "Create e2e.txt with hello, then run ls -la");
            }
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_prompt_submit_with_unwrapped_input_uses_raw_content() {
        let dir = std::env::temp_dir().join("agy_adapter_prompt_raw");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"USER_INPUT\",\"content\":\"plain prompt\"}\n",
        )
        .unwrap();
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({"transcriptPath": path.to_string_lossy()}),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, "plain prompt"),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_prompt_submit_takes_the_newest_user_input() {
        // A mid-turn PreInvocation must show the latest queued message, not
        // the one that started the turn.
        let dir = std::env::temp_dir().join("agy_adapter_prompt_newest");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nfirst\\n</USER_REQUEST>\"}\n",
                "{\"type\":\"PLANNER_RESPONSE\",\"content\":\"working on it\"}\n",
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nsecond\\n</USER_REQUEST>\"}\n"
            ),
        )
        .unwrap();
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({"transcriptPath": path.to_string_lossy()}),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, "second"),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stop_extracts_final_response_from_transcript() {
        let dir = std::env::temp_dir().join("agy_adapter_stop");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\ndo it\\n</USER_REQUEST>\"}\n",
                // Tool-call turn: PLANNER_RESPONSE with no content — skipped.
                "{\"type\":\"PLANNER_RESPONSE\",\"tool_calls\":[{\"name\":\"run_command\"}]}\n",
                // Tool-result rendering: GENERIC, skipped even with content.
                "{\"type\":\"GENERIC\",\"content\":\"tool output dump\"}\n",
                "{\"type\":\"PLANNER_RESPONSE\",\"content\":\"Created e2e.txt with hello.\\n\\n### Command output\"}\n"
            ),
        )
        .unwrap();
        let event = AntigravityAdapter
            .parse(
                "stop",
                &json!({
                    "terminationReason": "NO_TOOL_CALL",
                    "error": "",
                    "fullyIdle": true,
                    "transcriptPath": path.to_string_lossy()
                }),
            )
            .unwrap();
        match event {
            AgentEvent::Stop { last_message, .. } => {
                assert_eq!(
                    last_message,
                    "Created e2e.txt with hello.\n\n### Command output"
                );
            }
            other => panic!("expected Stop, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_transcript_degrades_to_empty_text() {
        // No transcriptPath (drift-test payloads), a missing file, and a
        // directory path all yield empty prompt/response rather than errors.
        let event = AntigravityAdapter
            .parse("user-prompt-submit", &json!({}))
            .unwrap();
        assert!(matches!(
            event,
            AgentEvent::UserPromptSubmit { prompt, .. } if prompt.is_empty()
        ));
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({"transcriptPath": "/nonexistent/agy/transcript.jsonl"}),
            )
            .unwrap();
        assert!(matches!(
            event,
            AgentEvent::UserPromptSubmit { prompt, .. } if prompt.is_empty()
        ));
        let event = AntigravityAdapter
            .parse(
                "stop",
                &json!({"terminationReason": "model_stop", "error": ""}),
            )
            .unwrap();
        assert!(matches!(
            event,
            AgentEvent::Stop { last_message, .. } if last_message.is_empty()
        ));
    }

    #[test]
    fn transcript_tail_skips_partial_first_line() {
        // A tail that starts mid-record must not mis-parse the truncated
        // line: it contains the type marker but is not valid JSON, so the
        // parser skips it and the following complete line wins.
        let dir = std::env::temp_dir().join("agy_adapter_tail");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nfirst\\n</USER_REQUEST>\"}\n",
                "_INPUT\",\"content\":\"<USER_REQUEST>\\nsecond\\n</USER_REQUEST>\"}\n",
                "{\"type\":\"USER_INPUT\",\"content\":\"<USER_REQUEST>\\nthird\\n</USER_REQUEST>\"}\n"
            ),
        )
        .unwrap();
        let event = AntigravityAdapter
            .parse(
                "user-prompt-submit",
                &json!({"transcriptPath": path.to_string_lossy()}),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, "third"),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pre_tool_use_and_post_invocation_not_wired() {
        // Supported upstream but deliberately not registered — see the
        // caveats on HOOK_REGISTRATIONS.
        assert!(
            AntigravityAdapter
                .parse("notification", &json!({}))
                .is_none()
        );
    }

    #[test]
    fn unsupported_kinds_rejected() {
        for name in [
            "session-end",
            "session-title",
            "stop-failure",
            "permission-denied",
            "cwd-changed",
            "subagent-start",
            "subagent-stop",
            "task-created",
            "task-completed",
            "teammate-idle",
        ] {
            assert!(
                AntigravityAdapter.parse(name, &json!({})).is_none(),
                "{name}"
            );
        }
    }
}
