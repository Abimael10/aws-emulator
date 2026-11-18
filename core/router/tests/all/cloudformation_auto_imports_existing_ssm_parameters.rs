// SPDX-License-Identifier: CC0-1.0

//! `CreateChangeSet(ImportExistingResources=true)` over AWS Systems Manager parameters that
//! already exist out of band — the wire form of `cdk deploy --import-existing-resources`.
//!
//! The rule under test: **a change set that states `ImportExistingResources` ADOPTS every
//! resource its plan would otherwise CREATE under a physical name the service that owns the
//! type already holds: the plan reports that resource with the `Import` action and the
//! identifier it already carries, so the resource-existence pre-deployment validation has no
//! addition left to fail and the change set is executable, and executing it records the
//! resource as one the stack manages WITHOUT modifying it. A plan that does not ask for
//! adoption is failed on that same name instead. Adoption is what the plan DOES rather than a
//! validation, so the member that turns the pre-deployment validations off does not turn it
//! off.**
//!
//! Two crates have to agree for that to hold — CloudFormation plans and executes the
//! adoption, and Systems Manager owns the parameter and is the only side that can say whether
//! one was created, adopted untouched, or left alone — so the suite lives here, over the
//! assembled emulator, rather than in either of them.
//!
//! The rows differ along what the rule generalises over: how many of the names the template
//! states are already held (neither, one of two, both), how the template SPELLS a name (as a
//! literal, and as an `Fn::Sub` over a template parameter the request supplies), whether
//! adoption is asked for at all (the member absent, `false`, `true`), and whether the request
//! also turns the pre-deployment validations off.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{blocks_of, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region every parameter lives in.
const REGION: &str = "us-east-1";

/// How long a wait for a clock-owed transition may take before the test fails rather than
/// hangs: far above the span itself (`STATUS_TRANSITION_DELAY`, 500 ms), so neither a loaded
/// host nor a busy test run can decide the outcome. Nothing here asserts how long anything
/// took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait sleeps between two looks at the resource it is waiting on.
const SETTLE_INTERVAL: Duration = Duration::from_millis(20);

/// The value the TEMPLATE declares for every parameter it names. Different from the value a
/// client wrote out of band, so an adoption that rewrote the resource would be visible.
const TEMPLATE_VALUE: &str = "from-the-template";

/// The value a client holds a name under before any stack is created.
const OUT_OF_BAND_VALUE: &str = "written-out-of-band";

/// The `StatusReason` a change set whose resource-existence validation failed carries — the
/// sentence CloudFormation answers for a failed early validation, naming the validations that
/// failed and sending the caller to `DescribeEvents` for the detail, as the recorded response
/// this suite's issue quotes words it.
const EXISTENCE_CHECK_FAILED: &str =
    "The following hook(s)/validation failed: \
     [AWS::EarlyValidation::ResourceExistenceCheck]. To troubleshoot Early Validation errors, \
     use the DescribeEvents API for detailed failure information.";

/// The logical ids every template here declares, in declaration order.
const LOGICAL_IDS: [&str; 2] = ["First", "Second"];

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

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
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

