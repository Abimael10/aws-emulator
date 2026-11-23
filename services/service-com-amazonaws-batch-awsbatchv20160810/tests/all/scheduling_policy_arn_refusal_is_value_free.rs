// SPDX-License-Identifier: CC0-1.0

//! What AWS Batch's frontend SAYS when a request carries a value it checks against a
//! published rule, end to end over the wire.
//!
//! The rule under test: such a refusal states the RULE that was broken and carries NO value
//! of the request, inside this service's input-validation envelope
//! (`Error executing request, Exception : <detail>, RequestId: <this call's id>`) — so two
//! callers that broke one rule with different identifiers read ONE sentence and a client
//! classifies the whole class with one matcher. The scheduling-policy ARN pattern is the
//! member under test here; the sibling validators that state a range the same way (the
//! `arns` arity of `DescribeSchedulingPolicies`, a page size, a published member range) are
//! asserted by the suites beside this one.
//!
//! Asserted over the SET of identifiers the pattern rule governs — the rows differ along the
//! KIND of malformation and along which ARN field is wrong — and over EVERY operation that
//! takes a scheduling-policy identifier, so what is pinned is the identifier's rule and not
//! one handler's wording. A second test tells this class apart from the refusals the service
//! decides from STATE: an identifier of the RIGHT shape that names no stored policy is not
//! answered this sentence.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{arn, compute_environment, error_code, error_message, Batch, REGION};
use crate::refusal_contract::enveloped;

/// The header AWS reports a response's own request id in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The sentence this service words for an identifier outside the scheduling-policy ARN
/// pattern, spelled out here rather than read from the crate's constant, so a reworded
/// refusal is a failing test and not a silently updated expectation.
const PATTERN_RULE: &str = "Scheduling policy arn should match a valid Scheduling policy arn pattern";

/// One operation that takes a scheduling-policy identifier: how it is addressed, and the
/// request body that carries `identifier` to it.
struct Taker {
    label: &'static str,
    uri: &'static str,
    /// Built per case, because the writers that create a resource need a distinct name per
    /// call; `identifier` is the scheduling-policy identifier under test.
    body: fn(identifier: &str, case_index: usize, compute_environment: &str) -> Value,
}

/// Every operation that reads a scheduling-policy identifier out of its request: the reader
/// that takes a LIST of them, the two writers that take a single one, and the job-queue
/// create that binds one.
fn takers() -> Vec<Taker> {
    vec![
        Taker {
            label: "DescribeSchedulingPolicies",
            uri: "/v1/describeschedulingpolicies",
            body: |identifier, _index, _environment| json!({"arns": [identifier]}),
        },
        Taker {
            label: "DeleteSchedulingPolicy",
            uri: "/v1/deleteschedulingpolicy",
            body: |identifier, _index, _environment| json!({"arn": identifier}),
        },
        Taker {
            label: "UpdateSchedulingPolicy",
            uri: "/v1/updateschedulingpolicy",
            body: |identifier, _index, _environment| json!({"arn": identifier}),
        },
        Taker {
            label: "CreateJobQueue",
            uri: "/v1/createjobqueue",
            body: |identifier, index, environment| {
                json!({
                    "jobQueueName": format!("jq-sp-rule-{index}"),
                    "priority": 1,
                    "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
                    "schedulingPolicyArn": identifier,
                })
            },
        },
    ]
}

