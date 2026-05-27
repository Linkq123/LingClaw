use super::*;

#[test]
fn phase_transitions_happy_path() {
    let mut ctx = AgentLoopCtx::new(false);
    assert_eq!(ctx.phase(), AgentPhase::Analyze);

    // Analyze -> Act
    ctx.transition_to_act();
    assert_eq!(ctx.phase(), AgentPhase::Act);

    // Act -> Observe (2 tool calls)
    ctx.transition_to_observe(2);
    assert_eq!(ctx.phase(), AgentPhase::Observe);
    assert_eq!(ctx.tool_calls, 2);

    // Observe -> Analyze (new cycle)
    ctx.transition_to_analyze();
    assert_eq!(ctx.phase(), AgentPhase::Analyze);
    assert_eq!(ctx.cycles, 1);

    // Analyze -> Finish
    ctx.transition_to_finish(FinishReason::Complete);
    assert_eq!(ctx.phase(), AgentPhase::Finish);
    assert_eq!(ctx.finish_reason, Some(FinishReason::Complete));
}

#[test]
fn direct_finish_without_tools() {
    let mut ctx = AgentLoopCtx::new(false);
    assert_eq!(ctx.phase(), AgentPhase::Analyze);
    ctx.transition_to_finish(FinishReason::Empty);
    assert_eq!(ctx.phase(), AgentPhase::Finish);
    assert_eq!(ctx.cycles, 0);
    assert_eq!(ctx.tool_calls, 0);
    assert_eq!(ctx.finish_reason, Some(FinishReason::Empty));
}

#[test]
fn multi_cycle_tracking() {
    let mut ctx = AgentLoopCtx::new(true);
    for i in 0..5 {
        ctx.transition_to_act();
        ctx.transition_to_observe(1);
        assert_eq!(ctx.tool_calls, i + 1);
        ctx.transition_to_analyze();
    }
    assert_eq!(ctx.cycles, 5);
    assert_eq!(ctx.tool_calls, 5);
    ctx.transition_to_finish(FinishReason::Complete);
    assert_eq!(ctx.phase(), AgentPhase::Finish);
}

#[test]
#[should_panic(expected = "Act requires Analyze")]
fn invalid_act_from_observe() {
    let mut ctx = AgentLoopCtx::new(false);
    ctx.transition_to_act();
    ctx.transition_to_observe(1);
    ctx.transition_to_act(); // wrong: should go to Analyze first
}

#[test]
#[should_panic(expected = "Finish requires Analyze")]
fn invalid_finish_from_act() {
    let mut ctx = AgentLoopCtx::new(false);
    ctx.transition_to_act();
    ctx.transition_to_finish(FinishReason::Complete); // wrong: should be in Analyze
}

#[test]
fn observation_annotation_short() {
    let short = "ok";
    assert_eq!(maybe_annotate_observation("exec", short), "ok");
}

#[test]
fn observation_annotation_long() {
    let long = "x\n".repeat(3000);
    let annotated = maybe_annotate_observation("exec", &long);
    assert!(annotated.starts_with("[Observation: exec returned"));
    assert!(annotated.contains("3000 lines"));
    assert!(annotated.ends_with(&long));
}

#[test]
fn finish_heuristic() {
    assert!(is_finish(true, false));
    assert!(!is_finish(true, true));
    assert!(!is_finish(false, false));
    assert!(is_empty_finish(false, false));
    assert!(!is_empty_finish(true, false));
}

#[test]
fn evaluate_finish_returns_correct_reasons() {
    // Tool calls -> continue (None)
    assert_eq!(evaluate_finish(true, true), None);
    assert_eq!(evaluate_finish(false, true), None);
    // Content, no tools -> Complete
    assert_eq!(evaluate_finish(true, false), Some(FinishReason::Complete));
    // No content, no tools -> Empty
    assert_eq!(evaluate_finish(false, false), Some(FinishReason::Empty));
}

#[test]
fn evaluate_finish_ignores_task_state_readiness_and_blockers() {
    let mut state = seeded_state("fix the timeout handling");
    state.uncertainties.push(UncertaintyItem {
        topic: "timeout verification".into(),
        reason: "the write path was not inspected yet".into(),
        blocking: true,
    });
    state.recompute_ready_to_finish();
    assert!(!state.ready_to_finish);
    assert!(state.has_blocking_uncertainty());

    assert_eq!(evaluate_finish(true, false), Some(FinishReason::Complete));
}

#[test]
fn evaluate_finish_returns_complete_for_action_oriented_reply_without_tools() {
    let mut state = seeded_state("implement retry handling in the CLI");
    state.recompute_ready_to_finish();
    assert!(!state.ready_to_finish);

    assert_eq!(evaluate_finish(true, false), Some(FinishReason::Complete));
}

fn seeded_state(query: &str) -> WorkingState {
    let mut state = WorkingState::default();
    state.seed_from_query(Some(query));
    state
}

#[test]
fn task_intent_classifies_common_queries() {
    assert_eq!(
        TaskIntent::classify(Some("what does main.rs do?")),
        TaskIntent::Inform
    );
    assert_eq!(
        TaskIntent::classify(Some("implement retry handling in the CLI")),
        TaskIntent::Change
    );
    assert_eq!(
        TaskIntent::classify(Some("diagnose why cargo test is timing out")),
        TaskIntent::Investigate
    );
    assert_eq!(
        TaskIntent::classify(Some("run cargo test --workspace")),
        TaskIntent::Execute
    );
}

