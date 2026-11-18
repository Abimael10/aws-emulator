// SPDX-License-Identifier: CC0-1.0

//! `CreateChangeSet` then `ExecuteChangeSet` over a template whose resources are AWS Systems
//! Manager parameters fed by a template parameter — the deploy path every change-set client
//! takes — and what each deploy leaves behind in both services.
//!
//! RULE: executing a change set applies the plan it carries to the stack it belongs to — the
//! change set's template becomes the stack's template and the parameter values resolved for
//! that plan become the stack's parameters — and a `CREATE` change set takes the stack it left
//! in `REVIEW_IN_PROGRESS` out of review, through `CREATE_IN_PROGRESS` to a terminal create
//! status, so the stack then serves its template, reports those parameters, and holds the
//! resources the template declares in the services that own them. Every later deploy resolves
//! the request's parameter set the same way: an entry that restates a key with
//! `UsePreviousValue` takes the value the stack is already using (`Parameter$UsePreviousValue`:
//! "During a stack update, use the existing parameter value that the stack is using for a given
//! parameter key. If you specify true, do not specify a parameter value"), and a key the
//! request's parameter set does not state at all takes the `Default` the template declares for
//! it.
//!
//! Two crates have to agree for that to hold — CloudFormation plans, executes and reports the
//! stack, and Systems Manager is the only side that can say what the deployed resource really
//! holds — so the suite lives here, over the assembled emulator, rather than in either of them.
//!
//! The rows differ along what the rule generalises over: the value the first deploy states (a
//! word, one carrying spaces and punctuation, one that looks numeric), how the third deploy
//! spells "state no value for this key" (the empty parameter set, and a set that states another
//! declared parameter instead), and how `ExecuteChangeSet` addresses the change set (by name
//! beside its stack, and by the id the creation answered, alone — `ExecuteChangeSetInput`
//! leaves `StackName` optional, and only an ARN names a change set on its own). Every row runs
//! the three deploys of the contract the rule covers: a value given, that value reused, and the
//! value dropped.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{element, elements, form_encode, opt_element, pairs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region every stack and every
/// parameter lives in.
const REGION: &str = "us-east-1";

/// How long a wait for a clock-owed transition may take before the test fails rather than
/// hangs: far above the span itself, so neither a loaded host nor a busy test run can decide
/// the outcome. Nothing here asserts how long anything took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait sleeps between two looks at the resource it is waiting on.
const SETTLE_INTERVAL: Duration = Duration::from_millis(20);

/// The template parameter the declared resources read their value from.
const GREETING: &str = "Greeting";

/// A second declared parameter no resource reads, so a request can state a parameter set that
/// is NOT empty and still states nothing for [`GREETING`].
const TIER: &str = "Tier";

/// The `Default` the template declares for [`GREETING`] — what a deploy that states no value
/// for it falls back to.
const DEFAULT_GREETING: &str = "default-value";

/// The `Default` the template declares for [`TIER`].
const DEFAULT_TIER: &str = "standard";

