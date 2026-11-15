// SPDX-License-Identifier: CC0-1.0

//! Cover for the whole `StopPipeline` / `StartPipeline` round trip a client drives by
//! polling: each leg of the cycle (`ACTIVE` -> `STOPPING` -> `STOPPED` -> `STARTING` ->
//! `ACTIVE`) settles by the NEXT read, so the cycle completes for a caller that only keeps
//! reading `GetPipeline`; and the members the service minted when the pipeline was created —
//! its ARN, its ingestion endpoint, its VPC endpoints, its creation instant — are properties
//! of the pipeline rather than of its run state, so they are retained across however many
//! cycles the client drives and are reported in the transient statuses as well as the
//! settled ones.
//!
//! Nothing here waits. No case sleeps, measures elapsed time or bounds anything by a clock:
//! the polling helper counts READS, because the property under test is that one read is
//! enough per leg. A leg that settled only after a wall-clock span would cost a polling
//! client a whole poll interval, however short that span is.
//!
//! The rows of the table differ along what the rule generalises over: how many stop/start
//! cycles the client drives, whether the pipeline is public or VPC-attached, its name shape
//! and length (both published `@length(3, 28)` bounds) and its unit counts.
//!
//! The harness — the router, the request walk and the lifecycle calls — is the sibling
//! suite's, reached rather than copied.

use serde_json::{json, Value};

use crate::pipeline_status_settling::{
    create_body, create_pipeline, expect_ok, new_router, poll_for_report, start_pipeline,
    stop_pipeline, Answer,
};

/// The members the service MINTS for a pipeline, which belong to the pipeline rather than to
/// its run state, so a stop/start cycle retains them (smithy_query: shape Pipeline).
const MINTED_MEMBERS: &[&str] = &["PipelineArn", "IngestEndpointUrls", "VpcEndpoints", "CreatedAt"];

/// The members a create SUPPLIES, which a stop/start cycle does not change either.
const SUPPLIED_MEMBERS: &[&str] = &[
    "PipelineName",
    "MinUnits",
    "MaxUnits",
    "PipelineConfigurationBody",
];

/// One row of the table: a client driving `cycles` stop/start cycles over one pipeline.
#[derive(Debug, Clone, Copy)]
struct CycleCase {
    label: &'static str,
    pipeline: &'static str,
    min_units: i64,
    max_units: i64,
    /// Whether the create asks for VPC access, which is what mints a `VpcEndpoint`.
    vpc: bool,
    cycles: usize,
}

/// The rows differ in how many cycles the client drives, in public vs VPC-attached, in name
/// length (both published `@length(3, 28)` bounds) and in unit counts.
const CYCLE_CASES: &[CycleCase] = &[
    CycleCase {
        label: "one cycle, shortest legal name",
        pipeline: "cyc",
        min_units: 1,
        max_units: 1,
        vpc: false,
        cycles: 1,
    },
    CycleCase {
        label: "two cycles, hyphenated name",
        pipeline: "stop-start-cycle-2",
        min_units: 2,
        max_units: 6,
        vpc: false,
        cycles: 2,
    },
    CycleCase {
        label: "three cycles, longest legal name",
        pipeline: "a-stop-start-cycle-case-28",
        min_units: 3,
        max_units: 3,
        vpc: false,
        cycles: 3,
    },
    CycleCase {
        label: "one cycle, vpc-attached pipeline",
        pipeline: "cycle-in-a-vpc",
        min_units: 4,
        max_units: 12,
        vpc: true,
        cycles: 1,
    },
];

/// The create request a row drives: the shared body, plus the `VpcOptions` the VPC-attached
/// row asks for.
fn cycle_create_body(case: &CycleCase) -> Value {
    let mut body = create_body(case.pipeline, case.min_units, case.max_units);
    if case.vpc {
        body["VpcOptions"] = json!({
            "SubnetIds": ["subnet-0123456789abcdef0"],
            "SecurityGroupIds": ["sg-0123456789abcdef0"],
        });
    }
    body
}