#[test]
fn seed_from_query_refreshes_goal_and_intent_for_new_intervention() {
    let mut state = seeded_state("what does config do?");
    state
        .completed_steps
        .push("read_file succeeded in 2ms (call c1).".into());
    state.evidence.push(EvidenceItem {
        claim: "Observed file content: timeout_ms = 30".into(),
        source_tool: "read_file".into(),
        source_ref: "c1".into(),
        confidence: EvidenceConfidence::High,
    });
    state.open_questions.push("Why is timeout_ms 30?".into());
    state
        .next_actions
        .push("Check the timeout caller before concluding.".into());
    state.uncertainties.push(UncertaintyItem {
        topic: "timeout handling".into(),
        reason: "the write path was not inspected yet".into(),
        blocking: true,
    });
    state.recompute_ready_to_finish();
    assert!(!state.ready_to_finish);

    state.seed_from_query(Some("fix the timeout handling"));

    assert_eq!(state.intent, TaskIntent::Change);
    assert_eq!(
        state.primary_goal.as_deref(),
        Some("fix the timeout handling")
    );
    assert!(state.completed_steps.is_empty());
    assert!(state.evidence.is_empty());
    assert!(state.open_questions.is_empty());
    assert!(state.next_actions.is_empty());
    assert!(state.uncertainties.is_empty());
    assert!(!state.ready_to_finish);
}

#[test]
fn seed_from_query_preserves_state_for_follow_up_continuation_query() {
    let mut state = seeded_state("fix the timeout handling");
    state
        .completed_steps
        .push("execution progress: patch `src/config.rs` via patch_file in 3ms (call c1).".into());
    state.evidence.push(EvidenceItem {
        claim: "Observed file content: timeout_ms = 45".into(),
        source_tool: "read_file".into(),
        source_ref: "c2".into(),
        confidence: EvidenceConfidence::High,
    });
    state.recompute_ready_to_finish();

    state.seed_from_query(Some("next step"));

    assert_eq!(state.intent, TaskIntent::Change);
    assert_eq!(
        state.primary_goal.as_deref(),
        Some("fix the timeout handling")
    );
    assert_eq!(state.completed_steps.len(), 1);
    assert_eq!(state.evidence.len(), 1);
}

#[test]
fn seed_from_query_preserves_state_for_polite_or_particle_continuation_query() {
    for query in ["continue please", "please continue", "继续一下"] {
        let mut state = seeded_state("fix the timeout handling");
        state.completed_steps.push(
            "execution progress: patch `src/config.rs` via patch_file in 3ms (call c1).".into(),
        );
        state.evidence.push(EvidenceItem {
            claim: "Observed file content: timeout_ms = 45".into(),
            source_tool: "read_file".into(),
            source_ref: "c2".into(),
            confidence: EvidenceConfidence::High,
        });
        state.recompute_ready_to_finish();

        state.seed_from_query(Some(query));

        assert_eq!(
            state.intent,
            TaskIntent::Change,
            "{query} should preserve intent"
        );
        assert_eq!(
            state.primary_goal.as_deref(),
            Some("fix the timeout handling"),
            "{query} should preserve goal"
        );
        assert_eq!(
            state.completed_steps.len(),
            1,
            "{query} should keep progress"
        );
        assert_eq!(state.evidence.len(), 1, "{query} should keep evidence");
    }
}

#[test]
fn seed_from_query_updates_goal_for_continuation_phrase_with_redirect() {
    let mut state = seeded_state("fix the installer");
    state
        .completed_steps
        .push("execution progress: patch `install.ps1` via patch_file in 3ms (call c1).".into());
    state.uncertainties.push(UncertaintyItem {
        topic: "installer cleanup".into(),
        reason: "the timeout path has not been benchmarked yet".into(),
        blocking: true,
    });
    state.recompute_ready_to_finish();

    state.seed_from_query(Some("next step: benchmark the timeout path instead"));

    assert_eq!(state.intent, TaskIntent::Execute);
    assert_eq!(
        state.primary_goal.as_deref(),
        Some("next step: benchmark the timeout path instead")
    );
    assert!(state.completed_steps.is_empty());
    assert!(state.uncertainties.is_empty());
    assert!(!state.ready_to_finish);
}

#[test]
fn seed_from_query_resets_state_for_short_change_redirect() {
    let mut state = seeded_state("fix the installer");
    state
        .completed_steps
        .push("execution progress: patch `install.ps1` via patch_file in 3ms (call c1).".into());
    state.evidence.push(EvidenceItem {
        claim: "Observed file content: install.ps1 writes the helper script".into(),
        source_tool: "read_file".into(),
        source_ref: "install.ps1".into(),
        confidence: EvidenceConfidence::High,
    });
    state
        .open_questions
        .push("Should install.ps1 be retried?".into());
    state.uncertainties.push(UncertaintyItem {
        topic: "installer cleanup".into(),
        reason: "the helper script still needs verification".into(),
        blocking: true,
    });
    state.recompute_ready_to_finish();

    state.seed_from_query(Some("fix the parser"));

    assert_eq!(state.intent, TaskIntent::Change);
    assert_eq!(state.primary_goal.as_deref(), Some("fix the parser"));
    assert!(state.completed_steps.is_empty());
    assert!(state.evidence.is_empty());
    assert!(state.open_questions.is_empty());
    assert!(state.uncertainties.is_empty());
    assert!(!state.ready_to_finish);
}