/// Identifiers no operation of this endpoint can read as a scheduling policy: neither a name
/// the writers resolve nor an ARN of this endpoint's own.
///
/// The rows differ along what the rule generalises over: the KIND of malformation (an ARN of
/// another Batch resource kind, an ARN with no resource at all, an ARN whose policy name is
/// empty or nested, a name carrying a character the name form excludes) and the ARN FIELD
/// that is wrong (partition, region, account) — plus a name one past the documented length.
fn malformed_identifiers() -> Vec<(String, String)> {
    let account = engine::EMULATOR_ACCOUNT_ID;
    vec![
        ("a name carrying a space".to_string(), "sp rule".to_string()),
        ("a name carrying a dot".to_string(), "sp.rule".to_string()),
        (
            "a name one past the documented length".to_string(),
            "r".repeat(state::MAX_SCHEDULING_POLICY_NAME_LEN + 1),
        ),
        (
            "an ARN with nothing after the account".to_string(),
            format!("arn:aws:batch:{REGION}:{account}"),
        ),
        ("a job-queue ARN".to_string(), arn("job-queue/jq-sp-rule")),
        (
            "a compute-environment ARN".to_string(),
            arn("compute-environment/ce-sp-rule"),
        ),
        (
            "an ARN with an empty policy name".to_string(),
            arn("scheduling-policy/"),
        ),
        (
            "an ARN carrying a nested path".to_string(),
            arn("scheduling-policy/sp-rule/extra"),
        ),
        (
            "an ARN of another partition".to_string(),
            format!("arn:aws-cn:batch:{REGION}:{account}:scheduling-policy/sp-rule"),
        ),
        (
            "an ARN of another region".to_string(),
            format!("arn:aws:batch:eu-west-3:{account}:scheduling-policy/sp-rule"),
        ),
        (
            "an ARN of another account".to_string(),
            format!("arn:aws:batch:{REGION}:111122223333:scheduling-policy/sp-rule"),
        ),
    ]
}