/// The single ingestion endpoint a report answered, asserted to be populated.
fn endpoint_of(case: &str, step: &str, answer: &Answer) -> String {
    let urls = answer
        .member("IngestEndpointUrls")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        urls.len(),
        1,
        "case {case:?}: {step} must report exactly one ingestion endpoint, got {urls:?}"
    );
    let url = urls[0].as_str().unwrap_or_default().to_string();
    assert!(
        !url.is_empty(),
        "case {case:?}: {step} must report a populated ingestion endpoint, got {:?}",
        answer.member("IngestEndpointUrls")
    );
    url
}

/// A client that stops a running pipeline and starts it again completes every leg on its
/// next read, and the pipeline it gets back is the SAME pipeline: the ingestion endpoint and
/// everything else the service minted at create time survive the cycle, and are reported in
/// the transient statuses as well as the settled ones.
#[tokio::test]
async fn a_stop_start_cycle_completes_and_retains_everything_the_create_minted() {
    for case in CYCLE_CASES {
        let label = case.label;
        let router = new_router();
        let name = case.pipeline;
        let created = expect_ok(
            label,
            "CreatePipeline",
            create_pipeline(&router, cycle_create_body(case)).await,
        );
        assert_eq!(
            created.status_value(),
            "CREATING",
            "case {label:?}: CreatePipeline must answer with CREATING, got {:?}",
            created.body
        );

        // Leg one: the create settles by the first read, as a polling client needs.
        let (running, reads) =
            poll_for_report(&router, label, name, &["ACTIVE", "CREATE_FAILED"]).await;
        assert_eq!(
            reads, 1,
            "case {label:?}: the create must have settled by the first read, got {:?} after \
             {reads} reads",
            running.status_value()
        );
        assert_eq!(
            running.status_value(),
            "ACTIVE",
            "case {label:?}: the created pipeline must come up ACTIVE, got {:?}",
            running.body
        );

        // Every member the cycle must retain is populated BEFORE it runs, so a comparison
        // after it cannot pass by both sides being absent.
        for member in MINTED_MEMBERS.iter().chain(SUPPLIED_MEMBERS) {
            assert!(
                !running.member(member).is_null(),
                "case {label:?}: the running pipeline must report {member}, got {:?}",
                running.body
            );
        }
        let vpc_endpoints = running
            .member("VpcEndpoints")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            !vpc_endpoints.is_empty(),
            case.vpc,
            "case {label:?}: a VpcEndpoint exists exactly when the create asked for VPC \
             access, got {vpc_endpoints:?}"
        );
        let endpoint_before = endpoint_of(label, "the report before the cycle", &running);

        for cycle in 1..=case.cycles {
            // The stop reports the transient status it is modelled to report, and keeps
            // reporting the endpoint while the pipeline is on its way down.
            let stopping = expect_ok(label, "StopPipeline", stop_pipeline(&router, name).await);
            assert_eq!(
                stopping.status_value(),
                "STOPPING",
                "case {label:?}: cycle {cycle}: StopPipeline must answer with STOPPING, got {:?}",
                stopping.body
            );
            assert_eq!(
                endpoint_of(label, "StopPipeline", &stopping),
                endpoint_before,
                "case {label:?}: cycle {cycle}: a stop must not change the ingestion endpoint, \
                 got {:?}",
                stopping.member("IngestEndpointUrls")
            );

            let (halted, reads) =
                poll_for_report(&router, label, name, &["STOPPED", "START_FAILED"]).await;
            assert_eq!(
                reads, 1,
                "case {label:?}: cycle {cycle}: the stop must have settled by the first read, \
                 got {:?} after {reads} reads",
                halted.status_value()
            );
            assert_eq!(
                halted.status_value(),
                "STOPPED",
                "case {label:?}: cycle {cycle}: the stopped pipeline must settle STOPPED, got \
                 {:?}",
                halted.body
            );
            // A stopped pipeline still publishes its endpoint: the endpoint belongs to the
            // pipeline, not to its running state.
            assert_eq!(
                endpoint_of(label, "the report of the stopped pipeline", &halted),
                endpoint_before,
                "case {label:?}: cycle {cycle}: a stopped pipeline must keep its endpoint, \
                 got {:?}",
                halted.member("IngestEndpointUrls")
            );

            let starting = expect_ok(label, "StartPipeline", start_pipeline(&router, name).await);
            assert_eq!(
                starting.status_value(),
                "STARTING",
                "case {label:?}: cycle {cycle}: StartPipeline must answer with STARTING, got {:?}",
                starting.body
            );
            assert_eq!(
                endpoint_of(label, "StartPipeline", &starting),
                endpoint_before,
                "case {label:?}: cycle {cycle}: a start must not change the ingestion endpoint, \
                 got {:?}",
                starting.member("IngestEndpointUrls")
            );

            let (restarted, reads) =
                poll_for_report(&router, label, name, &["ACTIVE", "START_FAILED"]).await;
            assert_eq!(
                reads, 1,
                "case {label:?}: cycle {cycle}: the start must have settled by the first read, \
                 got {:?} after {reads} reads",
                restarted.status_value()
            );
            assert_eq!(
                restarted.status_value(),
                "ACTIVE",
                "case {label:?}: cycle {cycle}: the started pipeline must settle ACTIVE, got {:?}",
                restarted.body
            );

            // The pipeline the cycle gives back is the same pipeline: nothing the service
            // minted, and nothing the create supplied, moved.
            for member in MINTED_MEMBERS.iter().chain(SUPPLIED_MEMBERS) {
                assert_eq!(
                    restarted.member(member),
                    running.member(member),
                    "case {label:?}: cycle {cycle}: {member} changed across the stop/start \
                     cycle, got {:?} against {:?}",
                    restarted.member(member),
                    running.member(member)
                );
            }
            assert_eq!(
                restarted.member("StatusReason"),
                &json!(null),
                "case {label:?}: cycle {cycle}: a settled pipeline carries no StatusReason, \
                 got {:?}",
                restarted.member("StatusReason")
            );
            // The cycle did move the pipeline, so its update stamp is not before its
            // creation stamp.
            assert!(
                restarted.member("LastUpdatedAt").as_f64() >= running.member("CreatedAt").as_f64(),
                "case {label:?}: cycle {cycle}: LastUpdatedAt must not precede CreatedAt, got \
                 {:?} / {:?}",
                restarted.member("LastUpdatedAt"),
                running.member("CreatedAt")
            );
        }
    }
}

