// SPDX-License-Identifier: CC0-1.0

//! How CodePipeline WORDS the refusals of the two calls that start a pipeline
//! execution, end to end over the wire the service speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! RULE: a modelled error CodePipeline raises from more than one operation carries the
//! status and the code of the model and the MESSAGE of the call that raised it, so two
//! calls that answer one condition answer it in two different sentences. This suite
//! pins that for the two calls that start an execution — `StartPipelineExecution` and
//! `PutActionRevision`, the operations
//! `smithy_query(query="operation", operation="StartPipelineExecution,PutActionRevision")`
//! reports `ConcurrentPipelineExecutionsLimitExceededException` (`errorKind=client`,
//! `httpError=400`) on:
//!
//! * the pipeline at its concurrent-execution ceiling refuses the next trigger with
//!   that error, whichever of the two calls sent it and whichever of them filled the
//!   slots, and the refusal records nothing;
//! * the two sentences share the condition they state and differ in the guidance they
//!   close with, so one shared string cannot serve both;
//! * the same per-call rule on `PipelineNotFoundException`: a deleted pipeline's name
//!   is refused by the declaration READ and by the execution START in their own
//!   sentences, over a set of names rather than one.
//!
//! The pipelines here hold their slots without the clock's help: their source action is
//! a `Custom` one, whose work is a job this account's own worker reports the outcome of,
//! so every admitted execution stays non-terminal until one does.

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE;

use crate::execution_engine::Emulator;
use crate::probe::{pipeline, put_revision, statuses, NON_TERMINAL};

/// The condition sentence both calls state, which the model documents as the error's own
/// description (`smithy_query(query="shape",
/// shape="ConcurrentPipelineExecutionsLimitExceededException")` reports "The pipeline has
/// reached the limit for concurrent pipeline executions.").
const THE_CONDITION: &str = "The limit for concurrent pipeline executions has been exceeded.";

/// Which call sent the trigger: the two operations that start an execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    /// `StartPipelineExecution`: the call that starts one DIRECTLY.
    Start,
    /// `PutActionRevision`: the call that starts one as the effect of recording a new
    /// source revision.
    Revision,
}

impl Call {
    /// The whole sentence this call's refusal carries: the shared condition, and the
    /// guidance this call closes with. Fixed text with no variable part.
    fn expected_message(self) -> String {
        let guidance = match self {
            Self::Start => "Please try again later.",
            Self::Revision => "Wait for concurrent executions to complete, and then try again.",
        };
        format!("{THE_CONDITION} {guidance}")
    }
}

/// Send one trigger of `call` at the pipeline. `nth` distinguishes the revisions a
/// revision driver reports, because a revision the action already carries starts nothing.
async fn trigger(emulator: &Emulator, call: Call, name: &str, nth: usize) -> (u16, Value) {
    match call {
        Call::Start => {
            emulator
                .cp("StartPipelineExecution", &json!({"name": name}))
                .await
        }
        Call::Revision => put_revision(emulator, name, &format!("rev-{nth}")).await,
    }
}

/// A `PARALLEL` pipeline whose every slot is taken, by triggers of `filler`.
async fn pipeline_at_its_ceiling(emulator: &Emulator, name: &str, filler: Call, case: &str) {
    emulator
        .ok(
            "CreatePipeline",
            &json!({"pipeline": pipeline(name, "Custom", "MySource")}),
        )
        .await;
    // One slot is already taken by the run the pipeline's creation started, so the
    // triggers that fit inside the bound are one fewer than the bound.
    for nth in 0..MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - 1 {
        let (status, answered) = trigger(emulator, filler, name, nth).await;
        assert_eq!(
            status, 200,
            "case {case:?}: trigger {nth} is inside the bound of \
             {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE}, got {status} with {answered}"
        );
    }
    let held = statuses(emulator, name).await;
    let in_flight = held
        .iter()
        .filter(|status| NON_TERMINAL.contains(&status.as_str()))
        .count();
    assert_eq!(
        in_flight, MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "case {case:?}: every execution waiting for its job worker holds a slot, got {held:?}"
    );
}

