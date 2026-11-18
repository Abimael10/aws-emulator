// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with a RETAINED resource, and what its event stream says
//! about a create it rolled back.
//!
//! Four rules, each read off one surface a client has:
//!
//! 1. **`DeletionPolicy: RetainExceptOnCreate` keeps a resource exactly as `Retain` keeps it
//!    wherever the stack stops managing a resource it HAS** — the stack's own deletion
//!    included, where the resource's last event is `DELETE_SKIPPED` and the physical resource
//!    goes on existing in the service that owns it. The one event the attribute does not keep
//!    a resource at is the rollback of the operation that CREATED it ("retain the resource
//!    only if the resource creation fails"), which deletes it.
//! 2. **The stack-level events of a create that failed and rolled back are exactly three**:
//!    `CREATE_IN_PROGRESS` reason `User Initiated`, `ROLLBACK_IN_PROGRESS` whose reason is the
//!    AGGREGATE naming every resource the operation could not create, and `ROLLBACK_COMPLETE`
//!    with no reason at all. A resource whose own transition the failure cancelled is one of
//!    the resources that aggregate names, and its own event says the transition was cancelled.
//! 3. **`StackEvent$Timestamp` strictly increases across the log**: `DescribeStackEvents` is
//!    the only ordering a client reconstructing a rollback has, so a resource's failure is
//!    strictly earlier than the stack-level rollback that failure causes.
//! 4. **The clause CloudFormation appends when it rolls a create back goes AFTER the
//!    failure's own sentence, which is reproduced verbatim** — so a `Rules` assertion's
//!    `AssertDescription` reads back character for character, its own final period and
//!    CloudFormation's both, and the resource-failure summary (which ends with its separating
//!    space) gains no second period.
//!
//! Two crates have to agree for the first rule to hold — CloudFormation decides whether to
//! ask, and AWS Systems Manager is what still holds (or no longer holds) the parameter — so
//! the suite lives here rather than in either crate.
//!
//! The rows span what each rule generalises over: every value `DeletionPolicy` takes
//! (absent, `Delete`, `Retain`, `RetainExceptOnCreate`) against both teardowns (the stack's
//! deletion and a rolled-back create), how many resources one failure leaves unfinished (one,
//! two, three), and how an `AssertDescription` ends (with a period, with no final
//! punctuation, with a bang, and with an inner period of its own).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{form_encode, opt_element};

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// The type CloudFormation reports a stack's OWN events under, which is what separates the
/// stack-level rows of `DescribeStackEvents` from its resources'.
const STACK_RESOURCE_TYPE: &str = "AWS::CloudFormation::Stack";

/// How long a row waits for a stack to leave `*_IN_PROGRESS`, and how often it looks.
///
/// The wait is for the CONDITION rather than for a span: a row whose rollback has more
/// transitions to drain only takes more looks, and the deadline is a safety net so a wait
/// nothing will satisfy ends in a failure rather than a hang. Nothing here asserts how long
/// anything took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The `Timeout` a wait condition that nothing will ever signal is declared with: the
/// shortest span a declaration can ask for, so the create fails as soon as the clock owes it.
const EXPIRING_TIMEOUT: &str = "1";

/// The `Timeout` the waits BESIDE it are declared with — far longer than any row's own
/// deadline, so such a wait is still open when the first one runs out and it is the FAILURE
/// rather than the clock that ends it.
const LONG_TIMEOUT: &str = "3600";

/// The reason CloudFormation words the expiry of a wait condition nothing signalled with.
const TIMED_OUT: &str = "WaitCondition timed out. Received 0 conditions when expecting 1";

/// The reason CloudFormation reports on a resource whose creation it ABANDONED because the
/// operation had already failed elsewhere.
const CREATION_CANCELLED: &str = "Resource creation cancelled";

/// The reason a stack's own opening `CREATE_IN_PROGRESS` event carries.
const USER_INITIATED: &str = "User Initiated";

/// The clause CloudFormation appends to the reason of a create it rolls back.
const ROLLBACK_CLAUSE: &str = "Rollback requested by user.";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
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

/// Call one AWS Systems Manager operation (`sigv4Name: ssm`, service shape `AmazonSSM`).
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