#[test]
fn text_mentions_short_single_token_anchor() {
    assert!(text_mentions_anchor(
        "Please inspect README before editing anything else.",
        "README"
    ));
    assert!(!text_mentions_anchor(
        "This note mentions timeouts and retries.",
        "me"
    ));
}

#[test]
fn text_mentions_command_anchor_requires_exact_command_match() {
    assert!(text_mentions_anchor(
        "Run `cargo test --workspace -- --nocapture` before wrapping up.",
        "cargo test --workspace"
    ));
    assert!(!text_mentions_anchor(
        "Run `cargo check --workspace` before wrapping up.",
        "cargo test --workspace"
    ));
}

#[test]
fn chinese_task_result_counts_as_action_and_change_progress() {
    let mut change_state = seeded_state("修复安装脚本");
    apply_rule_based_working_state_update(
        &mut change_state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "task".into(),
            result: "已修复并更新安装配置".into(),
            duration_ms: 20,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "delegate to `reviewer`".into(),
                agent: Some("reviewer".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );
    assert!(change_state.has_successful_change_trace());
    assert!(change_state.ready_to_finish);

    let mut execute_state = seeded_state("运行工作区测试");
    apply_rule_based_working_state_update(
        &mut execute_state,
        &[ToolResultEntry {
            id: "c2".into(),
            name: "task".into(),
            result: "已执行并通过验证".into(),
            duration_ms: 20,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "delegate to `reviewer`".into(),
                agent: Some("reviewer".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );
    assert!(execute_state.has_successful_execution_trace());
    assert!(execute_state.ready_to_finish);
}

#[test]
fn chinese_exec_success_text_counts_as_execution_progress_without_trace() {
    let mut state = seeded_state("运行工作区测试");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c-exec".into(),
            name: "exec".into(),
            result: "已通过测试并构建成功".into(),
            duration_ms: 20,
            is_error: false,
            call_summary: None,
            trace: None,
        }],
    );

    assert!(state.has_successful_execution_trace());
    assert!(!state.has_successful_change_trace());
    assert!(state.ready_to_finish);
}

#[test]
fn push_unique_evidence_dedupes_ephemeral_call_ids() {
    let mut evidence = Vec::new();
    push_unique_evidence(
        &mut evidence,
        EvidenceItem {
            claim: "Observed file content: timeout_ms = 45".into(),
            source_tool: "read_file".into(),
            source_ref: "tool_call_1234567890_1".into(),
            confidence: EvidenceConfidence::High,
        },
        WORKING_STATE_MAX_ITEMS,
    );
    push_unique_evidence(
        &mut evidence,
        EvidenceItem {
            claim: "Observed file content: timeout_ms = 45".into(),
            source_tool: "read_file".into(),
            source_ref: "550e8400-e29b-41d4-a716-446655440000".into(),
            confidence: EvidenceConfidence::High,
        },
        WORKING_STATE_MAX_ITEMS,
    );

    assert_eq!(evidence.len(), 1);
}

#[test]
fn push_unique_evidence_keeps_distinct_stable_refs() {
    let mut evidence = Vec::new();
    push_unique_evidence(
        &mut evidence,
        EvidenceItem {
            claim: "Observed file content: timeout_ms = 45".into(),
            source_tool: "read_file".into(),
            source_ref: "src/config.rs".into(),
            confidence: EvidenceConfidence::High,
        },
        WORKING_STATE_MAX_ITEMS,
    );
    push_unique_evidence(
        &mut evidence,
        EvidenceItem {
            claim: "Observed file content: timeout_ms = 45".into(),
            source_tool: "read_file".into(),
            source_ref: "src/runtime_loop.rs".into(),
            confidence: EvidenceConfidence::High,
        },
        WORKING_STATE_MAX_ITEMS,
    );

    assert_eq!(evidence.len(), 2);
}

#[test]
fn merge_state_digest_delta_does_not_trust_optimistic_ready_flag() {
    let mut state = seeded_state("implement retry handling in the CLI");

    merge_state_digest_delta(
        &mut state,
        StateDigestDelta {
            completed_steps: vec!["read src/retry.rs".into()],
            ready_to_finish: true,
            ..StateDigestDelta::default()
        },
    );

    assert_eq!(state.completed_steps.len(), 1);
    assert!(!state.ready_to_finish);
}

#[test]
fn auto_think_level_runtime_uses_observation_strength_and_task_pressure() {
    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            cycles: 1,
            observation_strength: AutoObservationStrength::Strong,
            user_msg_chars: 80,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "high"
    );

    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            cycles: 0,
            user_msg_chars: 80,
            task_pressure: 2,
            action_oriented: true,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "high"
    );
}

#[test]
fn auto_think_level_runtime_uses_progress_sensitive_late_loop_decay() {
    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            intent: TaskIntent::Investigate,
            cycles: 8,
            task_pressure: 1,
            ready_to_finish: false,
            action_oriented: true,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "medium"
    );

    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            intent: TaskIntent::Investigate,
            cycles: 8,
            task_pressure: 3,
            ready_to_finish: false,
            action_oriented: true,
            has_blocking_uncertainty: true,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "xhigh"
    );

    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            cycles: 8,
            ready_to_finish: true,
            progress_made: true,
            evidence_delta_quality: AutoEvidenceDeltaQuality::BetterEvidence,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "low"
    );
}

