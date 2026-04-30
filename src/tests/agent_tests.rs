use super::*;

#[test]
fn phase_transitions_happy_path() {
    let mut ctx = AgentLoopCtx::new(false);
    assert_eq!(ctx.phase(), AgentPhase::Analyze);

    // Analyze → Act
    ctx.transition_to_act();
    assert_eq!(ctx.phase(), AgentPhase::Act);

    // Act → Observe (2 tool calls)
    ctx.transition_to_observe(2);
    assert_eq!(ctx.phase(), AgentPhase::Observe);
    assert_eq!(ctx.tool_calls, 2);

    // Observe → Analyze (new cycle)
    ctx.transition_to_analyze();
    assert_eq!(ctx.phase(), AgentPhase::Analyze);
    assert_eq!(ctx.cycles, 1);

    // Analyze → Finish
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
    // Tool calls → continue (None)
    assert_eq!(evaluate_finish(true, true), None);
    assert_eq!(evaluate_finish(false, true), None);
    // Content, no tools → Complete
    assert_eq!(evaluate_finish(true, false), Some(FinishReason::Complete));
    // No content, no tools → Empty
    assert_eq!(evaluate_finish(false, false), Some(FinishReason::Empty));
}

#[test]
fn finish_gate_defers_action_oriented_plan_only_reply_once() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 0,
            total_tool_calls: 0,
            has_observation_history: false,
            latest_user_query: Some("optimize the repo performance and apply the changes"),
            consecutive_errors: 0,
            finish_deferred_once: false,
            assistant_content: Some(
                "I would start by reviewing the code and then optimize the hot path.",
            ),
        },
    );

    match decision {
        FinishDecision::Defer(hint) => {
            assert!(hint.contains("Finish Check"));
            assert!(hint.contains("execution-oriented request"));
        }
        _ => panic!("expected finish deferral for action-oriented plan-only reply"),
    }
}

#[test]
fn finish_gate_allows_finish_after_single_deferral() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 0,
            total_tool_calls: 0,
            has_observation_history: false,
            latest_user_query: Some("optimize the repo performance and apply the changes"),
            consecutive_errors: 0,
            finish_deferred_once: true,
            assistant_content: Some("Here is the best answer I can provide."),
        },
    );

    assert!(matches!(
        decision,
        FinishDecision::Finish(FinishReason::Complete)
    ));
}

#[test]
fn finish_gate_does_not_defer_for_informational_file_question() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 0,
            total_tool_calls: 0,
            has_observation_history: false,
            latest_user_query: Some("what does main.rs do?"),
            consecutive_errors: 0,
            finish_deferred_once: false,
            assistant_content: Some("main.rs wires together the CLI entrypoints and runtime."),
        },
    );

    assert!(matches!(
        decision,
        FinishDecision::Finish(FinishReason::Complete)
    ));
}

#[test]
fn finish_gate_allows_brief_answer_for_simple_query_after_observation() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 1,
            total_tool_calls: 1,
            has_observation_history: true,
            latest_user_query: Some("which file sets the timeout?"),
            consecutive_errors: 0,
            finish_deferred_once: false,
            assistant_content: Some("src/config.rs"),
        },
    );

    assert!(matches!(
        decision,
        FinishDecision::Finish(FinishReason::Complete)
    ));
}

#[test]
fn finish_gate_still_defers_brief_answer_for_complex_query_after_observation() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 1,
            total_tool_calls: 2,
            has_observation_history: true,
            latest_user_query: Some("optimize the repo performance and apply the changes"),
            consecutive_errors: 0,
            finish_deferred_once: false,
            assistant_content: Some("Done."),
        },
    );

    match decision {
        FinishDecision::Defer(hint) => {
            assert!(hint.contains("Use it to produce a fuller result"));
        }
        _ => panic!("expected finish deferral for brief post-tool answer on complex query"),
    }
}

#[test]
fn finish_gate_does_not_treat_chinese_recommendation_as_uncertain() {
    let decision = evaluate_finish_with_gate(
        true,
        false,
        &FinishGateContext {
            cycles: 1,
            total_tool_calls: 0,
            has_observation_history: false,
            latest_user_query: Some("接下来怎么处理？"),
            consecutive_errors: 2,
            finish_deferred_once: false,
            assistant_content: Some("建议先清理缓存，再重新运行命令并检查第一条报错。"),
        },
    );

    assert!(matches!(
        decision,
        FinishDecision::Finish(FinishReason::Complete)
    ));
}

#[test]
fn auto_think_level_adapts_by_cycle() {
    // First round: medium (short message, no errors)
    assert_eq!(auto_think_level(0, false, 100, 0), "medium");
    assert_eq!(auto_think_level(0, true, 100, 0), "medium");
    // First round with complex message (>200 chars): high
    assert_eq!(auto_think_level(0, false, 250, 0), "high");
    // Very large first-round request: xhigh
    assert_eq!(auto_think_level(0, false, 650, 0), "xhigh");
    // Mid rounds with observation: high
    assert_eq!(auto_think_level(1, true, 100, 0), "high");
    assert_eq!(auto_think_level(5, true, 100, 0), "high");
    // Mid rounds without observation: medium
    assert_eq!(auto_think_level(1, false, 100, 0), "medium");
    assert_eq!(auto_think_level(5, false, 100, 0), "medium");
    // Late rounds: low regardless
    assert_eq!(auto_think_level(6, false, 100, 0), "low");
    assert_eq!(auto_think_level(6, true, 100, 0), "low");
    assert_eq!(auto_think_level(100, true, 100, 0), "low");
    // Exactly at boundary: still medium
    assert_eq!(auto_think_level(0, false, 200, 0), "medium");
}

#[test]
fn auto_think_level_escalates_on_errors() {
    // Consecutive errors bump to high regardless of cycle
    assert_eq!(auto_think_level(3, false, 100, 2), "high");
    assert_eq!(auto_think_level(10, false, 100, 3), "high");
    // Severe repeated failures get the deepest budget.
    assert_eq!(auto_think_level(2, false, 100, 4), "xhigh");
    // Single error doesn't escalate
    assert_eq!(auto_think_level(6, false, 100, 1), "low");
}

#[test]
fn summarize_observations_empty_when_short() {
    let results = vec![ToolResultEntry {
        id: "c1".into(),
        name: "exec".into(),
        result: "short output".into(),
        duration_ms: 0,
        is_error: false,
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
        },
        ToolResultEntry {
            id: "c2".into(),
            name: "exec".into(),
            result: "ok".into(),
            duration_ms: 0,
            is_error: false,
        },
    ];
    let summaries = summarize_observations(&results);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_name, "read_file");
    assert_eq!(summaries[0].byte_size, big.len());
    assert!(summaries[0].hint.contains("3000 lines"));
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
        hint: "read_file returned 100 lines / 5000 bytes — focus on key findings".into(),
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