/// The change-set name each of the three deploys creates its plan under. A name is per deploy:
/// an executed change set is still one of the stack's, so the next plan is a new name, which is
/// what a client deploying repeatedly sends.
const CHANGE_SETS: [&str; 3] = ["deploy-one", "deploy-two", "deploy-three"];

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from. The signature is never checked:
/// this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be accepted.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// Call one AWS Systems Manager operation (`sigv4Name: ssm`, service shape `AmazonSSM`) over
/// its awsJson1_1 wire.
async fn ssm(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .header("authorization", authorization("ssm"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The value Systems Manager holds under `name`, or `None` where it holds none.
async fn parameter_value(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let (status, body) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    if status != 200 {
        return None;
    }
    body.pointer("/Parameter/Value")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// The `(Status, ExecutionStatus)` a change set rests in once it has left its in-flight
/// statuses, by LOOKING again until that holds or [`SETTLE_DEADLINE`] passes.
///
/// The wait is for the CONDITION rather than for a span, so a slower host only takes more
/// looks; the deadline is a safety net so a wait nothing will satisfy ends in a failure rather
/// than a hang.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    change_set: &str,
) -> (String, String) {
    let members = vec![("ChangeSetName".to_string(), change_set.to_string())];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeChangeSet", &members).await;
        let status = opt_element(&body, "Status").unwrap_or_default();
        let execution = opt_element(&body, "ExecutionStatus").unwrap_or_default();
        let in_flight = |seen: &str| {
            seen.is_empty() || seen.ends_with("_PENDING") || seen.ends_with("_IN_PROGRESS")
        };
        if !in_flight(&status) && !in_flight(&execution) {
            return (status, execution);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the change set was still ({status:?}, {execution:?}) after \
             {SETTLE_DEADLINE:?}, so nothing settled it: {body}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The status `stack` rests in once CloudFormation has left every `*_IN_PROGRESS` status,
/// waited for the way [`settled_change_set`] waits.
async fn settled_stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let members = vec![("StackName".to_string(), stack.to_string())];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeStacks", &members).await;
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.is_empty() && !status.ends_with("_IN_PROGRESS") {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was still {status:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The status a stack reports right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStacks",
        &[("StackName".to_string(), stack.to_string())],
    )
    .await;
    opt_element(&body, "StackStatus").unwrap_or_default()
}

/// The parameters a stack reports, as the key/value map a client reads them as.
async fn stack_parameters(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStacks",
        &[("StackName".to_string(), stack.to_string())],
    )
    .await;
    pairs_of(&body, "ParameterKey", "ParameterValue")
}

/// The resources a stack manages, as the logical-id -> physical-id map a client reads them as.
async fn managed_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackResources",
        &[("StackName".to_string(), stack.to_string())],
    )
    .await;
    physical_ids_of(&body)
}

/// The template a stack serves, or `None` where the read is refused — `GetTemplate` models no
/// error for a stack whose template it has, so a refusal here is the symptom, not an outcome.
async fn stack_template(router: &MultiServiceRouter, stack: &str) -> Option<String> {
    let (status, body) = cloudformation(
        router,
        "GetTemplate",
        &[("StackName".to_string(), stack.to_string())],
    )
    .await;
    if status != 200 {
        return None;
    }
    elements(&body, "TemplateBody").into_iter().next()
}

/// `Parameters.member.<n>` as an SDK spells a parameter set on the wire: a key beside the value
/// it states, a key beside `UsePreviousValue=true` for an entry that asks for the stack's own
/// value, and the bare member name for the EMPTY set — the query protocol serializes an empty
/// list as the member name with no value, which is how a client that reuses nothing spells it.
fn parameter_members(parameters: &[(&str, Option<&str>)]) -> Vec<(String, String)> {
    if parameters.is_empty() {
        return vec![("Parameters".to_string(), String::new())];
    }
    parameters
        .iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let at = format!("Parameters.member.{}", index + 1);
            match value {
                Some(value) => vec![
                    (format!("{at}.ParameterKey"), (*key).to_string()),
                    (format!("{at}.ParameterValue"), (*value).to_string()),
                ],
                None => vec![
                    (format!("{at}.ParameterKey"), (*key).to_string()),
                    (format!("{at}.UsePreviousValue"), "true".to_string()),
                ],
            }
        })
        .collect()
}

/// The Systems Manager name the template's `Param<n>` resource is declared under.
fn parameter_name(prefix: &str, index: usize) -> String {
    format!("{prefix}/{index}")
}