#[test]
fn auto_think_level_runtime_escalates_on_stagnation_and_error_streak() {
    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            intent: TaskIntent::Investigate,
            cycles: 7,
            stagnation_streak: 3,
            ready_to_finish: false,
            action_oriented: true,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "high"
    );

    assert_eq!(
        auto_think_decision_runtime(AutoThinkRuntimeSignals {
            intent: TaskIntent::Investigate,
            cycles: 7,
            error_streak: 4,
            stagnation_streak: 2,
            ready_to_finish: false,
            action_oriented: true,
            ..AutoThinkRuntimeSignals::default()
        })
        .selected_level
        .label(),
        "max"
    );
}

#[test]
fn auto_think_level_runtime_escalates_on_low_value_retries() {
    let repeated_retry = auto_think_decision_runtime(AutoThinkRuntimeSignals {
        intent: TaskIntent::Investigate,
        cycles: 6,
        action_oriented: true,
        retry_pattern: AutoRetryPattern::SameArgs,
        ..AutoThinkRuntimeSignals::default()
    });

    assert_eq!(repeated_retry.selected_level.label(), "xhigh");
    assert!(
        repeated_retry
            .escalators
            .contains(&"retry_same_args".to_string())
    );
    assert!(
        !repeated_retry
            .dampeners
            .contains(&"late_loop_decay".to_string())
    );
}

