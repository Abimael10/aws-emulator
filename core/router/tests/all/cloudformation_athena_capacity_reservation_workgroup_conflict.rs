// SPDX-License-Identifier: CC0-1.0

//! A stack whose capacity reservation would claim a workgroup another live reservation
//! already serves does not deploy: Amazon Athena refuses the assignment, CloudFormation rolls
//! the deployment back, and neither the reservation that was already serving the workgroup nor
//! the capacity the failed resource reserved is left in the wrong state.
//!
//! The rule under test: **a workgroup's capacity is served by at most ONE capacity
//! reservation, so a declaration whose reservation would claim a workgroup another live
//! reservation assigns is refused by Athena with HTTP 400 `InvalidRequestException`; the
//! resource FAILS, its `ResourceStatusReason` carrying that service's own sentence inside the
//! handler envelope under the `InvalidRequest` class, the stack ends in its ROLLBACK status,
//! the reservation that was already serving the workgroup is untouched, and the reservation
//! the failed resource created is taken back out of the account.** Two crates have to agree
//! for that to hold (CloudFormation makes the calls and wraps the refusal, Athena words it and
//! arbitrates the workgroup), so the suite lives here: a crate-local test can only ever see
//! one side of it.
//!
//! A `Ref` to an `AWS::Athena::CapacityReservation` resolves to the reservation's ARN — the
//! resource type's own primary identifier — so the output each stack publishes is asserted to
//! be that ARN, and the positive control below is what shows a REPLACEMENT changing it.
//!
//! The workgroups are declared under the property the TYPE publishes (`WorkgroupNames`), which
//! the deployment has to apply under the member `PutCapacityAssignmentConfiguration` takes
//! (`WorkGroupNames`): a declaration that reached Athena under the type's own spelling would
//! assign no workgroup at all, and no conflict would ever arise.
//!
//! The rows differ along how the second claim arrives: as a RENAME of the reservation inside
//! one stack (a replacement, the new reservation claiming what its still-live predecessor
//! holds), and as a SECOND STACK declaring a reservation of its own over the same workgroup.
//! The positive control is the same rename once the assignment is no longer declared, which
//! must complete.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::WORK_GROUPS_ASSIGNED_TO_ANOTHER_RESERVATION;
use smithy_server_runtime::MultiServiceRouter;

use crate::cfn_pipes_pipe_stabilisation_failure_class::{BEFORE_THE_TOKEN, ENVELOPE_OPENS};
use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// How long one poll waits before the next, and how many are made.
///
/// What is waited for is the CONDITION — a stack out of its `_IN_PROGRESS` statuses, a
/// reservation in the status Athena reports for it — and the bound is only a safety net far
/// above every clock-owed span involved, so a loaded host cannot decide an outcome. Nothing
/// here is asserted about elapsed time.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 1_200;

/// The `x-amz-target` prefix of Amazon Athena: the local name of its Smithy service shape
/// (`com.amazonaws.athena#AmazonAthena`), which is how the awsJson protocols build that header.
const ATHENA_TARGET_PREFIX: &str = "AmazonAthena";

/// The Region every stack here deploys in, which is the Region the capacity is reserved in.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The status a reservation holding the capacity it asked for is in.
const ACTIVE: &str = "ACTIVE";

/// The capacity every reservation here asks for: the smallest whole block Athena reserves.
const TARGET_DPUS: i64 = 4;

/// The logical id of the reservation in every template below, which is the resource whose
/// create has to fail.
const CAPACITY_RESERVATION: &str = "CapRes";

/// The `HandlerErrorCode` class a refusal of the REQUEST is filed under, which is what a
/// client reads to see that the failure is not worth retrying.
const INVALID_REQUEST_CLASS: &str = "InvalidRequest";

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
    router.register_service(service_com_amazonaws_athena_amazonathena::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
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
    members: &[(&str, &str)],
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

/// Call one Amazon Athena operation over the awsJson1_1 wire its model declares.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ATHENA_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("athena"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Wait for `stack` to reach a TERMINAL status, and answer with that status and the
/// `DescribeStacks` body that reported it.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks({stack}) must answer 200, got {status} with {described}"
        );
        body = described;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return (reached, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the stack {stack} never settled; last answer was {body}");
}