/// Whether AWS Systems Manager still holds the parameter `name`, read through its own
/// `GetParameter` — the only evidence that a resource a stack released really survived.
async fn parameter_is_held(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, answer) = ssm(router, "GetParameter", &json!({ "Name": name })).await;
    match status {
        200 => true,
        400 => false,
        other => panic!(
            "case {case:?}: expected GetParameter({name}) to answer 200 or 400, got {other} with \
             body {answer}"
        ),
    }
}

/// Every event one `DescribeStackEvents` answer carries is read through the ONE `StackEvent`
/// reader of this test target ([`super::cloudformation_records_per_resource_stack_events`]),
/// which already separates the answer's `<member>` blocks rather than zipping two
/// document-ordered element lists.
use super::cloudformation_records_per_resource_stack_events::{wire_events, WireEvent};

/// The events `stack_name` has recorded, OLDEST first — the order the stack went through
/// them, which is what every assertion below reads.
async fn events_of(router: &MultiServiceRouter, case: &str, stack_name: &str) -> Vec<WireEvent> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents({stack_name}), got {status} \
         with body {body}"
    );
    let mut events = wire_events(&body);
    events.reverse();
    events
}

/// The status `stack_name` rests in once CloudFormation has left `*_IN_PROGRESS`, read by
/// LOOKING again until that holds or [`SETTLE_DEADLINE`] passes.
async fn settled_stack_status(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.is_empty() && !reached.ends_with("_IN_PROGRESS") {
            return reached;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack_name} was still {reached:?} after {SETTLE_DEADLINE:?}, so \
             nothing settled it: {described}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The deletion of `stack_name`, waited out the way every other settle here is: a deleted
/// stack answers `DescribeStacks` by name with a refusal, which is the terminal state this
/// returns on.
async fn deleted_stack_status(
    router: &MultiServiceRouter,
    case: &str,
    stack_id: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_id)]).await;
        let reached = match status {
            200 => opt_element(&described, "StackStatus").unwrap_or_default(),
            _ => panic!(
                "case {case:?}: expected HTTP 200 from DescribeStacks({stack_id}), got {status} \
                 with body {described}"
            ),
        };
        if !reached.is_empty() && !reached.ends_with("_IN_PROGRESS") {
            return reached;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack_id} was still {reached:?} after {SETTLE_DEADLINE:?}: \
             {described}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The `StackId` a create answered with, which is what a deleted stack is read back by.
fn stack_id_of(created: &str) -> String {
    opt_element(created, "StackId").unwrap_or_default()
}

/// The declaration of one `AWS::SSM::Parameter`, carrying `policy` as its `DeletionPolicy`
/// where the row states one.
fn parameter_resource(name: &str, policy: Option<&str>) -> Value {
    let mut declaration = json!({
        "Type": "AWS::SSM::Parameter",
        "Properties": { "Name": name, "Type": "String", "Value": "kept-or-not" },
    });
    if let Some(policy) = policy {
        declaration["DeletionPolicy"] = json!(policy);
    }
    declaration
}

/// The status a resource's LAST event reports, which is what became of it.
fn last_status_of(events: &[WireEvent], logical_id: &str) -> Option<String> {
    events
        .iter()
        .filter(|event| event.logical_id == logical_id)
        .map(|event| event.status.clone())
        .next_back()
}