#[test]
fn auto_retry_pattern_treats_distinct_task_prompts_as_same_tool() {
    let first_trace = crate::tools::build_tool_execution_trace(
        "task",
        Some(r#"{"agent":"reviewer","prompt":"inspect the parser timeout path"}"#),
    )
    .expect("task trace should be built");
    let second_trace = crate::tools::build_tool_execution_trace(
        "task",
        Some(r#"{"agent":"reviewer","prompt":"inspect the runtime reconnect logic"}"#),
    )
    .expect("task trace should be built");

    let pattern = auto_retry_pattern(&[
        ToolResultEntry {
            id: "task-1".into(),
            name: "task".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(first_trace),
        },
        ToolResultEntry {
            id: "task-2".into(),
            name: "task".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(second_trace),
        },
    ]);

    assert_eq!(pattern, AutoRetryPattern::SameTool);
}

#[test]
fn auto_retry_pattern_treats_identical_task_prompts_as_same_args() {
    let first_trace = crate::tools::build_tool_execution_trace(
        "task",
        Some(r#"{"agent":"reviewer","prompt":"inspect the parser timeout path"}"#),
    )
    .expect("task trace should be built");
    let second_trace = crate::tools::build_tool_execution_trace(
        "task",
        Some(r#"{"agent":"reviewer","prompt":"inspect the parser timeout path"}"#),
    )
    .expect("task trace should be built");

    let pattern = auto_retry_pattern(&[
        ToolResultEntry {
            id: "task-1".into(),
            name: "task".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(first_trace),
        },
        ToolResultEntry {
            id: "task-2".into(),
            name: "task".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(second_trace),
        },
    ]);

    assert_eq!(pattern, AutoRetryPattern::SameArgs);
}

#[test]
fn auto_retry_pattern_treats_distinct_orchestrations_with_same_width_as_same_tool() {
    let first_trace = crate::tools::build_tool_execution_trace(
        "orchestrate",
        Some(
            r#"{"tasks":[{"id":"a","agent":"reviewer","prompt":"trace startup"},{"id":"b","agent":"coder","prompt":"trace shutdown","depends_on":["a"]}]}"#,
        ),
    )
    .expect("orchestrate trace should be built");
    let second_trace = crate::tools::build_tool_execution_trace(
        "orchestrate",
        Some(
            r#"{"tasks":[{"id":"a","agent":"reviewer","prompt":"trace websocket"},{"id":"b","agent":"coder","prompt":"trace reconnect","depends_on":["a"]}]}"#,
        ),
    )
    .expect("orchestrate trace should be built");

    let pattern = auto_retry_pattern(&[
        ToolResultEntry {
            id: "orch-1".into(),
            name: "orchestrate".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("orchestrate 2 delegated tasks".into()),
            trace: Some(first_trace),
        },
        ToolResultEntry {
            id: "orch-2".into(),
            name: "orchestrate".into(),
            result: "delegated".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("orchestrate 2 delegated tasks".into()),
            trace: Some(second_trace),
        },
    ]);

    assert_eq!(pattern, AutoRetryPattern::SameTool);
}

#[test]
fn auto_retry_pattern_treats_distinct_exec_commands_in_same_dir_as_same_tool() {
    let first_trace = crate::tools::build_tool_execution_trace(
        "exec",
        Some(r#"{"command":"cargo test auto_trace","working_dir":"src"}"#),
    )
    .expect("exec trace should be built");
    let second_trace = crate::tools::build_tool_execution_trace(
        "exec",
        Some(r#"{"command":"cargo test replay_live_round","working_dir":"src"}"#),
    )
    .expect("exec trace should be built");

    let pattern = auto_retry_pattern(&[
        ToolResultEntry {
            id: "exec-1".into(),
            name: "exec".into(),
            result: "done".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("run `cargo test auto_trace` in `src`".into()),
            trace: Some(first_trace),
        },
        ToolResultEntry {
            id: "exec-2".into(),
            name: "exec".into(),
            result: "done".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("run `cargo test replay_live_round` in `src`".into()),
            trace: Some(second_trace),
        },
    ]);

    assert_eq!(pattern, AutoRetryPattern::SameTool);
}

#[test]
fn auto_retry_pattern_treats_distinct_read_windows_on_same_file_as_same_tool() {
    let first_trace = crate::tools::build_tool_execution_trace(
        "read_file",
        Some(r#"{"path":"src/main.rs","start_line":1,"end_line":40}"#),
    )
    .expect("read_file trace should be built");
    let second_trace = crate::tools::build_tool_execution_trace(
        "read_file",
        Some(r#"{"path":"src/main.rs","start_line":80,"end_line":120}"#),
    )
    .expect("read_file trace should be built");

    let pattern = auto_retry_pattern(&[
        ToolResultEntry {
            id: "read-1".into(),
            name: "read_file".into(),
            result: "fn first() {}".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("read `src/main.rs` lines 1-40".into()),
            trace: Some(first_trace),
        },
        ToolResultEntry {
            id: "read-2".into(),
            name: "read_file".into(),
            result: "fn second() {}".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("read `src/main.rs` lines 80-120".into()),
            trace: Some(second_trace),
        },
    ]);

    assert_eq!(pattern, AutoRetryPattern::SameTool);
}

#[test]
fn rule_update_adds_completed_steps_and_evidence() {
    let mut state = seeded_state("which file sets the timeout?");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "search_files".into(),
            result: "src/config.rs: timeout_ms = 30".into(),
            duration_ms: 4,
            is_error: false,
            call_summary: None,
            trace: None,
        }],
    );

    assert_eq!(state.completed_steps.len(), 1);
    assert_eq!(state.evidence.len(), 1);
    assert!(state.ready_to_finish);
}

#[test]
fn rule_update_does_not_treat_read_only_probe_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "read_file".into(),
            result: "fn retry() {}".into(),
            duration_ms: 4,
            is_error: false,
            call_summary: Some("read `src/retry.rs`".into()),
            trace: None,
        }],
    );

    assert_eq!(state.completed_steps.len(), 1);
    assert!(state.has_confirmed_evidence());
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_does_not_treat_read_only_exec_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "E:/work/workspace/vibe-coding/LingClaw".into(),
            duration_ms: 4,
            is_error: false,
            call_summary: Some("run `pwd`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "run `pwd`".into(),
                command: Some("pwd".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert_eq!(state.completed_steps.len(), 1);
    assert!(!state.has_successful_execution_trace());
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_does_not_treat_validation_exec_alone_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.34s".into(),
            duration_ms: 24,
            is_error: false,
            call_summary: Some("run `cargo check`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "run `cargo check`".into(),
                command: Some("cargo check".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert!(state.has_successful_execution_trace());
    assert!(!state.has_successful_change_trace());
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_does_not_treat_probe_plus_validation_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[
            ToolResultEntry {
                id: "c1".into(),
                name: "read_file".into(),
                result: "fn retry() { /* TODO */ }".into(),
                duration_ms: 4,
                is_error: false,
                call_summary: Some("read `src/retry.rs`".into()),
                trace: Some(ToolExecutionTrace {
                    summary: "read `src/retry.rs`".into(),
                    path: Some("src/retry.rs".into()),
                    ..ToolExecutionTrace::default()
                }),
            },
            ToolResultEntry {
                id: "c2".into(),
                name: "exec".into(),
                result: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.34s"
                    .into(),
                duration_ms: 24,
                is_error: false,
                call_summary: Some("run `cargo check`".into()),
                trace: Some(ToolExecutionTrace {
                    summary: "run `cargo check`".into(),
                    command: Some("cargo check".into()),
                    ..ToolExecutionTrace::default()
                }),
            },
        ],
    );

    assert!(state.has_confirmed_evidence());
    assert!(state.has_successful_execution_trace());
    assert!(!state.has_successful_change_trace());
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_treats_patch_file_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "patch_file".into(),
            result: "patched successfully".into(),
            duration_ms: 4,
            is_error: false,
            call_summary: Some("patch `src/retry.rs`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "patch `src/retry.rs`".into(),
                path: Some("src/retry.rs".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert!(state.has_successful_change_trace());
    assert!(state.ready_to_finish);
}

#[test]
fn rule_update_treats_write_and_delete_file_as_change_completion() {
    for tool_name in ["write_file", "delete_file"] {
        let mut state = seeded_state("implement retry handling in the CLI");
        apply_rule_based_working_state_update(
            &mut state,
            &[ToolResultEntry {
                id: format!("{tool_name}-1"),
                name: tool_name.into(),
                result: "ok".into(),
                duration_ms: 4,
                is_error: false,
                call_summary: Some(format!("{tool_name} `src/retry.rs`")),
                trace: Some(ToolExecutionTrace {
                    summary: format!("{tool_name} `src/retry.rs`"),
                    path: Some("src/retry.rs".into()),
                    ..ToolExecutionTrace::default()
                }),
            }],
        );

        assert!(
            state.has_successful_execution_trace(),
            "{tool_name} should count as execution progress"
        );
        assert!(
            state.has_successful_change_trace(),
            "{tool_name} should count as change progress"
        );
        assert!(state.ready_to_finish, "{tool_name} should unlock finish");
    }
}

#[test]
fn rule_update_treats_validation_exec_as_execution_progress() {
    let mut state = seeded_state("run the workspace tests");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "test result: ok. 42 passed; 0 failed".into(),
            duration_ms: 24,
            is_error: false,
            call_summary: Some("run `cargo test --workspace`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "run `cargo test --workspace`".into(),
                command: Some("cargo test --workspace".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert!(state.has_successful_execution_trace());
    assert!(state.ready_to_finish);
}

#[test]
fn rule_update_treats_orchestrate_fix_summary_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "orch-1".into(),
            name: "orchestrate".into(),
            result: "Implemented and validated the retry fix across delegated tasks.".into(),
            duration_ms: 40,
            is_error: false,
            call_summary: Some("orchestrate 2 delegated tasks".into()),
            trace: Some(ToolExecutionTrace {
                summary: "orchestrate 2 delegated tasks".into(),
                task_count: Some(2),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert!(state.has_successful_execution_trace());
    assert!(state.has_successful_change_trace());
    assert!(state.ready_to_finish);
}

#[test]
fn rule_update_does_not_treat_metadata_or_version_exec_as_execution_progress() {
    for command in [
        "cargo metadata --format-version 1",
        "cargo fmt --check",
        "rustc --version",
        "git rev-parse HEAD",
    ] {
        let mut state = seeded_state("implement retry handling in the CLI");
        apply_rule_based_working_state_update(
            &mut state,
            &[ToolResultEntry {
                id: "c1".into(),
                name: "exec".into(),
                result: "ok".into(),
                duration_ms: 4,
                is_error: false,
                call_summary: Some(format!("run `{command}`")),
                trace: Some(ToolExecutionTrace {
                    summary: format!("run `{command}`"),
                    command: Some(command.into()),
                    ..ToolExecutionTrace::default()
                }),
            }],
        );

        assert!(
            !state.has_successful_execution_trace(),
            "{command} should stay read-only"
        );
        assert!(!state.ready_to_finish, "{command} should not unlock finish");
    }
}

#[test]
fn rule_update_treats_namespaced_npm_run_scripts_as_execution_progress() {
    for command in ["npm run build:prod", "npm run test:unit"] {
        let mut state = seeded_state("run the frontend validation scripts");
        apply_rule_based_working_state_update(
            &mut state,
            &[ToolResultEntry {
                id: "c1".into(),
                name: "exec".into(),
                result: "ok".into(),
                duration_ms: 4,
                is_error: false,
                call_summary: Some(format!("run `{command}`")),
                trace: Some(ToolExecutionTrace {
                    summary: format!("run `{command}`"),
                    command: Some(command.into()),
                    ..ToolExecutionTrace::default()
                }),
            }],
        );

        assert!(
            state.has_successful_execution_trace(),
            "{command} should count as execution progress"
        );
        assert!(state.ready_to_finish, "{command} should unlock finish");
    }
}

#[test]
fn rule_update_does_not_treat_research_task_as_change_completion() {
    let mut state = seeded_state("implement retry handling in the CLI");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "task".into(),
            result: "I reviewed the retry path and found the root cause in src/retry.rs.".into(),
            duration_ms: 40,
            is_error: false,
            call_summary: Some("delegate to `reviewer`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "delegate to `reviewer`".into(),
                agent: Some("reviewer".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
    );

    assert!(!state.has_successful_execution_trace());
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_records_blocking_uncertainty_on_error() {
    let mut state = seeded_state("diagnose why the build is failing");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c2".into(),
            name: "exec".into(),
            result: "cargo test exited with status 101".into(),
            duration_ms: 15,
            is_error: true,
            call_summary: Some("run `cargo test --workspace`".into()),
            trace: None,
        }],
    );

    assert!(state.has_blocking_uncertainty());
    assert_eq!(state.open_questions.len(), 1);
    assert_eq!(state.next_actions.len(), 1);
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_with_memory_adds_relevant_recovery_actions_after_failure() {
    let mut state = seeded_state("run the workspace tests");
    let task_memory = crate::memory::RetrievedTaskMemory {
        open_loops: vec![crate::memory::OpenLoop {
            goal: "workspace test flow".into(),
            blocker: "command choice is inconsistent".into(),
            next_step: "standardize on cargo test --workspace".into(),
            status: crate::memory::OpenLoopStatus::Open,
            updated_at: 10,
        }],
        lessons: vec![crate::memory::MemoryLesson {
            title: "Rust test loop".into(),
            when_to_apply: "before a full workspace pass".into(),
            recommendation: "Run cargo check before cargo test --workspace".into(),
            scope: "repo".into(),
            confidence: crate::memory::MemoryConfidence::High,
            last_seen_at: 20,
        }],
        command_patterns: vec![crate::memory::CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: crate::memory::MemoryConfidence::High,
            last_seen_at: 30,
        }],
        ..crate::memory::RetrievedTaskMemory::default()
    };

    apply_rule_based_working_state_update_with_memory(
        &mut state,
        &[ToolResultEntry {
            id: "c2".into(),
            name: "exec".into(),
            result: "cargo test exited with status 101".into(),
            duration_ms: 15,
            is_error: true,
            call_summary: Some("run `cargo test --workspace`".into()),
            trace: None,
        }],
        Some(&task_memory),
    );

    assert!(state.has_blocking_uncertainty());
    assert!(
        state
            .next_actions
            .iter()
            .any(|action| action.contains("cargo check before cargo test --workspace"))
    );
}

#[test]
fn unrelated_success_does_not_clear_previous_tool_failure() {
    let mut state = seeded_state("diagnose why the build is failing");
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "cargo test exited with status 101".into(),
            duration_ms: 15,
            is_error: true,
            call_summary: Some("run `cargo test --workspace`".into()),
            trace: None,
        }],
    );
    apply_rule_based_working_state_update(
        &mut state,
        &[ToolResultEntry {
            id: "c2".into(),
            name: "exec".into(),
            result: "E:/work/workspace/vibe-coding/LingClaw".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("run `pwd`".into()),
            trace: None,
        }],
    );

    assert!(state.has_blocking_uncertainty());
    assert_eq!(state.open_questions.len(), 1);
    assert_eq!(state.next_actions.len(), 1);
    assert!(!state.ready_to_finish);
}

#[test]
fn rule_update_with_memory_clears_anchor_matched_blocker_on_success() {
    let mut state = seeded_state("diagnose why the entrypoint wiring is failing");
    state.uncertainties.push(UncertaintyItem {
        topic: "read_file failure".into(),
        reason: "src/main.rs could not be inspected earlier".into(),
        blocking: true,
    });
    state
        .open_questions
        .push("Why was src/main.rs unreadable before?".into());
    state
        .next_actions
        .push("Inspect src/main.rs again before concluding.".into());

    let task_memory = crate::memory::RetrievedTaskMemory {
        project_signals: vec![crate::memory::ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 10,
        }],
        ..crate::memory::RetrievedTaskMemory::default()
    };

    apply_rule_based_working_state_update_with_memory(
        &mut state,
        &[ToolResultEntry {
            id: "c3".into(),
            name: "read_file".into(),
            result: "fn main() { println!(\"ok\"); }".into(),
            duration_ms: 4,
            is_error: false,
            call_summary: Some("read `src/main.rs`".into()),
            trace: None,
        }],
        Some(&task_memory),
    );

    assert!(!state.has_blocking_uncertainty());
    assert!(
        !state
            .open_questions
            .iter()
            .any(|question| question.contains("src/main.rs"))
    );
    assert!(
        !state
            .next_actions
            .iter()
            .any(|action| action.contains("src/main.rs"))
    );
    assert!(state.evidence.iter().any(|item| {
        item.claim
            .contains("Validated remembered anchor: src/main.rs")
    }));
    assert!(state.ready_to_finish);
}

#[test]
fn rule_update_with_memory_keeps_command_blocker_for_different_command() {
    let mut state = seeded_state("diagnose why the workspace tests are failing");
    state.uncertainties.push(UncertaintyItem {
        topic: "exec failure".into(),
        reason: "cargo test --workspace still exits with status 101".into(),
        blocking: true,
    });
    state
        .next_actions
        .push("Retry cargo test --workspace after inspecting the logs.".into());

    let task_memory = crate::memory::RetrievedTaskMemory {
        command_patterns: vec![crate::memory::CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: crate::memory::MemoryConfidence::High,
            last_seen_at: 10,
        }],
        ..crate::memory::RetrievedTaskMemory::default()
    };

    apply_rule_based_working_state_update_with_memory(
        &mut state,
        &[ToolResultEntry {
            id: "c3".into(),
            name: "exec".into(),
            result: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.34s".into(),
            duration_ms: 24,
            is_error: false,
            call_summary: Some("run `cargo check --workspace`".into()),
            trace: Some(ToolExecutionTrace {
                summary: "run `cargo check --workspace`".into(),
                command: Some("cargo check --workspace".into()),
                ..ToolExecutionTrace::default()
            }),
        }],
        Some(&task_memory),
    );

    assert!(state.has_blocking_uncertainty());
    assert!(
        state
            .next_actions
            .iter()
            .any(|action| action.contains("cargo test --workspace"))
    );
    assert!(!state.evidence.iter().any(|item| {
        item.claim
            .contains("Validated remembered anchor: cargo test --workspace")
    }));
}

#[test]
fn summarize_result_snippets_skips_filtered_prefix_lines() {
    let snippets = summarize_result_snippets("\n   \nuseful line\nsecond useful line", 1);

    assert_eq!(snippets, vec!["useful line".to_string()]);
}

#[test]
fn state_digest_trigger_matches_large_error_or_multi_tool_batches() {
    let short_ok = ToolResultEntry {
        id: "c1".into(),
        name: "read_file".into(),
        result: "ok".into(),
        duration_ms: 1,
        is_error: false,
        call_summary: None,
        trace: None,
    };
    assert!(!should_trigger_state_digest(std::slice::from_ref(
        &short_ok
    )));

    let big = ToolResultEntry {
        result: "x".repeat(OBSERVATION_SUMMARY_THRESHOLD + 1),
        ..short_ok.clone()
    };
    assert!(should_trigger_state_digest(std::slice::from_ref(&big)));

    let error = ToolResultEntry {
        is_error: true,
        ..short_ok.clone()
    };
    assert!(should_trigger_state_digest(std::slice::from_ref(&error)));

    assert!(should_trigger_state_digest(&[
        short_ok.clone(),
        short_ok.clone(),
        short_ok
    ]));
}

#[test]
fn render_task_state_for_prompt_applies_budget_and_ordering() {
    let mut state = seeded_state("investigate the timeout path and summarize the result");
    for idx in 0..8 {
        state.completed_steps.push(format!("completed step {idx}"));
        state.evidence.push(EvidenceItem {
            claim: format!("evidence item {idx}"),
            source_tool: "search_files".into(),
            source_ref: format!("c{idx}"),
            confidence: EvidenceConfidence::High,
        });
    }
    state.open_questions = vec![
        "question one".into(),
        "question two".into(),
        "question three".into(),
        "question four".into(),
    ];
    state.next_actions = vec![
        "next action one".into(),
        "next action two".into(),
        "next action three".into(),
        "next action four".into(),
    ];

    let rendered = render_task_state_for_prompt(&state).expect("task state should render");
    assert!(rendered.starts_with("## Task State"));
    assert!(rendered.contains("- Goal:"));
    assert!(rendered.contains("- Completed:"));
    assert!(rendered.contains("- Evidence:"));
    assert!(rendered.contains("- Open Questions:"));
    assert!(rendered.contains("- Next Actions:"));
    assert!(rendered.len() <= 1_200);
}

#[test]
fn summarize_observations_empty_when_short() {
    let results = vec![ToolResultEntry {
        id: "c1".into(),
        name: "exec".into(),
        result: "short output".into(),
        duration_ms: 0,
        is_error: false,
        call_summary: Some("run `cargo test -q`".into()),
        trace: None,
    }];
    assert!(summarize_observations(&results).is_empty());
}

#[test]
fn summarize_observations_produces_summary_for_large() {
    let big = "x\n".repeat(3000);
    let results = vec![
        ToolResultEntry {
            id: "c1".into(),
            name: "read_file".into(),
            result: big.clone(),
            duration_ms: 0,
            is_error: false,
            call_summary: Some("read `src/main.rs`".into()),
            trace: None,
        },
        ToolResultEntry {
            id: "c2".into(),
            name: "exec".into(),
            result: "ok".into(),
            duration_ms: 0,
            is_error: false,
            call_summary: None,
            trace: None,
        },
    ];
    let summaries = summarize_observations(&results);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_name, "read_file");
    assert_eq!(summaries[0].byte_size, big.len());
    assert!(summaries[0].hint.contains("3000 lines"));
    assert!(summaries[0].hint.contains("read `src/main.rs`"));
}

#[test]
fn observation_context_hint_none_when_empty() {
    assert!(build_observation_context_hint(&[], 0).is_none());
}

#[test]
fn observation_context_hint_builds_markdown() {
    let summaries = vec![ObservationSummary {
        tool_call_id: "c1".into(),
        tool_name: "read_file".into(),
        byte_size: 5000,
        line_count: 100,
        hint: "read_file returned 100 lines / 5000 bytes - focus on key findings".into(),
    }];
    let hint = build_observation_context_hint(&summaries, 0).unwrap();
    assert!(hint.starts_with("## Recent Observation Notes"));
    assert!(hint.contains("**read_file**"));
    assert!(hint.contains("c1"));
}

#[test]
fn observation_hint_degradation_at_2_errors() {
    let hint = build_observation_context_hint(&[], 2).unwrap();
    assert!(hint.contains("2 consecutive tool errors"));
    assert!(hint.contains("alternative approach"));
}

#[test]
fn observation_hint_degradation_at_3_errors() {
    let hint = build_observation_context_hint(&[], 3).unwrap();
    assert!(hint.contains("3 consecutive tool errors"));
    assert!(hint.contains("not working"));
    assert!(hint.contains("different tool"));
}

#[test]
fn observation_hint_no_degradation_below_2() {
    assert!(build_observation_context_hint(&[], 0).is_none());
    assert!(build_observation_context_hint(&[], 1).is_none());
}

#[test]
fn finish_nudge_none_for_short_runs() {
    assert!(build_finish_nudge(0).is_none());
    assert!(build_finish_nudge(5).is_none());
    assert!(build_finish_nudge(14).is_none());
}

#[test]
fn finish_nudge_gentle_at_15() {
    let nudge = build_finish_nudge(15).unwrap();
    assert!(nudge.contains("Guidance"));
    assert!(nudge.contains("wrap up"));
}

#[test]
fn finish_nudge_strong_at_30() {
    let nudge = build_finish_nudge(30).unwrap();
    assert!(nudge.contains("Wrap Up Now"));
    assert!(nudge.contains("Do not start new tool calls"));
}

#[test]
fn simple_query_short_greetings() {
    assert!(is_simple_query("hello"));
    assert!(is_simple_query("hi there"));
    assert!(is_simple_query("what time is it?"));
    assert!(is_simple_query("who are you?"));
}

#[test]
fn simple_query_rejects_complex() {
    assert!(!is_simple_query("write a function to sort an array"));
    assert!(!is_simple_query("debug this error message"));
    assert!(!is_simple_query("implement a binary search tree"));
    assert!(!is_simple_query("explain how async/await works in Rust"));
    assert!(!is_simple_query("review the performance optimization plan"));
    assert!(!is_simple_query("analyze this:\nfn main() {}"));
    assert!(!is_simple_query(&"a".repeat(200)));
    // Chinese complex keywords
    assert!(!is_simple_query("帮我实现一个排序算法"));
    assert!(!is_simple_query("分析这段代码"));
    assert!(!is_simple_query("编写一个函数"));
}