/// Write one parameter through Systems Manager's own wire, so the name is held by a CLIENT
/// and no stack manages it.
async fn put_parameter(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = ssm(
        router,
        "PutParameter",
        &json!({"Name": name, "Type": "String", "Value": OUT_OF_BAND_VALUE}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutParameter for {name:?}, got {status} with \
         {body}"
    );
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

/// A `DescribeChangeSet` answer read once the change set has left its creating statuses, by
/// LOOKING again until that holds or [`SETTLE_DEADLINE`] passes.
///
/// The wait is for the CONDITION rather than for a span, so a slower host only takes more
/// looks; the deadline is a safety net so a wait nothing will satisfy ends in a failure rather
/// than a hang.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let members = vec![
        ("StackName".to_string(), stack.to_string()),
        ("ChangeSetName".to_string(), change_set.to_string()),
    ];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeChangeSet", &members).await;
        let status = opt_element(&body, "Status").unwrap_or_default();
        if !status.ends_with("_PENDING") && !status.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the change set was still {status:?} after {SETTLE_DEADLINE:?}, so \
             nothing settled it: {body}"
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
        if !status.ends_with("_IN_PROGRESS") {
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

/// The plan a `DescribeChangeSet` answer carries, as `logical id -> (action, physical id)`.
///
/// Each pair is read from the `ResourceChange` block that carries them, rather than by zipping
/// document-ordered element lists: an addition reports no physical id at all, so a zip would
/// pair the wrong halves instead of showing an empty one.
fn plan_of(xml: &str) -> BTreeMap<String, (String, String)> {
    blocks_of(xml, "ResourceChange")
        .into_iter()
        .filter_map(|change| {
            let logical_id = opt_element(change, "LogicalResourceId")?;
            let action = opt_element(change, "Action")?;
            let physical_id = opt_element(change, "PhysicalResourceId").unwrap_or_default();
            Some((logical_id, (action, physical_id)))
        })
        .collect()
}

/// The two names a row's template states, in declaration order.
fn names(prefix: &str) -> [String; 2] {
    [format!("{prefix}/one"), format!("{prefix}/two")]
}

/// A template declaring two `AWS::SSM::Parameter` resources under the names `prefix` builds.
///
/// `Type` and `Value` are the type's two required properties and `Name` is the custom name
/// `ImportExistingResources` adopts by (source: the CloudFormation resource reference for
/// `AWS::SSM::Parameter`). `parameterised` states each name as an `Fn::Sub` over a template
/// parameter the request supplies instead of as a literal — the form a CDK app's template
/// states a named resource in — so the name the check asks about is the one the execution
/// would really produce.
fn template(prefix: &str, parameterised: bool) -> String {
    let [first, second] = names(prefix);
    if parameterised {
        return format!(
            r#"{{"Parameters":{{"Prefix":{{"Type":"String"}}}},
                "Resources":{{
                  "First":{{"Type":"AWS::SSM::Parameter","Properties":{{
                     "Name":{{"Fn::Sub":"${{Prefix}}/one"}},
                     "Type":"String","Value":"{TEMPLATE_VALUE}"}}}},
                  "Second":{{"Type":"AWS::SSM::Parameter","Properties":{{
                     "Name":{{"Fn::Sub":"${{Prefix}}/two"}},
                     "Type":"String","Value":"{TEMPLATE_VALUE}"}}}}}}}}"#
        );
    }
    format!(
        r#"{{"Resources":{{
              "First":{{"Type":"AWS::SSM::Parameter","Properties":{{
                 "Name":"{first}","Type":"String","Value":"{TEMPLATE_VALUE}"}}}},
              "Second":{{"Type":"AWS::SSM::Parameter","Properties":{{
                 "Name":"{second}","Type":"String","Value":"{TEMPLATE_VALUE}"}}}}}}}}"#
    )
}

/// What the rule says a row's plan must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// An executable plan whose two changes carry these actions, in declaration order.
    Plan([&'static str; 2]),
    /// A change set the resource-existence validation failed, with nothing provisioned.
    FailedExistenceCheck,
}

/// One row: a request, and what the rule says the change set and then the two parameters must
/// be.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The prefix both of this row's names are built from, unique across the suite.
    prefix: &'static str,
    /// How many of the two declared names a client already holds, in declaration order.
    held: usize,
    /// `ImportExistingResources` as the request states it, or `None` to leave it out.
    adopting: Option<bool>,
    /// Whether the request also turns the pre-deployment validations off.
    disable_validation: bool,
    /// Whether the template states its names through a template parameter.
    parameterised: bool,
    expected: Expected,
    /// Whether the row goes on to EXECUTE the plan it asserted. A plan whose addition is
    /// under a name that is already held deploys into the resource-level collision other
    /// suites cover, so the one row that carries such an addition stops at the plan.
    executes: bool,
}