/// One row of the retention table: the `DeletionPolicy` a declared parameter carries, and
/// what each of the two teardowns owes it.
struct Retention {
    case: &'static str,
    /// The attribute the declaration carries, or `None` for a declaration that states none —
    /// the documented default.
    policy: Option<&'static str>,
    /// The resource's last event, and whether AWS Systems Manager still holds the parameter,
    /// once the completed stack is DELETED.
    on_delete: (&'static str, bool),
    /// The same two, once a create that FAILED is rolled back.
    on_rolled_back_create: (&'static str, bool),
}

/// The class the retention rule covers: every value the `DeletionPolicy` resource attribute
/// takes. Source: the attribute's own documented values — `Delete` (the default), `Retain`,
/// `RetainExceptOnCreate` and `Snapshot` — minus `Snapshot`, which asks the owning service
/// for a final snapshot of a type that takes none and is a different rule.
fn retention_rows() -> Vec<Retention> {
    vec![
        Retention {
            case: "no DeletionPolicy at all",
            policy: None,
            on_delete: ("DELETE_COMPLETE", false),
            on_rolled_back_create: ("DELETE_COMPLETE", false),
        },
        Retention {
            case: "the documented default stated outright",
            policy: Some("Delete"),
            on_delete: ("DELETE_COMPLETE", false),
            on_rolled_back_create: ("DELETE_COMPLETE", false),
        },
        Retention {
            case: "Retain",
            policy: Some("Retain"),
            on_delete: ("DELETE_SKIPPED", true),
            on_rolled_back_create: ("DELETE_SKIPPED", true),
        },
        Retention {
            case: "RetainExceptOnCreate",
            policy: Some("RetainExceptOnCreate"),
            // Kept on the stack's deletion exactly as `Retain` keeps it...
            on_delete: ("DELETE_SKIPPED", true),
            // ...and deleted on the rollback of the create that made it, which is the one
            // event the attribute names.
            on_rolled_back_create: ("DELETE_COMPLETE", false),
        },
    ]
}

#[tokio::test]
async fn a_retaining_deletion_policy_keeps_its_resource_when_the_stack_is_deleted() {
    let router = build_router();
    for (index, row) in retention_rows().iter().enumerate() {
        let case = row.case;
        let stack_name = format!("retain-on-delete-{index}");
        let parameter = format!("/retain-on-delete-{index}/kept");
        let template = json!({
            "Resources": { "Held": parameter_resource(&parameter, row.policy) },
        })
        .to_string();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let stack_id = stack_id_of(&created);
        let reached = settled_stack_status(&router, case, &stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached:?}"
        );
        assert!(
            parameter_is_held(&router, case, &parameter).await,
            "case {case:?}: expected the create to leave {parameter} in AWS Systems Manager"
        );

        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        let reached = deleted_stack_status(&router, case, &stack_id).await;
        assert_eq!(
            reached, "DELETE_COMPLETE",
            "case {case:?}: expected the deletion to complete, got {reached:?}"
        );

        let events = events_of(&router, case, &stack_id).await;
        let (expected_status, survives) = row.on_delete;
        let last = last_status_of(&events, "Held");
        assert_eq!(
            last.as_deref(),
            Some(expected_status),
            "case {case:?}: expected the resource's last event to be {expected_status:?}, got \
             {last:?} in {events:?}"
        );
        let held = parameter_is_held(&router, case, &parameter).await;
        assert_eq!(
            held, survives,
            "case {case:?}: expected AWS Systems Manager to hold {parameter} = {survives}, got \
             {held}"
        );
    }
}

#[tokio::test]
async fn a_rolled_back_create_keeps_only_what_its_resources_policies_retain() {
    let router = build_router();
    for (index, row) in retention_rows().iter().enumerate() {
        let case = row.case;
        let stack_name = format!("retain-on-rollback-{index}");
        let parameter = format!("/retain-on-rollback-{index}/made");
        // The parameter is created BEFORE the gate that fails the create, so the rollback has
        // a resource of this create's own to decide about.
        let template = json!({
            "Resources": {
                "Held": parameter_resource(&parameter, row.policy),
                "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" },
                "Gate": {
                    "Type": "AWS::CloudFormation::WaitCondition",
                    "DependsOn": "Held",
                    "Properties": {
                        "Handle": { "Ref": "Handle" },
                        "Timeout": EXPIRING_TIMEOUT,
                        "Count": "1",
                    },
                },
            },
        })
        .to_string();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let reached = settled_stack_status(&router, case, &stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the create whose wait ran out to roll back, got {reached:?}"
        );

        let events = events_of(&router, case, &stack_name).await;
        let (expected_status, survives) = row.on_rolled_back_create;
        let last = last_status_of(&events, "Held");
        assert_eq!(
            last.as_deref(),
            Some(expected_status),
            "case {case:?}: expected the resource's last event to be {expected_status:?}, got \
             {last:?} in {events:?}"
        );
        let held = parameter_is_held(&router, case, &parameter).await;
        assert_eq!(
            held, survives,
            "case {case:?}: expected AWS Systems Manager to hold {parameter} = {survives}, got \
             {held}"
        );
    }
}

/// One row of the rollback-report table: how many waits the create declares, in declaration
/// order, the first of which is the one whose `Timeout` runs out.
struct Rollback {
    case: &'static str,
    waits: &'static [&'static str],
}