#[tokio::test]
async fn each_call_that_starts_an_execution_words_the_ceiling_its_own_way() {
    // The rule generalises over the CALL, so the table crosses the call that filled the
    // pipeline's slots with the call that then finds it full: the accounting is one and
    // the sentence belongs to whoever asked.
    for filler in [Call::Start, Call::Revision] {
        for refuser in [Call::Start, Call::Revision] {
            let case = format!("slots filled by {filler:?}, ceiling reached by {refuser:?}");
            let emulator = Emulator::new();
            let name = "ceiling";
            pipeline_at_its_ceiling(&emulator, name, filler, &case).await;

            let (status, refused) = trigger(
                &emulator,
                refuser,
                name,
                MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
            )
            .await;
            assert_eq!(
                (
                    status,
                    refused["__type"].as_str(),
                    refused["message"].as_str()
                ),
                (
                    400,
                    Some("ConcurrentPipelineExecutionsLimitExceededException"),
                    Some(refuser.expected_message().as_str())
                ),
                "case {case:?}: the trigger past the bound is refused in {refuser:?}'s own \
                 words, got {status} with {refused}"
            );

            // The refusal recorded nothing: the pipeline still holds exactly its bound.
            let retained = statuses(&emulator, name).await;
            assert_eq!(
                retained.len(),
                MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
                "case {case:?}: a refused trigger records no execution, got {retained:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_two_calls_share_the_condition_and_differ_in_the_guidance() {
    let case = "one pipeline at its ceiling, asked by both calls";
    let emulator = Emulator::new();
    let name = "both-calls";
    pipeline_at_its_ceiling(&emulator, name, Call::Start, case).await;

    let mut messages: Vec<(Call, String)> = Vec::new();
    for refuser in [Call::Start, Call::Revision] {
        let (status, refused) = trigger(
            &emulator,
            refuser,
            name,
            MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        )
        .await;
        let message = refused["message"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            (status, refused["__type"].as_str()),
            (
                400,
                Some("ConcurrentPipelineExecutionsLimitExceededException")
            ),
            "case {case:?}: {refuser:?} is refused the modelled error, got {status} with \
             {refused}"
        );
        assert!(
            message.starts_with(THE_CONDITION),
            "case {case:?}: {refuser:?} states the shared condition first, got {message:?}"
        );
        messages.push((refuser, message));
    }

    let (first_call, first) = &messages[0];
    let (second_call, second) = &messages[1];
    assert_ne!(
        first, second,
        "case {case:?}: {first_call:?} and {second_call:?} close with their own guidance, both \
         got {first:?}"
    );
}

#[tokio::test]
async fn a_deleted_pipelines_name_is_refused_per_call_on_a_read_and_on_a_start() {
    // The rule generalises over the NAME too, so the cases differ in alphabet and in
    // length: the punctuated name carries every character besides the alphanumerics that
    // `PipelineName`'s pattern allows, and the shortest and the longest are the bounds of
    // its `@length(1, 100)` (`smithy_query(query="shape", shape="PipelineName")`).
    let names: Vec<(&str, String)> = vec![
        ("a plain name", "deleted-pipe".to_string()),
        ("a punctuated name", "a.b@c-d_e".to_string()),
        ("the shortest name", "z".to_string()),
        ("the longest name", "q".repeat(100)),
    ];
    let account = engine::emulator_account_id();

    for (case, name) in names {
        let emulator = Emulator::new();
        emulator
            .ok(
                "CreatePipeline",
                &json!({"pipeline": pipeline(&name, "Custom", "MySource")}),
            )
            .await;
        emulator.ok("DeletePipeline", &json!({"name": name})).await;

        let (status, read) = emulator.cp("GetPipeline", &json!({"name": name})).await;
        assert_eq!(
            (status, read["__type"].as_str(), read["message"].as_str()),
            (
                400,
                Some("PipelineNotFoundException"),
                Some(
                    format!("Account '{account}' does not have a pipeline with name '{name}'")
                        .as_str()
                )
            ),
            "case {case:?}: the declaration read carries its own sentence, got {status} with \
             {read}"
        );

        let (status, started) = emulator
            .cp("StartPipelineExecution", &json!({"name": name}))
            .await;
        assert_eq!(
            (
                status,
                started["__type"].as_str(),
                started["message"].as_str()
            ),
            (
                400,
                Some("PipelineNotFoundException"),
                Some(
                    format!("Pipeline with name {name} does not exist in account {account}.")
                        .as_str()
                )
            ),
            "case {case:?}: the execution start carries its own sentence, got {status} with \
             {started}"
        );
        assert_ne!(
            read["message"].as_str(),
            started["message"].as_str(),
            "case {case:?}: the read and the start do not word the refusal the same way, both \
             got {:?}",
            read["message"].as_str()
        );
    }
}