/// The `CapacityReservation` Athena reports for `name` once it is in `status`.
async fn reservation_in(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    status: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (code, body) = athena(router, "GetCapacityReservation", &json!({ "Name": name })).await;
        last = body;
        if code == 200 && last["CapacityReservation"]["Status"] == json!(status) {
            return last["CapacityReservation"].clone();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the reservation {name} never reached {status}; last read {last}");
}

/// Every reservation name the account holds, as `ListCapacityReservations` reports them.
async fn listed(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = athena(router, "ListCapacityReservations", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListCapacityReservations must answer 200, got {status} with {body}"
    );
    body["CapacityReservations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|reservation| reservation["Name"].as_str().map(ToString::to_string))
        .collect()
}

/// The workgroups Athena reports as the capacity assignment configuration of `reservation`.
async fn serves(router: &MultiServiceRouter, case: &str, reservation: &str) -> Vec<String> {
    let (status, body) = athena(
        router,
        "GetCapacityAssignmentConfiguration",
        &json!({ "CapacityReservationName": reservation }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the assignment configuration of {reservation} must be readable, got \
         {status} with {body}"
    );
    body["CapacityAssignmentConfiguration"]["CapacityAssignments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|assignment| {
            assignment["WorkGroupNames"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|work_group| work_group.as_str().map(ToString::to_string))
        .collect()
}

/// The `ResourceStatusReason` of the event in which the reservation resource FAILED.
async fn failure_reason(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {status} with {events}"
    );
    let reasons: Vec<String> = members_of(&events)
        .into_iter()
        .filter(|event| {
            elements(event, "LogicalResourceId")
                .first()
                .is_some_and(|logical| logical == CAPACITY_RESERVATION)
                && elements(event, "ResourceStatus")
                    .first()
                    .is_some_and(|status| status.ends_with("_FAILED"))
        })
        .filter_map(|event| elements(event, "ResourceStatusReason").into_iter().next())
        .collect();
    assert_eq!(
        reasons.len(),
        1,
        "case {case:?}: expected exactly one failure event for {CAPACITY_RESERVATION}, got \
         {reasons:?} in {events}"
    );
    reasons.into_iter().next().unwrap_or_default()
}

/// The ARN of the reservation `name`, which is the value a `Ref` to the resource resolves to:
/// the resource type's own primary identifier, in the form the IAM Service Authorization
/// Reference publishes for an Athena capacity reservation.
fn reservation_arn(name: &str) -> String {
    format!("arn:aws:athena:{REGION}:{ACCOUNT}:capacity-reservation/{name}")
}

/// One output of a `DescribeStacks` answer.
fn output(described: &str, case: &str, key: &str) -> String {
    let outputs = outputs_of(described);
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The template one row deploys: a workgroup, a reservation named `reservation`, and — where
/// `assign` says so — that workgroup assigned to the reservation by `Ref`.
fn template(work_group: &str, reservation: &str, assign: bool) -> String {
    let mut properties = json!({
        "Name": reservation,
        "TargetDpus": TARGET_DPUS,
    });
    if assign {
        properties["CapacityAssignmentConfiguration"] = json!({
            "CapacityAssignments": [{ "WorkgroupNames": [{ "Ref": "WG" }] }]
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "WG": {
                "Type": "AWS::Athena::WorkGroup",
                "Properties": { "Name": work_group }
            },
            CAPACITY_RESERVATION: {
                "Type": "AWS::Athena::CapacityReservation",
                "DependsOn": "WG",
                "Properties": properties
            }
        },
        "Outputs": { "ReservationRef": { "Value": { "Ref": CAPACITY_RESERVATION } } }
    })
    .to_string()
}

/// Hold the reason to the envelope CloudFormation reports a resource provider's answer in, and
/// to the exception Athena's own SDK raises inside it: the service's sentence, the diagnostic
/// that SDK appends, and the class the refusal falls in.
///
/// The request id and the request token are values one run MINTS, so they are checked for
/// being present and non-empty rather than for a value.
fn assert_athena_refusal_reason(case: &str, reason: &str) {
    let rest = reason.strip_prefix(ENVELOPE_OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open {ENVELOPE_OPENS:?}, got {reason:?}")
    });
    let (message, rest) = rest.split_once(BEFORE_THE_TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected the quoted message to close {BEFORE_THE_TOKEN:?}, got {reason:?}")
    });
    let (token, class) = rest.split_once(", HandlerErrorCode: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected the envelope to name the handler class, got {reason:?}")
    });
    assert!(
        !token.is_empty(),
        "case {case:?}: expected the resource request's own token, got {reason:?}"
    );
    assert_eq!(
        class.strip_suffix(')').unwrap_or(class),
        INVALID_REQUEST_CLASS,
        "case {case:?}: expected a refusal of the request, got {reason:?}"
    );

    // Athena's own sentence, then the diagnostic its SDK appends to a refused call: the
    // service as its SDK client names it, the status it answered, the id of the call and how
    // many attempts the SDK made at it — one, a refusal of the request not being retried.
    let opens = " (Service: Athena, Status Code: 400, Request ID: ";
    let (sentence, rest) = message.split_once(opens).unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK diagnostic {opens:?}, got {message:?}")
    });
    assert_eq!(
        sentence, WORK_GROUPS_ASSIGNED_TO_ANOTHER_RESERVATION,
        "case {case:?}: expected Athena's own sentence, got {message:?}"
    );
    let (request_id, attempts) = rest.split_once(") (SDK Attempt Count: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected the call's id and attempt count, got {message:?}")
    });
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the refused call's own id, got {message:?}"
    );
    assert_eq!(
        attempts.strip_suffix(')').unwrap_or(attempts),
        "1",
        "case {case:?}: a refusal of the request is raised on the first attempt, got {message:?}"
    );
}

