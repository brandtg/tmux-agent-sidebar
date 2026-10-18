use serde_json::Value;

use crate::event::{AgentEvent, AgentEventKind, EventAdapter};
use crate::tmux::HERMES_AGENT;
use crate::tool_name::CanonicalTool;

use super::{EventBase, HookRegistration, json_str, json_value_or_null, optional_str};

pub struct HermesAdapter;

/// Hermes payloads carry only `session_id` and `cwd` at the top level — no
/// `permission_mode`, worktree, or subagent identity. Everything event-specific
/// (prompts, responses, approval kinds, child roles) arrives under `extra`.
fn base(input: &Value) -> EventBase {
    EventBase::new(input, HERMES_AGENT)
}

/// The `extra` dict of the shell-hook wire payload: every event-specific kwarg
/// the fire site passes lands there (see `agent/shell_hooks.py::_payload_fields`).
fn extra(input: &Value) -> Value {
    json_value_or_null(input, "extra")
}

/// Read a string field from the `extra` dict.
fn extra_str<'a>(input: &'a Value, key: &str) -> &'a str {
    input
        .get("extra")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// Hermes shell hooks pass `hook_event_name` on stdin, so one external sidebar
/// event can be produced by two different upstream triggers (see the
/// `user-prompt-submit` arm). This is only used for disambiguation; the
/// `event_name` argument remains the source of truth for the arm selection.
fn upstream_event(input: &Value) -> &str {
    json_str(input, "hook_event_name")
}

/// Hermes tool names are lowercase snake_case (`terminal`, `read_file`, …);
/// normalize the common ones onto the canonical Claude-style vocabulary so the
/// activity log's label strategies (command/file extraction) fire. Unknown
/// names pass through unchanged, exactly like the OpenCode adapter.
fn normalize_tool_name(raw: &str) -> String {
    let canonical = match raw {
        "terminal" => CanonicalTool::Bash,
        "read_file" => CanonicalTool::Read,
        "write_file" => CanonicalTool::Write,
        "patch" => CanonicalTool::Edit,
        "glob" => CanonicalTool::Glob,
        "grep" => CanonicalTool::Grep,
        "web_search" => CanonicalTool::WebSearch,
        "web_extract" => CanonicalTool::WebFetch,
        "delegate_task" => CanonicalTool::Agent,
        "clarify" => CanonicalTool::AskUserQuestion,
        other => return other.to_string(),
    };
    canonical.as_str().to_string()
}

/// Hermes file tools take a `path` argument; the label extractor wants
/// Claude-style `file_path`. Detached terminal spawns carry `background: true`
/// while the shared activity-log handler tracks background shells by Claude's
/// `run_in_background` key. Both are added alongside the originals so
/// raw-payload consumers still see them (same convention as the OpenCode
/// adapter).
fn normalize_tool_input(tool_name: &str, input: Value) -> Value {
    let mut input = input;
    if matches!(tool_name, "Read" | "Write" | "Edit")
        && let Value::Object(map) = &mut input
        && !map.contains_key("file_path")
        && let Some(path) = map.get("path").cloned()
    {
        map.insert("file_path".into(), path);
    }
    if tool_name == CanonicalTool::Bash.as_str()
        && let Value::Object(map) = &mut input
        && map.get("background").and_then(|v| v.as_bool()) == Some(true)
        && !map.contains_key("run_in_background")
    {
        map.insert("run_in_background".into(), Value::Bool(true));
    }
    input
}

/// `post_tool_call`'s `result` kwarg is a JSON-encoded string. Parse it back
/// into a value so the activity log sees structured responses like Claude's;
/// non-JSON strings are wrapped as-is.
fn parse_result(input: &Value) -> Value {
    let raw = extra_str(input, "result");
    if raw.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(raw).unwrap_or(Value::String(raw.to_string()))
}