/// The class the aggregate rule covers: a create failing at a wait with any number of the
/// operation's other resources left unfinished.
fn rollback_rows() -> Vec<Rollback> {
    vec![
        Rollback {
            case: "one wait, which is the one that ran out",
            waits: &["Only"],
        },
        Rollback {
            case: "two waits, one of which the failure cancels",
            waits: &["Fast", "Slow"],
        },
        Rollback {
            case: "three waits, two of which the failure cancels",
            waits: &["First", "Second", "Third"],
        },
    ]
}

/// The template one rollback row deploys: a handle and one wait condition per declared id,
/// the first asking for [`EXPIRING_TIMEOUT`] and the rest for [`LONG_TIMEOUT`].
fn waiting_template(waits: &[&str]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Handle".to_string(),
        json!({ "Type": "AWS::CloudFormation::WaitConditionHandle" }),
    );
    for (index, logical_id) in waits.iter().enumerate() {
        let timeout = if index == 0 {
            EXPIRING_TIMEOUT
        } else {
            LONG_TIMEOUT
        };
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "AWS::CloudFormation::WaitCondition",
                "Properties": {
                    "Handle": { "Ref": "Handle" },
                    "Timeout": timeout,
                    "Count": "1",
                },
            }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// The stack-level rows of an event log: the ones whose `ResourceType` is the stack's own.
fn stack_level(events: &[WireEvent]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter(|event| event.resource_type == STACK_RESOURCE_TYPE)
        .map(|event| (event.status.clone(), event.status_reason.clone()))
        .collect()
}

#[tokio::test]
async fn the_stack_level_events_of_a_rolled_back_create_name_every_resource_it_failed() {
    let router = build_router();
    for (index, row) in rollback_rows().iter().enumerate() {
        let case = row.case;
        let stack_name = format!("rollback-events-{index}");
        let template = waiting_template(row.waits);

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let reached = settled_stack_status(&router, case, &stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the create whose wait ran out to roll back, got {reached:?}"
        );

        // The aggregate names EVERY resource the create could not carry through, in the order
        // the template declared them, and the clause goes in the separating space the
        // summary's own sentence ends with — one period, not two.
        let named = row.waits.join(", ");
        let aggregate =
            format!("The following resource(s) failed to create: [{named}]. {ROLLBACK_CLAUSE}");
        let expected: Vec<(String, Option<String>)> = vec![
            (
                "CREATE_IN_PROGRESS".to_string(),
                Some(USER_INITIATED.to_string()),
            ),
            ("ROLLBACK_IN_PROGRESS".to_string(), Some(aggregate)),
            ("ROLLBACK_COMPLETE".to_string(), None),
        ];
        let events = events_of(&router, case, &stack_name).await;
        let reported = stack_level(&events);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stack's own events to be {expected:?}, got {reported:?}"
        );

        // ...and each resource says for itself why it is in the aggregate: the wait that ran
        // out reports the expiry, and every wait the failure left unfinished reports that its
        // creation was cancelled.
        for (position, logical_id) in row.waits.iter().enumerate() {
            let expected_reason = if position == 0 {
                TIMED_OUT
            } else {
                CREATION_CANCELLED
            };
            let failure = events.iter().find(|event| {
                event.logical_id == *logical_id && event.status == "CREATE_FAILED"
            });
            let reason = failure.and_then(|event| event.status_reason.clone());
            assert_eq!(
                reason.as_deref(),
                Some(expected_reason),
                "case {case:?}: expected {logical_id}'s CREATE_FAILED to report \
                 {expected_reason:?}, got {reason:?} in {events:?}"
            );
        }
    }
}

/// One event instant as a COMPARABLE key: the text with its fractional second padded to a
/// fixed width.
///
/// An ISO 8601 instant prints as many fractional digits as it has, so two of them compare by
/// time only once the fraction is the same width — without the padding `…:11.6Z` would sort
/// before `…:11.59Z`.
fn instant_key(timestamp: &str) -> String {
    let stripped = timestamp.trim_end_matches('Z');
    match stripped.split_once('.') {
        Some((whole, fraction)) => format!("{whole}.{fraction:0<9}"),
        None => format!("{stripped}.000000000"),
    }
}