#[tokio::test]
async fn a_rename_that_would_double_assign_a_workgroup_rolls_the_update_back() {
    let case = "renaming the reservation while the assignment stands";
    let router = build_router();
    let stack = "athena-capacity-rename";
    let work_group = "rename-analytics";

    // The deployment that stands: the reservation serves the workgroup the template assigns to
    // it, and the stack publishes the reservation's ARN.
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template(work_group, "capres-first", true)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} with {body}"
    );
    let (reached, described) = settled(&router, case, stack).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: the first deployment must complete, got {reached:?} with {described}"
    );
    assert_eq!(
        output(&described, case, "ReservationRef"),
        reservation_arn("capres-first"),
        "case {case:?}: a Ref to the reservation must resolve to its ARN"
    );
    let first = reservation_in(&router, case, "capres-first", ACTIVE).await;
    assert_eq!(
        first["TargetDpus"].as_i64(),
        Some(TARGET_DPUS),
        "case {case:?}: the reservation must request what the template declared, got {first}"
    );
    assert_eq!(
        serves(&router, case, "capres-first").await,
        vec![work_group.to_string()],
        "case {case:?}: the declared workgroup must run on the reservation"
    );

    // The RENAME: the name is fixed when a reservation is created, so the update replaces the
    // resource — and the replacement cannot claim the workgroup its still-live predecessor
    // serves, so the deployment rolls back.
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template(work_group, "capres-renamed", true)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateStack must answer 200, got {status} with {body}"
    );
    let (reached, described) = settled(&router, case, stack).await;
    assert_eq!(
        reached, "UPDATE_ROLLBACK_COMPLETE",
        "case {case:?}: the update must roll back, got {reached:?} with {described}"
    );
    assert_athena_refusal_reason(case, &failure_reason(&router, case, stack).await);

    // The reservation that was already serving the workgroup is untouched, and it still serves
    // it; the stack publishes that reservation's ARN again.
    let first = reservation_in(&router, case, "capres-first", ACTIVE).await;
    assert_eq!(
        first["TargetDpus"].as_i64(),
        Some(TARGET_DPUS),
        "case {case:?}: the rolled-back update must leave the reservation as it was, got {first}"
    );
    assert_eq!(
        serves(&router, case, "capres-first").await,
        vec![work_group.to_string()],
        "case {case:?}: the workgroup must still run on the reservation that serves it"
    );
    assert_eq!(
        output(&described, case, "ReservationRef"),
        reservation_arn("capres-first"),
        "case {case:?}: the rolled-back stack must publish the reservation it kept"
    );

    // And the capacity the failed resource reserved is not left behind: the account holds the
    // first reservation and nothing under the declared replacement name.
    let held = listed(&router, case).await;
    assert!(
        held.contains(&"capres-first".to_string()) && !held.contains(&"capres-renamed".to_string()),
        "case {case:?}: expected the account to hold only the reservation that survived, got \
         {held:?}"
    );

    // The POSITIVE CONTROL: the same rename, with the assignment no longer declared, is a
    // replacement nothing stands in the way of — so it completes, and the ARN the stack
    // publishes is the new reservation's.
    let case = "renaming the reservation with the assignment no longer declared";
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            (
                "TemplateBody",
                &template(work_group, "capres-renamed", false),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateStack must answer 200, got {status} with {body}"
    );
    let (reached, described) = settled(&router, case, stack).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: the update must complete, got {reached:?} with {described}"
    );
    assert_eq!(
        output(&described, case, "ReservationRef"),
        reservation_arn("capres-renamed"),
        "case {case:?}: a replacement must change the ARN the Ref resolves to"
    );
    let renamed = reservation_in(&router, case, "capres-renamed", ACTIVE).await;
    assert_eq!(
        renamed["TargetDpus"].as_i64(),
        Some(TARGET_DPUS),
        "case {case:?}: the replacement must request what the template declared, got {renamed}"
    );
}