/// Map a `on_human_input_request` kind onto the sidebar's wait-reason
/// vocabulary. All three kinds block on a person, so they must stay
/// `waiting` even when a background shell is alive — `permission` and
/// `question` are already in `is_permission_wait_reason`.
///
/// Unknown kinds (additive upstream) are reported as meta-only: agent meta
/// is refreshed but no attention flag is raised.
fn wait_reason_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "approval" | "sudo" => Some("permission"),
        "clarify" => Some("question"),
        _ => None,
    }
}

impl HermesAdapter {
    /// Single source of truth for Hermes shell-hook wiring, verified against
    /// `hermes_cli.plugins.VALID_HOOKS` and the shell-hook wire protocol in
    /// `agent/shell_hooks.py` (stdin `{hook_event_name, tool_name, tool_input,
    /// session_id, cwd, profile, extra}`).
    ///
    /// Caveats:
    /// - `on_session_end` is deliberately NOT wired: despite its name it fires
    ///   after *every turn* (the real teardown hook is `on_session_finalize`).
    /// - `on_human_input_resolved` maps to the same internal event as
    ///   `pre_llm_call` (`user-prompt-submit`): the user just answered, the
    ///   agent resumes. The arm disambiguates via `hook_event_name`.
    /// - `subagent_start`/`subagent_stop` are delegation-only; they carry the
    ///   child role rather than a tool-agent type.
    /// - `post_tool_call` also feeds background-shell tracking: a
    ///   `terminal(background=true)` spawn is translated to Claude's
    ///   `run_in_background` flag, so when the turn ends (`post_llm_call` →
    ///   `stop`) the pane resolves to `background` instead of `idle` while
    ///   the detached process is alive. The result's `pid` is stored in
    ///   `@pane_bg_pid` so the liveness sweep tracks the process by PID —
    ///   Hermes spawns detached (`start_new_session`) under a login shell,
    ///   and orchestrator commands carry quotes and `$(…)` that the raw
    ///   command string can never match against shell-expanded ps args.
    pub const HOOK_REGISTRATIONS: &'static [HookRegistration] = &[
        HookRegistration {
            trigger: "on_session_start",
            matcher: None,
            kind: AgentEventKind::SessionStart,
        },
        HookRegistration {
            trigger: "pre_llm_call",
            matcher: None,
            kind: AgentEventKind::UserPromptSubmit,
        },
        HookRegistration {
            trigger: "post_llm_call",
            matcher: None,
            kind: AgentEventKind::Stop,
        },
        HookRegistration {
            trigger: "post_tool_call",
            matcher: None,
            kind: AgentEventKind::ActivityLog,
        },
        HookRegistration {
            trigger: "on_human_input_request",
            matcher: None,
            kind: AgentEventKind::Notification,
        },
        HookRegistration {
            trigger: "on_human_input_resolved",
            matcher: None,
            kind: AgentEventKind::UserPromptSubmit,
        },
        HookRegistration {
            trigger: "on_session_finalize",
            matcher: None,
            kind: AgentEventKind::SessionEnd,
        },
        HookRegistration {
            trigger: "subagent_start",
            matcher: None,
            kind: AgentEventKind::SubagentStart,
        },
        HookRegistration {
            trigger: "subagent_stop",
            matcher: None,
            kind: AgentEventKind::SubagentStop,
        },
    ];
}