/// A template declaring two parameters and `resources` Systems Manager parameters whose value
/// is `{"Ref": "Greeting"}`.
///
/// `Name`, `Type` and `Value` are the properties `AWS::SSM::Parameter` carries (source: the
/// CloudFormation resource reference for that type, the same three properties
/// `cloudformation_auto_imports_existing_ssm_parameters.rs` declares), and `Default` is what a
/// template parameter falls back to when a stack operation states no value for it.
fn template(prefix: &str, resources: usize) -> String {
    let declared = (1..=resources)
        .map(|index| {
            let name = parameter_name(prefix, index);
            format!(
                r#""Param{index}":{{"Type":"AWS::SSM::Parameter","Properties":{{"Name":"{name}","Type":"String","Value":{{"Ref":"{GREETING}"}}}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"AWSTemplateFormatVersion":"2010-09-09","Parameters":{{"{GREETING}":{{"Type":"String","Default":"{DEFAULT_GREETING}"}},"{TIER}":{{"Type":"String","Default":"{DEFAULT_TIER}"}}}},"Resources":{{{declared}}}}}"#
    )
}

/// How a deploy addresses the change set it executes.
#[derive(Clone, Copy, Debug)]
enum Address {
    /// `ChangeSetName` as the creation named it, beside the `StackName` it belongs to.
    ByNameBesideItsStack,
    /// The `Id` the creation answered, alone — an ARN names a change set on its own.
    ByIdAlone,
}

/// How the third deploy states "no value for [`GREETING`]".
#[derive(Clone, Copy, Debug)]
enum Dropping {
    /// The empty parameter set: every declared parameter falls back to its `Default`.
    EmptySet,
    /// A set that states the OTHER declared parameter with this value and says nothing about
    /// [`GREETING`], which falls back to its `Default` just the same.
    OtherKeyOnly(&'static str),
}

/// One deploy: create a change set, wait for its creation, execute it, and wait for the stack's
/// terminal status — answering what the stack rests in.
///
/// The statuses the opened operation may be seen in are passed in rather than read off the
/// answer: what this asserts between the execution and the settling is that the stack LEFT the
/// status the change set had parked it in, whether or not the clock has already made the
/// terminal status due.
#[allow(clippy::too_many_arguments)]
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
    change_set_type: &str,
    template_body: &str,
    parameters: &[(&str, Option<&str>)],
    address: Address,
    opening: &str,
) -> String {
    let mut members = vec![
        ("StackName".to_string(), stack.to_string()),
        ("ChangeSetName".to_string(), change_set.to_string()),
        ("ChangeSetType".to_string(), change_set_type.to_string()),
        ("TemplateBody".to_string(), template_body.to_string()),
    ];
    members.extend(parameter_members(parameters));
    let created = cloudformation_ok(router, case, "CreateChangeSet", &members).await;
    let id = element(&created, "Id");
    assert!(
        id.starts_with("arn:"),
        "case {case:?}: CreateChangeSet must answer the change set's ARN, got {created}"
    );

    let settled = settled_change_set(router, case, &id).await;
    assert_eq!(
        (settled.0.as_str(), settled.1.as_str()),
        ("CREATE_COMPLETE", "AVAILABLE"),
        "case {case:?}: a plan with changes must be executable, got {settled:?}"
    );

    let execution = match address {
        Address::ByNameBesideItsStack => vec![
            ("StackName".to_string(), stack.to_string()),
            ("ChangeSetName".to_string(), change_set.to_string()),
        ],
        Address::ByIdAlone => vec![("ChangeSetName".to_string(), id.clone())],
    };
    cloudformation_ok(router, case, "ExecuteChangeSet", &execution).await;

    // The execution opens the stack's operation at once: whatever the clock has made due by
    // now, the stack is no longer parked where it was before the call.
    let opened = stack_status(router, case, stack).await;
    let terminal = opening.replace("_IN_PROGRESS", "_COMPLETE");
    assert!(
        opened == opening || opened == terminal,
        "case {case:?}: the execution must open the stack's {opening} operation, got {opened:?}"
    );

    let reached = settled_stack_status(router, case, stack).await;
    assert_eq!(
        reached, terminal,
        "case {case:?}: expected the execution to reach {terminal}, got {reached:?}"
    );
    let executed = settled_change_set(router, case, &id).await;
    assert_eq!(
        (executed.0.as_str(), executed.1.as_str()),
        ("CREATE_COMPLETE", "EXECUTE_COMPLETE"),
        "case {case:?}: the executed change set must report its execution, got {executed:?}"
    );
    reached
}

/// One row: a stack deployed three times through the change-set path.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The prefix this row's Systems Manager names are built from, unique across the suite.
    prefix: &'static str,
    /// The value the FIRST deploy states for [`GREETING`].
    given: &'static str,
    /// How the THIRD deploy states no value for it.
    dropping: Dropping,
    address: Address,
}

/// RULE: executing a change set makes its template and its resolved parameters the stack's own
/// — a `CREATE` change set taking the stack out of `REVIEW_IN_PROGRESS` through
/// `CREATE_IN_PROGRESS` to a terminal create status, with the declared resources provisioned in
/// the service that owns them — and every later deploy reuses the stack's value for a key
/// restated with `UsePreviousValue` and falls back to the template's `Default` for a key its
/// parameter set does not state.
#[tokio::test]
async fn an_executed_change_set_deploys_its_template_and_resolves_its_parameters() {
    let router = build_router();

    let rows: Vec<Row> = vec![
        Row {
            case: "a word value, the change set named beside its stack, dropped by the empty set",
            stack: "cspv-word",
            prefix: "/cspv/word",
            given: "first",
            dropping: Dropping::EmptySet,
            address: Address::ByNameBesideItsStack,
        },
        Row {
            case: "a value carrying spaces and punctuation, the change set addressed by its id \
                   alone, dropped by a set that states the other key",
            stack: "cspv-sentence",
            prefix: "/cspv/sentence",
            given: "hello, world!",
            dropping: Dropping::OtherKeyOnly("premium"),
            address: Address::ByIdAlone,
        },
        Row {
            case: "a value that looks numeric, the change set addressed by its id alone, dropped \
                   by the empty set",
            stack: "cspv-numeric",
            prefix: "/cspv/numeric",
            given: "0042",
            dropping: Dropping::EmptySet,
            address: Address::ByIdAlone,
        },
    ];

    for row in &rows {
        let case = row.case;
        let first = template(row.prefix, 1);
        let second = template(row.prefix, 2);
        let one = parameter_name(row.prefix, 1);
        let two = parameter_name(row.prefix, 2);

        // ---- Deploy 1: the stack is created by the change set, with a value given. ----
        let creating = cloudformation_ok(
            &router,
            case,
            "CreateChangeSet",
            &[
                ("StackName".to_string(), row.stack.to_string()),
                ("ChangeSetName".to_string(), "review-plan".to_string()),
                ("ChangeSetType".to_string(), "CREATE".to_string()),
                ("TemplateBody".to_string(), first.clone()),
            ],
        )
        .await;
        let review_plan = element(&creating, "Id");
        let created = settled_change_set(&router, case, &review_plan).await;
        assert_eq!(
            (created.0.as_str(), created.1.as_str()),
            ("CREATE_COMPLETE", "AVAILABLE"),
            "case {case:?}: the first plan must be executable, got {created:?}"
        );
        // The stack the plan registered exists, in review and with nothing provisioned: the
        // state the execution below has to take it out of.
        let parked = stack_status(&router, case, row.stack).await;
        assert_eq!(
            parked, "REVIEW_IN_PROGRESS",
            "case {case:?}: a stack a CREATE change set registered waits in review, got \
             {parked:?}"
        );
        let unprovisioned = parameter_value(&router, &one).await;
        assert_eq!(
            unprovisioned, None,
            "case {case:?}: a plan that has not been executed provisions nothing, yet {one:?} is \
             held as {unprovisioned:?}"
        );

        let reached = deploy(
            &router,
            case,
            row.stack,
            CHANGE_SETS[0],
            "CREATE",
            &first,
            &[(GREETING, Some(row.given))],
            row.address,
            "CREATE_IN_PROGRESS",
        )
        .await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: executing the CREATE plan must create the stack, got {reached:?}"
        );

        let served = stack_template(&router, row.stack).await;
        assert_eq!(
            served.as_deref(),
            Some(first.as_str()),
            "case {case:?}: the executed template must be the one the stack serves, got {served:?}"
        );
        let reported = stack_parameters(&router, case, row.stack).await;
        let expected: BTreeMap<String, String> = [(GREETING, row.given), (TIER, DEFAULT_TIER)]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: the created stack must report the plan's parameters, expected \
             {expected:?}, got {reported:?}"
        );
        let ids = managed_resources(&router, case, row.stack).await;
        let expected_ids: BTreeMap<String, String> =
            [("Param1".to_string(), one.clone())].into_iter().collect();
        assert_eq!(
            ids, expected_ids,
            "case {case:?}: the created stack must manage the declared parameter, expected \
             {expected_ids:?}, got {ids:?}"
        );
        let held = parameter_value(&router, &one).await;
        assert_eq!(
            held.as_deref(),
            Some(row.given),
            "case {case:?}: the deployed resource must hold the value given, got {held:?}"
        );

        // ---- Deploy 2: the value is REUSED, and a second resource reads the reused value. ----
        let reached = deploy(
            &router,
            case,
            row.stack,
            CHANGE_SETS[1],
            "UPDATE",
            &second,
            &[(GREETING, None)],
            row.address,
            "UPDATE_IN_PROGRESS",
        )
        .await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: the reusing deploy must complete, got {reached:?}"
        );
        let reported = stack_parameters(&router, case, row.stack).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: UsePreviousValue must keep the stack's value, expected {expected:?}, \
             got {reported:?}"
        );
        // The resource the second template declares anew is one of the stack's, beside the one
        // the create deployed: the update reconciled the template rather than replacing it.
        let ids = managed_resources(&router, case, row.stack).await;
        let expected_ids: BTreeMap<String, String> = [
            ("Param1".to_string(), one.clone()),
            ("Param2".to_string(), two.clone()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            ids, expected_ids,
            "case {case:?}: the updated stack must manage both declared parameters, expected \
             {expected_ids:?}, got {ids:?}"
        );
        for name in [&one, &two] {
            let held = parameter_value(&router, name).await;
            assert_eq!(
                held.as_deref(),
                Some(row.given),
                "case {case:?}: {name:?} must be deployed from the reused value, got {held:?}"
            );
        }

        // ---- Deploy 3: the key is not stated at all, so the template's Default applies. ----
        let dropped: Vec<(&str, Option<&str>)> = match row.dropping {
            Dropping::EmptySet => Vec::new(),
            Dropping::OtherKeyOnly(tier) => vec![(TIER, Some(tier))],
        };
        let reached = deploy(
            &router,
            case,
            row.stack,
            CHANGE_SETS[2],
            "UPDATE",
            &second,
            &dropped,
            row.address,
            "UPDATE_IN_PROGRESS",
        )
        .await;
        assert_eq!(
            reached, "UPDATE_COMPLETE",
            "case {case:?}: the deploy that drops the value must complete, got {reached:?}"
        );
        let reported = stack_parameters(&router, case, row.stack).await;
        let tier = match row.dropping {
            Dropping::EmptySet => DEFAULT_TIER,
            Dropping::OtherKeyOnly(stated) => stated,
        };
        let expected: BTreeMap<String, String> =
            [(GREETING, DEFAULT_GREETING), (TIER, tier)]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: a key the parameter set does not state falls back to the template's \
             Default, expected {expected:?}, got {reported:?}"
        );
        for name in [&one, &two] {
            let held = parameter_value(&router, name).await;
            assert_eq!(
                held.as_deref(),
                Some(DEFAULT_GREETING),
                "case {case:?}: {name:?} must be redeployed from the declared default, got \
                 {held:?}"
            );
        }
        let served = stack_template(&router, row.stack).await;
        assert_eq!(
            served.as_deref(),
            Some(second.as_str()),
            "case {case:?}: the last executed template must be the one the stack serves, got \
             {served:?}"
        );
    }
}