#[tokio::test]
async fn every_stack_event_is_strictly_later_than_the_one_before_it() {
    let case = "a create that failed at one wait and cancelled another";
    let router = build_router();
    let stack_name = "rollback-event-order";
    let waits = ["Fast", "Slow"];
    let template = waiting_template(&waits);

    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
    );
    let reached = settled_stack_status(&router, case, stack_name).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the create whose wait ran out to roll back, got {reached:?}"
    );

    let events = events_of(&router, case, stack_name).await;
    assert!(
        events.len() > 3,
        "case {case:?}: expected a rolled-back create to record more than three events, got \
         {events:?}"
    );
    for pair in events.windows(2) {
        let (earlier, later) = (&pair[0], &pair[1]);
        assert!(
            instant_key(&earlier.timestamp) < instant_key(&later.timestamp),
            "case {case:?}: expected {} {} at {:?} to be strictly earlier than {} {} at {:?}",
            earlier.logical_id,
            earlier.status,
            earlier.timestamp,
            later.logical_id,
            later.status,
            later.timestamp
        );
    }

    // The one pair the rule exists for: the resource's failure and the stack-level rollback
    // that failure CAUSES, which one call records.
    let instant_of = |logical_id: &str, status: &str| -> String {
        events
            .iter()
            .find(|event| event.logical_id == logical_id && event.status == status)
            .map(|event| event.timestamp.clone())
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {status} event for {logical_id}: {events:?}")
            })
    };
    let failed = instant_of("Fast", "CREATE_FAILED");
    let rolling_back = instant_of(stack_name, "ROLLBACK_IN_PROGRESS");
    assert!(
        instant_key(&failed) < instant_key(&rolling_back),
        "case {case:?}: expected the wait's CREATE_FAILED at {failed:?} to be strictly earlier \
         than the stack's ROLLBACK_IN_PROGRESS at {rolling_back:?}"
    );
}

/// One row of the assertion-text table: what a template's `Rules` block states as its
/// `AssertDescription`.
struct Assertion {
    case: &'static str,
    description: &'static str,
}

/// The class the verbatim-reason rule covers: an `AssertDescription` however it is
/// punctuated, since the text is the SUBMITTER's and CloudFormation reproduces it.
fn assertion_rows() -> Vec<Assertion> {
    vec![
        Assertion {
            case: "a description ending in a period",
            description: "CDK bootstrap stack version 6 required. Please run 'cdk bootstrap' \
                          with a recent version of the CDK CLI.",
        },
        Assertion {
            case: "a description ending in no punctuation at all",
            description: "prod stacks must use the large size",
        },
        Assertion {
            case: "a description ending in a bang",
            description: "pick a supported size!",
        },
        Assertion {
            case: "a description whose only period is inside it",
            description: "the size must be large. nothing else is supported",
        },
    ]
}

#[tokio::test]
async fn the_rollback_clause_leaves_a_rules_assertion_description_as_the_template_wrote_it() {
    let router = build_router();
    for (index, row) in assertion_rows().iter().enumerate() {
        let case = row.case;
        let stack_name = format!("rule-reason-{index}");
        let template = json!({
            "Parameters": { "Size": { "Type": "String", "Default": "small" } },
            "Rules": {
                "SizeMustBeLarge": {
                    "Assertions": [{
                        "Assert": { "Fn::Equals": [{ "Ref": "Size" }, "large"] },
                        "AssertDescription": row.description,
                    }],
                },
            },
            "Resources": {
                "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" },
            },
        })
        .to_string();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let reached = settled_stack_status(&router, case, &stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected a failing Rules assertion to roll the create back, got \
             {reached:?}"
        );

        // The reason the ROLLBACK's own event carries: the fixed text, the description
        // character for character, and then the clause — so a description that ends in its own
        // period keeps it and CloudFormation's period lands after it.
        let expected = format!(
            "Parameter validation failed: assertion error: {}. {ROLLBACK_CLAUSE}",
            row.description
        );
        let events = events_of(&router, case, &stack_name).await;
        let rolled_back: Vec<String> = events
            .iter()
            .filter(|event| event.status == "ROLLBACK_IN_PROGRESS")
            .filter_map(|event| event.status_reason.clone())
            .collect();
        assert_eq!(
            rolled_back,
            vec![expected.clone()],
            "case {case:?}: expected the rollback's reason to be {expected:?}, got \
             {rolled_back:?} in {events:?}"
        );
        // ...and the description the template declared is in it unaltered, which is what a
        // client matching on its own assertion text looks for.
        let carried = rolled_back
            .first()
            .is_some_and(|reason| reason.contains(row.description));
        assert!(
            carried,
            "case {case:?}: expected the reason to carry {:?} verbatim, got {rolled_back:?}",
            row.description
        );
    }
}