/// RULE: `ImportExistingResources` adopts every resource the plan would create under a name
/// its owning service already holds — reported as an `Import` carrying that identifier, and
/// executed as an adoption that leaves the resource exactly as it was — while a plan that does
/// not ask for adoption is failed on the same name. Turning the pre-deployment validations off
/// does not turn adoption off: it is what the plan does, not a validation.
#[tokio::test]
async fn an_auto_importing_change_set_adopts_every_parameter_that_already_exists() {
    let router = build_router();

    let rows: Vec<Row> = vec![
        Row {
            case: "neither name held, adoption asked for",
            stack: "impx-none-held",
            prefix: "/impx/none",
            held: 0,
            adopting: Some(true),
            disable_validation: false,
            parameterised: false,
            expected: Expected::Plan(["Add", "Add"]),
            executes: true,
        },
        Row {
            case: "one of the two names held, adoption asked for",
            stack: "impx-one-held",
            prefix: "/impx/one",
            held: 1,
            adopting: Some(true),
            disable_validation: false,
            parameterised: false,
            expected: Expected::Plan(["Import", "Add"]),
            executes: true,
        },
        Row {
            case: "both names held, each stated through a template parameter",
            stack: "impx-both-held",
            prefix: "/impx/both",
            held: 2,
            adopting: Some(true),
            disable_validation: false,
            parameterised: true,
            expected: Expected::Plan(["Import", "Import"]),
            executes: true,
        },
        Row {
            case: "one name held, the member left out",
            stack: "impx-absent",
            prefix: "/impx/absent",
            held: 1,
            adopting: None,
            disable_validation: false,
            parameterised: false,
            expected: Expected::FailedExistenceCheck,
            executes: false,
        },
        Row {
            case: "one name held, the member stated false",
            stack: "impx-false",
            prefix: "/impx/false",
            held: 1,
            adopting: Some(false),
            disable_validation: false,
            parameterised: true,
            expected: Expected::FailedExistenceCheck,
            executes: false,
        },
        Row {
            case: "one name held, adoption asked for with the validations turned off",
            stack: "impx-unvalidated",
            prefix: "/impx/unvalidated",
            held: 1,
            adopting: Some(true),
            disable_validation: true,
            parameterised: false,
            expected: Expected::Plan(["Import", "Add"]),
            executes: true,
        },
        Row {
            // The other half of the same decoupling: with the stage off and nothing asked to
            // be adopted, the held name fails nothing and the plan is offered unvalidated —
            // which is what turning the validations off is for.
            case: "one name held, no adoption, the validations turned off",
            stack: "impx-unvalidated-plain",
            prefix: "/impx/plain",
            held: 1,
            adopting: None,
            disable_validation: true,
            parameterised: false,
            expected: Expected::Plan(["Add", "Add"]),
            executes: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let declared = names(row.prefix);
        for name in declared.iter().take(row.held) {
            put_parameter(&router, case, name).await;
        }

        let mut members = vec![
            ("StackName".to_string(), row.stack.to_string()),
            (
                "ChangeSetName".to_string(),
                "cdk-deploy-change-set".to_string(),
            ),
            ("ChangeSetType".to_string(), "CREATE".to_string()),
            (
                "TemplateBody".to_string(),
                template(row.prefix, row.parameterised),
            ),
        ];
        if let Some(adopting) = row.adopting {
            members.push(("ImportExistingResources".to_string(), adopting.to_string()));
        }
        if row.disable_validation {
            members.push(("DisableValidation".to_string(), "true".to_string()));
        }
        if row.parameterised {
            members.push((
                "Parameters.member.1.ParameterKey".to_string(),
                "Prefix".to_string(),
            ));
            members.push((
                "Parameters.member.1.ParameterValue".to_string(),
                row.prefix.to_string(),
            ));
        }
        cloudformation_ok(&router, case, "CreateChangeSet", &members).await;
        let described = settled_change_set(&router, case, row.stack, "cdk-deploy-change-set").await;

        // The change set echoes the member it was created with, whatever the request stated
        // for it: `false` is the default, so a request that left it out reads the same way as
        // one that stated it.
        let echoed = opt_element(&described, "ImportExistingResources").unwrap_or_default();
        assert_eq!(
            echoed,
            row.adopting.unwrap_or(false).to_string(),
            "case {case:?}: the change set must report the member it was created with, got \
             {described}"
        );

        match row.expected {
            Expected::Plan(actions) => {
                let expected: BTreeMap<String, (String, String)> = LOGICAL_IDS
                    .iter()
                    .zip(actions)
                    .zip(declared.iter())
                    .map(|((logical_id, action), name)| {
                        // An adoption reports the identifier the resource ALREADY carries;
                        // an addition has none, because the resource does not exist yet.
                        let physical_id = match action {
                            "Import" => name.clone(),
                            _ => String::new(),
                        };
                        ((*logical_id).to_string(), (action.to_string(), physical_id))
                    })
                    .collect();
                let plan = plan_of(&described);
                assert_eq!(
                    (
                        opt_element(&described, "Status").unwrap_or_default(),
                        opt_element(&described, "ExecutionStatus").unwrap_or_default(),
                        plan.clone(),
                    ),
                    (
                        "CREATE_COMPLETE".to_string(),
                        "AVAILABLE".to_string(),
                        expected.clone()
                    ),
                    "case {case:?}: expected an executable plan reporting {expected:?}, got \
                     {plan:?} from {described}"
                );

                if !row.executes {
                    continue;
                }
                // Executing it brings every adopted parameter under the stack and creates
                // every other one: the stack reports all of them under the names the template
                // states, and only the ones it CREATED carry the template's value.
                cloudformation_ok(
                    &router,
                    case,
                    "ExecuteChangeSet",
                    &[
                        ("StackName".to_string(), row.stack.to_string()),
                        (
                            "ChangeSetName".to_string(),
                            "cdk-deploy-change-set".to_string(),
                        ),
                    ],
                )
                .await;
                let reached = settled_stack_status(&router, case, row.stack).await;
                assert_eq!(
                    reached, "CREATE_COMPLETE",
                    "case {case:?}: expected the execution to complete the stack, got \
                     {reached:?}"
                );
                let resources = cloudformation_ok(
                    &router,
                    case,
                    "DescribeStackResources",
                    &[("StackName".to_string(), row.stack.to_string())],
                )
                .await;
                let reported = physical_ids_of(&resources);
                let expected_ids: BTreeMap<String, String> = LOGICAL_IDS
                    .iter()
                    .zip(declared.iter())
                    .map(|(logical_id, name)| ((*logical_id).to_string(), name.clone()))
                    .collect();
                assert_eq!(
                    reported, expected_ids,
                    "case {case:?}: the stack must manage both names, expected \
                     {expected_ids:?}, got {reported:?} from {resources}"
                );
                for (action, name) in actions.iter().zip(declared.iter()) {
                    let expected_value = match *action {
                        // Adopting a resource brings it under management without modifying
                        // it, so the value the client wrote is still the one in force.
                        "Import" => OUT_OF_BAND_VALUE,
                        _ => TEMPLATE_VALUE,
                    };
                    let held = parameter_value(&router, name).await;
                    assert_eq!(
                        held.as_deref(),
                        Some(expected_value),
                        "case {case:?}: the {action} of {name:?} must leave Systems Manager \
                         holding {expected_value:?}, got {held:?}"
                    );
                }
            }
            Expected::FailedExistenceCheck => {
                let reason = opt_element(&described, "StatusReason").unwrap_or_default();
                assert_eq!(
                    (
                        opt_element(&described, "Status").unwrap_or_default(),
                        opt_element(&described, "ExecutionStatus").unwrap_or_default(),
                        reason.as_str(),
                    ),
                    (
                        "FAILED".to_string(),
                        "UNAVAILABLE".to_string(),
                        EXISTENCE_CHECK_FAILED
                    ),
                    "case {case:?}: expected the existence check to fail the change set, got \
                     {described}"
                );
                // Nothing was provisioned: the stack a CREATE change set brought into
                // existence stays in review, and neither name changed hands.
                let described_stack = cloudformation_ok(
                    &router,
                    case,
                    "DescribeStacks",
                    &[("StackName".to_string(), row.stack.to_string())],
                )
                .await;
                let status = opt_element(&described_stack, "StackStatus").unwrap_or_default();
                assert_eq!(
                    status, "REVIEW_IN_PROGRESS",
                    "case {case:?}: expected the stack to stay in review, got {described_stack}"
                );
                let held = parameter_value(&router, &declared[0]).await;
                assert_eq!(
                    held.as_deref(),
                    Some(OUT_OF_BAND_VALUE),
                    "case {case:?}: the refused plan must leave {:?} as the client wrote it, \
                     got {held:?}",
                    declared[0]
                );
                let free = parameter_value(&router, &declared[1]).await;
                assert_eq!(
                    free, None,
                    "case {case:?}: the refused plan must create nothing, yet {:?} is held as \
                     {free:?}",
                    declared[1]
                );
            }
        }
    }
}