/// How many policies `DescribeSchedulingPolicies` reports for `policy_arn`.
async fn described(batch: &Batch, policy_arn: &str) -> usize {
    let body = batch
        .ok("/v1/describeschedulingpolicies", json!({"arns": [policy_arn]}))
        .await;
    body["schedulingPolicies"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// How many job queues `DescribeJobQueues` reports for `name`.
async fn queues_named(batch: &Batch, name: &str) -> usize {
    let body = batch
        .ok("/v1/describejobqueues", json!({"jobQueues": [name]}))
        .await;
    body["jobQueues"].as_array().map(Vec::len).unwrap_or_default()
}

/// RULE: every operation that reads a scheduling-policy identifier refuses one outside the
/// ARN pattern with the modelled `ClientException` (400) whose message is the envelope around
/// the PATTERN RULE and nothing else — the same sentence for every identifier, carrying none
/// of it — and leaves the store as it was.
#[tokio::test]
async fn one_sentence_states_the_rule_for_every_malformed_identifier() {
    let batch = Batch::new();

    let created = batch
        .ok("/v1/createschedulingpolicy", json!({"name": "sp-rule-stored"}))
        .await;
    let stored_arn = created["arn"]
        .as_str()
        .expect("the create answers the policy ARN")
        .to_string();
    let environment = compute_environment(&batch, "ce-sp-rule-host").await;

    let mut request_ids: Vec<String> = Vec::new();
    let mut case_index = 0usize;
    for (label, identifier) in malformed_identifiers() {
        for taker in takers() {
            case_index += 1;
            let case = format!("{label} on {}", taker.label);
            let body = (taker.body)(&identifier, case_index, &environment);
            let queue_name = body["jobQueueName"].as_str().unwrap_or_default().to_string();
            let (status, headers, refused) =
                batch.send_reported("POST", taker.uri, Some(body)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected the modelled ClientException 400, got {status} {refused}"
            );
            assert_eq!(
                error_code(&refused),
                "ClientException",
                "case {case:?}: expected the modelled ClientException, got {refused}"
            );
            let message = error_message(&refused);
            let (detail, request_id) = enveloped(&case, &message);
            assert_eq!(
                detail, PATTERN_RULE,
                "case {case:?}: expected the pattern rule alone inside the envelope, got \
                 {message:?}"
            );
            assert!(
                PATTERN_RULE.contains(identifier.as_str()) || !message.contains(identifier.as_str()),
                "case {case:?}: expected the refusal to carry no value of the request, got \
                 {message:?}"
            );
            // The id the envelope states is the id the response reports, so one call is
            // correlatable by one id.
            let reported = headers
                .get(REQUEST_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reported, request_id,
                "case {case:?}: expected {REQUEST_ID_HEADER} to report the id the message \
                 states ({request_id:?}), got {reported:?}"
            );
            request_ids.push(request_id);

            // The refusal is effect-free: the stored policy is still readable by its ARN,
            // and a refused create left no queue behind.
            assert_eq!(
                described(&batch, &stored_arn).await,
                1,
                "case {case:?}: a refused call must leave the stored policy describable"
            );
            if !queue_name.is_empty() {
                assert_eq!(
                    queues_named(&batch, &queue_name).await,
                    0,
                    "case {case:?}: a refused create must leave no job queue named \
                     {queue_name:?}"
                );
            }
        }
    }

    let distinct: std::collections::BTreeSet<&String> = request_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        request_ids.len(),
        "case every refusal of the scheduling-policy plane: expected one request id per call, \
         got {request_ids:?}"
    );
}

/// RULE: the pattern sentence is the SHAPE class only. An identifier of the right shape that
/// names no stored policy is a different answer on every operation that takes one — the
/// reader reports it absent, the delete is the same empty success, and the writers state the
/// service's own not-found — so the pattern rule is never a catch-all.
#[tokio::test]
async fn an_identifier_of_the_right_shape_is_not_answered_the_pattern_rule() {
    let batch = Batch::new();

    let environment = compute_environment(&batch, "ce-sp-rule-absent-host").await;
    // Well-formed scheduling-policy ARNs of this endpoint that name nothing stored: the rows
    // differ along the name inside the ARN, which is what the shape rule admits.
    let absent: Vec<String> = ["sp-rule-absent", "s", "SP_Rule_Absent-2"]
        .iter()
        .map(|name| arn(&format!("scheduling-policy/{name}")))
        .collect();

    for (index, reference) in absent.iter().enumerate() {
        // The reader reports an absent entry as absent, not as a refusal.
        let body = batch
            .ok(
                "/v1/describeschedulingpolicies",
                json!({"arns": [reference]}),
            )
            .await;
        assert_eq!(
            body["schedulingPolicies"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {reference:?} on DescribeSchedulingPolicies: expected no policy reported, \
             got {body}"
        );

        // The delete of a policy that is already gone is the same empty success.
        let (status, answered) = batch
            .call("/v1/deleteschedulingpolicy", json!({"arn": reference}))
            .await;
        assert_eq!(
            status, 200,
            "case {reference:?} on DeleteSchedulingPolicy: expected the empty success, got \
             {status} {answered}"
        );

        // The writers that must RESOLVE it state the service's own not-found, which is not
        // this class's sentence.
        for (label, uri, request) in [
            (
                "UpdateSchedulingPolicy",
                "/v1/updateschedulingpolicy",
                json!({"arn": reference}),
            ),
            (
                "CreateJobQueue",
                "/v1/createjobqueue",
                json!({
                    "jobQueueName": format!("jq-sp-rule-absent-{index}"),
                    "priority": 1,
                    "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
                    "schedulingPolicyArn": reference,
                }),
            ),
        ] {
            let (status, refused) = batch.call(uri, request).await;
            let case = format!("{reference:?} on {label}");
            assert_eq!(
                status, 400,
                "case {case}: expected the modelled ClientException 400, got {status} {refused}"
            );
            let message = error_message(&refused);
            assert!(
                !message.contains(PATTERN_RULE),
                "case {case}: an ARN of the right shape must not be answered the pattern rule, \
                 got {message:?}"
            );
            assert!(
                message.contains(reference.as_str()),
                "case {case}: the not-found class names the reference it could not resolve, \
                 got {message:?}"
            );
        }
    }
}