#[tokio::test]
async fn a_declared_assignment_member_the_type_does_not_publish_fails_the_stack() {
    // The other half of the mapping rule: a member of an assignment that the resource type
    // publishes no property for is REFUSED rather than dropped — a reservation reported
    // created while the workgroups the template meant to assign run on no reserved capacity is
    // exactly the shallow success this check exists to catch. The row declares the member
    // under the OWNING API's spelling, which is not a property of this type.
    let case = "an assignment declaring a member the type publishes no property for";
    let router = build_router();
    let stack = "athena-capacity-misdeclared";
    let template = json!({
        "Resources": {
            "WG": {
                "Type": "AWS::Athena::WorkGroup",
                "Properties": { "Name": "misdeclared-analytics" }
            },
            CAPACITY_RESERVATION: {
                "Type": "AWS::Athena::CapacityReservation",
                "DependsOn": "WG",
                "Properties": {
                    "Name": "capres-misdeclared",
                    "TargetDpus": TARGET_DPUS,
                    "CapacityAssignmentConfiguration": {
                        "CapacityAssignments": [{ "WorkGroupNames": ["misdeclared-analytics"] }]
                    }
                }
            }
        }
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} with {body}"
    );
    let (reached, described) = settled(&router, case, stack).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: the stack must roll back, got {reached:?} with {described}"
    );
    let reason = failure_reason(&router, case, stack).await;
    assert!(
        reason.contains("WorkGroupNames"),
        "case {case:?}: the failure must name the member it did not apply, got {reason:?}"
    );
    let held = listed(&router, case).await;
    assert!(
        !held.contains(&"capres-misdeclared".to_string()),
        "case {case:?}: no capacity may be left reserved for a resource that failed, got \
         {held:?}"
    );
}

#[tokio::test]
async fn a_second_stack_claiming_an_assigned_workgroup_rolls_back() {
    let case = "a second stack declaring a reservation over the same workgroup";
    let router = build_router();
    let work_group = "shared-analytics";

    // The stack that gets there first, whose reservation serves the workgroup.
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", "athena-capacity-held"),
            ("TemplateBody", &template(work_group, "capres-held", true)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} with {body}"
    );
    let (reached, _) = settled(&router, case, "athena-capacity-held").await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: the first stack must complete, got {reached:?}"
    );
    reservation_in(&router, case, "capres-held", ACTIVE).await;

    // The second stack declares the SAME workgroup — which it reaches by name, the workgroup
    // being held by the other stack — on a reservation of its own.
    let claiming = json!({
        "Resources": {
            CAPACITY_RESERVATION: {
                "Type": "AWS::Athena::CapacityReservation",
                "Properties": {
                    "Name": "capres-claiming",
                    "TargetDpus": TARGET_DPUS,
                    "CapacityAssignmentConfiguration": {
                        "CapacityAssignments": [{ "WorkgroupNames": [work_group] }]
                    }
                }
            }
        }
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", "athena-capacity-claiming"),
            ("TemplateBody", &claiming),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} with {body}"
    );
    let (reached, described) = settled(&router, case, "athena-capacity-claiming").await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: the second stack must roll back, got {reached:?} with {described}"
    );
    assert_athena_refusal_reason(
        case,
        &failure_reason(&router, case, "athena-capacity-claiming").await,
    );

    // The workgroup still runs on the reservation that was serving it, and the reservation the
    // failed resource created is not left holding capacity.
    assert_eq!(
        serves(&router, case, "capres-held").await,
        vec![work_group.to_string()],
        "case {case:?}: the workgroup must still run on the reservation that serves it"
    );
    let held = listed(&router, case).await;
    assert!(
        held.contains(&"capres-held".to_string()) && !held.contains(&"capres-claiming".to_string()),
        "case {case:?}: expected the account to hold only the reservation that survived, got \
         {held:?}"
    );
}