/// The statuses a polling client OBSERVES over a cycle, in order: the transient one from the
/// call that armed each leg, then the settled one from the read that follows it. The
/// sequence is the same whichever pipeline shape the cycle runs on.
#[tokio::test]
async fn a_polling_client_observes_the_whole_transition_sequence_in_order() {
    for case in CYCLE_CASES {
        let label = case.label;
        let router = new_router();
        let name = case.pipeline;
        let mut observed = Vec::new();

        let created = expect_ok(
            label,
            "CreatePipeline",
            create_pipeline(&router, cycle_create_body(case)).await,
        );
        observed.push(created.status_value());
        let (running, _) = poll_for_report(&router, label, name, &["ACTIVE", "CREATE_FAILED"]).await;
        observed.push(running.status_value());

        let mut expected = vec!["CREATING".to_string(), "ACTIVE".to_string()];
        for _ in 1..=case.cycles {
            let stopping = expect_ok(label, "StopPipeline", stop_pipeline(&router, name).await);
            observed.push(stopping.status_value());
            let (halted, _) =
                poll_for_report(&router, label, name, &["STOPPED", "START_FAILED"]).await;
            observed.push(halted.status_value());
            let starting = expect_ok(label, "StartPipeline", start_pipeline(&router, name).await);
            observed.push(starting.status_value());
            let (restarted, _) =
                poll_for_report(&router, label, name, &["ACTIVE", "START_FAILED"]).await;
            observed.push(restarted.status_value());
            expected.extend(
                ["STOPPING", "STOPPED", "STARTING", "ACTIVE"]
                    .iter()
                    .map(|status| status.to_string()),
            );
        }

        assert_eq!(
            observed, expected,
            "case {label:?}: expected the observed status sequence {expected:?}, got {observed:?}"
        );
    }
}