impl EventAdapter for HermesAdapter {
    fn parse(&self, event_name: &str, input: &Value) -> Option<AgentEvent> {
        match event_name {
            // Shell hooks put the session id and cwd at the top level, so the
            // shared EventBase picks them up; the model/platform context in
            // `extra` has no sidebar equivalent.
            "session-start" => Some(base(input).session_start(String::new())),
            // Two upstream triggers produce this event. `pre_llm_call` is
            // Hermes' UserPromptSubmit equivalent (it even documents Claude
            // Code's UserPromptSubmit as "use pre_llm_call here");
            // `on_human_input_resolved` is the reply to a blocking prompt —
            // the wait is over either way. A `cancelled` outcome (the
            // prompt never reached a human: callback raised, interrupted
            // read, …) still ENDS the wait, so it must clear the
            // attention ring and wait reason; dropping the event used to
            // leave the pane flagged `waiting` until the next hook. The
            // empty prompt keeps the run clock running instead of
            // restarting it.
            "user-prompt-submit" => {
                if upstream_event(input) == "on_human_input_resolved" {
                    return Some(base(input).user_prompt_submit(String::new()));
                }
                Some(base(input).user_prompt_submit(json_str(&extra(input), "user_message").into()))
            }
            "stop" => {
                Some(base(input).stop(json_str(&extra(input), "assistant_response").into(), None))
            }
            "notification" => {
                let kind = extra_str(input, "kind");
                match wait_reason_for_kind(kind) {
                    Some(wait_reason) => Some(base(input).notification(wait_reason.into(), false)),
                    // Additive upstream kinds we don't understand: refresh
                    // agent meta without raising attention.
                    None => Some(base(input).notification(String::new(), true)),
                }
            }
            "session-end" => Some(AgentEvent::SessionEnd {
                end_reason: extra_str(input, "reason").into(),
                session_id: optional_str(input, "session_id"),
            }),
            "subagent-start" => {
                // Drift-test minimal payloads use Claude's `agent_type` key;
                // real Hermes payloads carry `child_role` under `extra`.
                let role = match extra_str(input, "child_role") {
                    "" => json_str(input, "agent_type"),
                    role => role,
                };
                if role.is_empty() {
                    return None;
                }
                Some(AgentEvent::SubagentStart {
                    agent_type: role.into(),
                    agent_id: optional_str(&extra(input), "child_subagent_id"),
                })
            }
            "subagent-stop" => {
                let role = match extra_str(input, "child_role") {
                    "" => json_str(input, "agent_type"),
                    role => role,
                };
                if role.is_empty() {
                    return None;
                }
                Some(AgentEvent::SubagentStop {
                    agent_type: role.into(),
                    agent_id: optional_str(&extra(input), "child_subagent_id"),
                    last_message: json_str(&extra(input), "child_summary").into(),
                    transcript_path: String::new(),
                })
            }
            "activity-log" => {
                let raw_name = json_str(input, "tool_name");
                if raw_name.is_empty() {
                    return None;
                }
                let tool_name = normalize_tool_name(raw_name);
                let tool_input =
                    normalize_tool_input(&tool_name, json_value_or_null(input, "tool_input"));
                Some(AgentEvent::ActivityLog {
                    tool_name,
                    tool_input,
                    tool_response: parse_result(input),
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

    /// Wrap event-specific kwargs in the shell-hook envelope the way
    /// `agent/shell_hooks.py::_serialize_payload` does.
    fn payload(hook_event_name: &str, extra: Value) -> Value {
        json!({
            "hook_event_name": hook_event_name,
            "tool_name": Value::Null,
            "tool_input": Value::Null,
            "session_id": "sess_h1",
            "cwd": "/repo",
            "profile": "default",
            "extra": extra,
        })
    }

    #[test]
    fn hook_registrations_match_parse_arms() {
        super::super::assert_table_drift_free("hermes", HermesAdapter::HOOK_REGISTRATIONS);
    }

    #[test]
    fn session_start() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "session-start",
                &payload("on_session_start", json!({"model": "m", "platform": "cli"})),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionStart {
                agent: HERMES_AGENT.into(),
                cwd: "/repo".into(),
                permission_mode: "".into(),
                source: "".into(),
                worktree: None,
                agent_id: None,
                session_id: Some("sess_h1".into()),
            }
        );
    }

    #[test]
    fn pre_llm_call_maps_to_user_prompt_submit() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "user-prompt-submit",
                &payload("pre_llm_call", json!({"user_message": "fix the bug"})),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { agent, prompt, .. } => {
                assert_eq!(agent, "hermes");
                assert_eq!(prompt, "fix the bug");
            }
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
    }

    #[test]
    fn pre_llm_call_multimodal_message_yields_empty_prompt() {
        // A multimodal turn's user_message is a list of content parts, not a
        // string; json_str must degrade to empty rather than panic.
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "user-prompt-submit",
                &payload("pre_llm_call", json!({"user_message": [{"type": "text"}]})),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, ""),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
    }

    #[test]
    fn human_input_resolved_maps_to_resume() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "user-prompt-submit",
                &payload(
                    "on_human_input_resolved",
                    json!({"kind": "approval", "outcome": "once"}),
                ),
            )
            .unwrap();
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, ""),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
    }

    #[test]
    fn human_input_resolved_cancelled_still_clears_the_wait() {
        // A `cancelled` outcome means the prompt never reached a human
        // (callback raised, interrupted read, …) — but the wait IS over.
        // The event must still clear the attention ring and wait reason;
        // dropping it used to leave the pane flagged `waiting` until the
        // next hook fired.
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "user-prompt-submit",
                &payload(
                    "on_human_input_resolved",
                    json!({"kind": "approval", "outcome": "cancelled"}),
                ),
            )
            .expect("cancelled resolution must still map to an event");
        match event {
            AgentEvent::UserPromptSubmit { prompt, .. } => assert_eq!(prompt, ""),
            other => panic!("expected UserPromptSubmit, got {:?}", other),
        }
    }

    #[test]
    fn post_llm_call_maps_to_stop() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "stop",
                &payload("post_llm_call", json!({"assistant_response": "all done"})),
            )
            .unwrap();
        match event {
            AgentEvent::Stop {
                agent,
                last_message,
                response,
                ..
            } => {
                assert_eq!(agent, "hermes");
                assert_eq!(last_message, "all done");
                assert!(response.is_none());
            }
            other => panic!("expected Stop, got {:?}", other),
        }
    }

    #[test]
    fn human_input_request_approval_maps_to_permission_wait() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "notification",
                &payload(
                    "on_human_input_request",
                    json!({"kind": "approval", "prompt": "rm -rf /tmp/x"}),
                ),
            )
            .unwrap();
        match event {
            AgentEvent::Notification {
                wait_reason,
                meta_only,
                ..
            } => {
                assert_eq!(wait_reason, "permission");
                assert!(!meta_only);
            }
            other => panic!("expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn human_input_request_clarify_maps_to_question_wait() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "notification",
                &payload("on_human_input_request", json!({"kind": "clarify"})),
            )
            .unwrap();
        match event {
            AgentEvent::Notification { wait_reason, .. } => assert_eq!(wait_reason, "question"),
            other => panic!("expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn human_input_request_unknown_kind_is_meta_only() {
        // Additive upstream kinds must not raise attention.
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "notification",
                &payload("on_human_input_request", json!({"kind": "telepathy"})),
            )
            .unwrap();
        match event {
            AgentEvent::Notification { meta_only, .. } => assert!(meta_only),
            other => panic!("expected Notification, got {:?}", other),
        }
    }

    #[test]
    fn session_finalize_maps_to_session_end() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "session-end",
                &payload("on_session_finalize", json!({"reason": "quit"})),
            )
            .unwrap();
        assert_eq!(
            event,
            AgentEvent::SessionEnd {
                end_reason: "quit".into(),
                session_id: Some("sess_h1".into()),
            }
        );
    }

    #[test]
    fn post_tool_call_normalizes_names_and_parses_result() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "hook_event_name": "post_tool_call",
                    "tool_name": "read_file",
                    "tool_input": {"path": "/repo/src/main.rs"},
                    "session_id": "sess_h1",
                    "cwd": "/repo",
                    "extra": {"result": "{\"content\": \"hi\"}", "status": "success"}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog {
                tool_name,
                tool_input,
                tool_response,
            } => {
                assert_eq!(tool_name, "Read");
                assert_eq!(tool_input["file_path"], "/repo/src/main.rs");
                assert_eq!(tool_input["path"], "/repo/src/main.rs");
                assert_eq!(tool_response["content"], "hi");
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn post_tool_call_terminal_maps_to_bash() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "hook_event_name": "post_tool_call",
                    "tool_name": "terminal",
                    "tool_input": {"command": "cargo test"},
                    "extra": {}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_name, .. } => assert_eq!(tool_name, "Bash"),
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn post_tool_call_background_terminal_sets_run_in_background() {
        // `terminal(background=true)` is hermes' detached-spawn dialect; the
        // activity-log handler keys background-shell tracking on Claude's
        // `run_in_background`. Translating the flag is what makes the
        // turn-ending stop resolve to `background` (green) while the process
        // is alive instead of `idle`.
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "hook_event_name": "post_tool_call",
                    "tool_name": "terminal",
                    "tool_input": {"command": "opencode run --title m3", "background": true},
                    "extra": {}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_input, .. } => {
                assert_eq!(tool_input["run_in_background"], true);
                assert_eq!(
                    tool_input["background"], true,
                    "the original key must survive for raw-payload consumers"
                );
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn post_tool_call_foreground_terminal_gets_no_background_flag() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "hook_event_name": "post_tool_call",
                    "tool_name": "terminal",
                    "tool_input": {"command": "cargo test", "background": false},
                    "extra": {}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_input, .. } => {
                assert!(tool_input.get("run_in_background").is_none());
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn post_tool_call_non_json_result_is_wrapped() {
        let adapter = HermesAdapter;
        let event = adapter
            .parse(
                "activity-log",
                &json!({
                    "hook_event_name": "post_tool_call",
                    "tool_name": "terminal",
                    "tool_input": {"command": "ls"},
                    "extra": {"result": "plain text output"}
                }),
            )
            .unwrap();
        match event {
            AgentEvent::ActivityLog { tool_response, .. } => {
                assert_eq!(tool_response, Value::String("plain text output".into()));
            }
            other => panic!("expected ActivityLog, got {:?}", other),
        }
    }

    #[test]
    fn post_tool_call_without_tool_name_is_dropped() {
        let adapter = HermesAdapter;
        assert!(
            adapter
                .parse("activity-log", &payload("post_tool_call", json!({})))
                .is_none()
        );
    }

    #[test]
    fn subagent_events_use_child_role() {
        let adapter = HermesAdapter;
        let start = adapter
            .parse(
                "subagent-start",
                &payload(
                    "subagent_start",
                    json!({"child_role": "leaf", "child_subagent_id": "sa-1", "child_goal": "do it"}),
                ),
            )
            .unwrap();
        match start {
            AgentEvent::SubagentStart {
                agent_type,
                agent_id,
            } => {
                assert_eq!(agent_type, "leaf");
                assert_eq!(agent_id, Some("sa-1".into()));
            }
            other => panic!("expected SubagentStart, got {:?}", other),
        }

        let stop = adapter
            .parse(
                "subagent-stop",
                &payload(
                    "subagent_stop",
                    json!({"child_role": "leaf", "child_summary": "done", "child_status": "completed"}),
                ),
            )
            .unwrap();
        match stop {
            AgentEvent::SubagentStop {
                agent_type,
                last_message,
                ..
            } => {
                assert_eq!(agent_type, "leaf");
                assert_eq!(last_message, "done");
            }
            other => panic!("expected SubagentStop, got {:?}", other),
        }
    }

    #[test]
    fn subagent_events_accept_drift_test_minimal_payload() {
        // The shared drift-test helper models Claude's payload shape
        // (`agent_type` at the top level); the adapter must accept it.
        let adapter = HermesAdapter;
        assert!(
            adapter
                .parse("subagent-start", &json!({"agent_type": "Explore"}))
                .is_some()
        );
        assert!(
            adapter
                .parse("subagent-stop", &json!({"agent_type": "Explore"}))
                .is_some()
        );
    }

    #[test]
    fn unsupported_events_are_ignored() {
        let adapter = HermesAdapter;
        // Claude/Codex-only external names and hermes events we don't wire.
        for name in [
            "stop-failure",
            "permission-denied",
            "cwd-changed",
            "session-title",
            "task-created",
            "teammate-idle",
        ] {
            assert!(adapter.parse(name, &json!({})).is_none(), "{name}");
        }
    }
}
