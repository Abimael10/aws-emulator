// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the stacks CloudFormation provisions through its OWN API.
//!
//! The rule every case here holds the service to: **a resource operation on a registry type
//! whose owning service provisions it through an API of its own is carried out BY that API,
//! and the request's `OperationStatus` is that service's own lifecycle for the resource** —
//! `IN_PROGRESS` for as long as that service reports it still changing, `SUCCESS` when that
//! lifecycle reaches the state the operation asked for, `FAILED` when it reaches another or
//! that service refuses the call — and the resource is read, listed and patched THERE.
//!
//! So an `AWS::CloudFormation::Stack` a `CreateResource` starts is a REAL stack: it is
//! visible through CloudFormation's own `DescribeStacks` and `DescribeStackResource` while it
//! is `CREATE_IN_PROGRESS`, a `SignalResource` against a gate its template declares is what
//! releases it, and only then does the Cloud Control request terminate. While that create
//! holds the resource, a second operation on it is refused the modelled
//! `ConcurrentOperationException` (HTTP 409) naming the request that holds it, and the
//! refusal changes nothing.
//!
//! The rows span what the rule generalises over: how many gates hold the create open and how
//! many signals each needs (so the request terminates after the LAST of them, never on a
//! timer of this service's own), what the desired state carries (parameters, tags, a stack
//! policy), which operation addresses the resource, and which way the owning service refuses
//! a call.
//!
//! `sqs_queue_read_through` holds the same shape of rule for a type whose resources live in
//! another service's STORE, so what is asserted across the two suites is where a resource
//! LIVES rather than anything about one owning service.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::emulator::{
    code, descriptions, element, message, progress, properties_of, text, Emulator,
    SETTLED_INTERVAL, STATUS_CANCEL_COMPLETE, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PENDING,
    STATUS_SUCCESS,
};

/// The type whose resources CloudFormation provisions through its own API.
const STACK: &str = "AWS::CloudFormation::Stack";

/// How long a wait in this suite runs before the case fails.
///
/// Every wait here is for a STATUS another SERVICE reports — a stack's create runs that
/// service's own deployment and its own transitions, each a further request of this
/// emulator — so the budget is far above anything that flow takes, including on a host
/// running the whole workspace's tests at once. No case waits for a span: each re-reads
/// until the status it is after is there or this deadline passes.
const STACK_BUDGET: Duration = Duration::from_secs(180);

/// Poll the request `token` names until it reports a TERMINAL status, answering the
/// `ProgressEvent` it reports then.
async fn terminal(case: &str, emulator: &Emulator, token: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = emulator.status(token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             body {body:?}"
        );
        let event = progress(&body);
        let reached = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&reached.as_str()) {
            return event;
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: expected the request to reach a terminal status within \
             {STACK_BUDGET:?}, it is still {reached:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// [`terminal`], required to have SUCCEEDED.
async fn settled(case: &str, emulator: &Emulator, token: &str) -> Value {
    let event = terminal(case, emulator, token).await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: expected the operation to succeed, got {event:?}"
    );
    event
}

/// Create one resource and wait until the owning service reports it created, answering its
/// primary identifier — the one that service assigned, never a value a case chose.
async fn provision(case: &str, emulator: &Emulator, desired: &Value) -> String {
    let accepted = emulator.create_ok(STACK, desired, None).await;
    let event = settled(case, emulator, &text(&accepted, "RequestToken")).await;
    text(&event, "Identifier")
}

/// Delete one resource and wait until the owning service reports it gone.
async fn delete_and_settle(case: &str, emulator: &Emulator, identifier: &str) {
    let (status, body) = emulator.delete(STACK, identifier).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete of {identifier:?} to be accepted, got {status} with \
         body {body:?}"
    );
    settled(case, emulator, &text(&progress(&body), "RequestToken")).await;
}

/// The status CloudFormation reports for a resource whose creation is still running, and for
/// a gate that is waiting for its signals.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";

/// The status CloudFormation reports for a stack whose create has completed.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status CloudFormation reports for a stack whose deletion has completed — which a
/// NAME no longer resolves to, so a read of it is addressed by the stack id that is this
/// type's primary identifier.
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// One gate of a row: the resource whose `CreationPolicy` waits for signals, and the
/// `UniqueId` of each signal its `Count` asks for.
struct Gate {
    logical_id: &'static str,
    signals: &'static [&'static str],
}

/// A template whose only resources are the gates `gates` names, each waiting for as many
/// signals as it has `UniqueId`s — the shape of template a client bootstraps a stack with.
fn gated_template(gates: &[Gate]) -> String {
    let resources: Value = Value::Object(
        gates
            .iter()
            .map(|gate| {
                (
                    gate.logical_id.to_string(),
                    json!({
                        "Type": "AWS::CloudFormation::WaitCondition",
                        "CreationPolicy": {
                            "ResourceSignal": {
                                "Count": gate.signals.len(),
                                "Timeout": "PT10M",
                            }
                        },
                    }),
                )
            })
            .collect(),
    );
    json!({ "Resources": resources }).to_string()
}

/// The desired state a create of this type carries: the stack's name and the template it is
/// deployed from, which is the one form the type's schema states a template in by value.
fn desired_state(stack_name: &str, template: &str) -> Value {
    json!({ "StackName": stack_name, "TemplateBody": template })
}

/// Poll the request `token` names until it reports the identifier the owning service assigned
/// the resource, answering it — and hold the request to `IN_PROGRESS` all the way there,
/// which is what the create of a gated stack must report.
///
/// The wait is for the identifier and never for a span: it re-reads until one is there or the
/// harness's generous deadline passes.
async fn awaited_identifier(case: &str, emulator: &Emulator, token: &str) -> String {
    let started = Instant::now();
    loop {
        let (status, body) = emulator.status(token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             body {body:?}"
        );
        let event = progress(&body);
        let reported = text(&event, "OperationStatus");
        assert_eq!(
            reported, STATUS_IN_PROGRESS,
            "case {case:?}: expected the create to be held open by the stack's own lifecycle, \
             got {event:?}"
        );
        let identifier = text(&event, "Identifier");
        if !identifier.is_empty() {
            return identifier;
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: expected the create to name the stack it provisions within \
             {STACK_BUDGET:?}, it still reports {event:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// The `ResourceStatus` CloudFormation's own read reports for one resource of one stack.
async fn resource_status(emulator: &Emulator, stack_name: &str, logical_id: &str) -> String {
    let described = emulator
        .cfn_ok(
            "DescribeStackResource",
            &[
                ("StackName", stack_name),
                ("LogicalResourceId", logical_id),
            ],
        )
        .await;
    element(&described, "ResourceStatus")
}

/// The `StackStatus` CloudFormation's own read reports for the stack `name_or_id` names.
async fn stack_status(emulator: &Emulator, name_or_id: &str) -> String {
    let described = emulator
        .cfn_ok("DescribeStacks", &[("StackName", name_or_id)])
        .await;
    element(&described, "StackStatus")
}

/// One row of the gated-create table.
struct GatedCase {
    case: &'static str,
    stack: &'static str,
    gates: &'static [Gate],
}

fn gated_cases() -> Vec<GatedCase> {
    vec![
        GatedCase {
            case: "one gate waiting for one signal",
            stack: "cc-gated-one",
            gates: &[Gate {
                logical_id: "Gate",
                signals: &["release"],
            }],
        },
        GatedCase {
            case: "two independent gates, each waiting for one signal",
            stack: "cc-gated-two",
            gates: &[
                Gate {
                    logical_id: "Gate",
                    signals: &["release-a"],
                },
                Gate {
                    logical_id: "Second",
                    signals: &["release-b"],
                },
            ],
        },
        GatedCase {
            case: "one gate waiting for two signals",
            stack: "cc-gated-count",
            gates: &[Gate {
                logical_id: "Gate",
                signals: &["host-one", "host-two"],
            }],
        },
    ]
}

#[tokio::test]
async fn a_create_follows_the_stacks_own_lifecycle_and_holds_the_resource_while_it_runs() {
    for row in gated_cases() {
        let case = row.case;
        let emulator = Emulator::with_cloudformation();
        let template = gated_template(row.gates);
        let accepted = emulator
            .create_ok(STACK, &desired_state(row.stack, &template), None)
            .await;
        let started = text(&accepted, "OperationStatus");
        assert!(
            started == STATUS_IN_PROGRESS || started == STATUS_PENDING,
            "case {case:?}: expected the create to be answered in a non-terminal status, got \
             {accepted:?}"
        );
        let token = text(&accepted, "RequestToken");

        // The request names the resource once the owning service has assigned its identifier,
        // and goes on running while that service reports the stack still being created.
        let identifier = awaited_identifier(case, &emulator, &token).await;

        // The stack is CloudFormation's own: its reads report it, and each gate the template
        // declares is waiting there.
        let reported = stack_status(&emulator, row.stack).await;
        assert_eq!(
            reported, CREATE_IN_PROGRESS,
            "case {case:?}: expected CloudFormation to report the stack being created, got \
             {reported:?}"
        );
        let described = emulator
            .cfn_ok("DescribeStacks", &[("StackName", row.stack)])
            .await;
        assert_eq!(
            element(&described, "StackId"),
            identifier,
            "case {case:?}: expected the request to name the stack id CloudFormation assigned, \
             got {identifier:?} against {described}"
        );
        for gate in row.gates {
            let waiting = resource_status(&emulator, row.stack, gate.logical_id).await;
            assert_eq!(
                waiting, CREATE_IN_PROGRESS,
                "case {case:?}: expected {} to be reported waiting, got {waiting:?}",
                gate.logical_id
            );
        }

        // A second operation on the resource that create holds is refused, naming the
        // resource under contention and the request that holds it.
        let (status, body) = emulator.delete(STACK, &identifier).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the delete of a held resource to be refused HTTP 409, got \
             {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ConcurrentOperationException"),
            "case {case:?}: expected ConcurrentOperationException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!(
                "Concurrent operation found for resource {identifier} with RequestToken {token}"
            ),
            "case {case:?}: expected the refusal to name the resource and the request that \
             holds it, got {:?}",
            message(&body)
        );

        // ...and the refusal had no effect: the create it collided with is still running.
        let (status, body) = emulator.status(&token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 polling the held create, got {status} with body \
             {body:?}"
        );
        assert_eq!(
            text(&progress(&body), "OperationStatus"),
            STATUS_IN_PROGRESS,
            "case {case:?}: expected the refused delete to leave the create running, got {:?}",
            progress(&body)
        );

        // Nothing but the signals the template asked for releases the stack, and with it the
        // Cloud Control request.
        for gate in row.gates {
            for unique_id in gate.signals {
                emulator
                    .cfn_ok(
                        "SignalResource",
                        &[
                            ("StackName", row.stack),
                            ("LogicalResourceId", gate.logical_id),
                            ("UniqueId", unique_id),
                            ("Status", "SUCCESS"),
                        ],
                    )
                    .await;
            }
        }
        let settled = settled(case, &emulator, &token).await;
        assert_eq!(
            text(&settled, "Identifier"),
            identifier,
            "case {case:?}: expected the completed request to name the same resource, got \
             {settled:?}"
        );
        // What the request leaves behind is the stack the owning service now holds.
        let model: Value = serde_json::from_str(&text(&settled, "ResourceModel"))
            .unwrap_or(Value::Null);
        assert_eq!(
            text(&model, "StackId"),
            identifier,
            "case {case:?}: expected the resource model to carry the stack's own id, got \
             {model:?}"
        );
        assert_eq!(
            text(&model, "StackStatus"),
            CREATE_COMPLETE,
            "case {case:?}: expected the resource model to carry the status CloudFormation \
             reports, got {model:?}"
        );

        // The delete that was refused while the create held the resource is now accepted, and
        // it is CloudFormation that takes the stack down.
        delete_and_settle(case, &emulator, &identifier).await;
        let gone = stack_status(&emulator, &identifier).await;
        assert_eq!(
            gone, DELETE_COMPLETE,
            "case {case:?}: expected CloudFormation to report the stack deleted, got {gone:?}"
        );
        let (status, body) = emulator.get(STACK, &identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted stack to read as not found, got {status} with \
             body {body:?}"
        );
        assert!(
            code(&body).contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException for the deleted stack, got {:?}",
            code(&body)
        );
    }
}

/// One row of the read-through table: what the create's desired state carries beside the
/// stack's name and template.
struct ModelCase {
    case: &'static str,
    stack: &'static str,
    carries: Value,
    /// The parameter the patch below changes, where the row has one.
    patched: Option<(&'static str, &'static str)>,
}

/// A template with no gate at all — a wait condition HANDLE, which is a resource of
/// CloudFormation's own that nothing waits on — declaring the parameters a row passes.
fn ungated_template(parameters: &[&str]) -> String {
    let declared: Value = Value::Object(
        parameters
            .iter()
            .map(|name| ((*name).to_string(), json!({ "Type": "String" })))
            .collect(),
    );
    let mut template = json!({
        "Resources": { "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" } }
    });
    if !parameters.is_empty() {
        template["Parameters"] = declared;
    }
    template.to_string()
}

fn model_cases() -> Vec<ModelCase> {
    vec![
        ModelCase {
            case: "a stack with nothing but a name and a template",
            stack: "cc-model-plain",
            carries: json!({}),
            patched: None,
        },
        ModelCase {
            case: "a stack carrying parameters and a tag set",
            stack: "cc-model-parameters",
            carries: json!({
                "Parameters": { "Size": "small" },
                "Tags": [{ "Key": "owner", "Value": "cloud-control" }],
            }),
            patched: Some(("Size", "large")),
        },
        ModelCase {
            case: "a stack carrying a timeout and a tag set of two",
            stack: "cc-model-timeout",
            carries: json!({
                "TimeoutInMinutes": 30,
                "Tags": [
                    { "Key": "owner", "Value": "cloud-control" },
                    { "Key": "tier", "Value": "test" },
                ],
            }),
            patched: None,
        },
    ]
}

#[tokio::test]
async fn a_stack_is_read_listed_and_patched_where_it_lives() {
    for row in model_cases() {
        let case = row.case;
        let emulator = Emulator::with_cloudformation();
        let parameters: Vec<&str> = row.carries["Parameters"]
            .as_object()
            .map(|declared| declared.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let template = ungated_template(&parameters);
        let mut desired = desired_state(row.stack, &template);
        for (property, value) in row.carries.as_object().cloned().unwrap_or_default() {
            desired[property] = value;
        }
        let identifier = provision(case, &emulator, &desired).await;

        // The resource the request provisioned is the stack CloudFormation holds, under the
        // stack id that is this type's primary identifier.
        let described = emulator
            .cfn_ok("DescribeStacks", &[("StackName", row.stack)])
            .await;
        assert_eq!(
            element(&described, "StackId"),
            identifier,
            "case {case:?}: expected the identifier to be the stack id CloudFormation \
             assigned, got {identifier:?} against {described}"
        );

        // ...and a read through this service reports that stack: its identity, the status
        // CloudFormation computed, and what the create asked for.
        let model = emulator.properties(STACK, &identifier).await;
        assert_eq!(
            text(&model, "StackId"),
            identifier,
            "case {case:?}: expected the read to report the stack's own id, got {model:?}"
        );
        assert_eq!(
            text(&model, "StackName"),
            row.stack,
            "case {case:?}: expected the read to report the stack's name, got {model:?}"
        );
        assert_eq!(
            text(&model, "StackStatus"),
            CREATE_COMPLETE,
            "case {case:?}: expected the read to report the status CloudFormation computed, \
             got {model:?}"
        );
        // The template is a write-only property of this type: a stack's own read reports no
        // template, so the model a client round-trips carries none.
        assert!(
            model.get("TemplateBody").is_none(),
            "case {case:?}: expected the read to report no template body, got {model:?}"
        );
        for (property, expected) in row.carries.as_object().cloned().unwrap_or_default() {
            assert_eq!(
                model.get(&property),
                Some(&expected),
                "case {case:?}: expected the read to report {property} as the create asked \
                 for it, got {model:?}"
            );
        }

        // A listing of the type reports it too, read from the same place.
        let listed = emulator.list(STACK, None, None).await;
        let identifiers: Vec<String> = descriptions(&listed)
            .iter()
            .map(|description| text(description, "Identifier"))
            .collect();
        assert!(
            identifiers.contains(&identifier),
            "case {case:?}: expected the listing to report the stack, got {identifiers:?}"
        );
        let reported = descriptions(&listed)
            .into_iter()
            .find(|description| text(description, "Identifier") == identifier)
            .map(|description| properties_of(&description))
            .unwrap_or(Value::Null);
        assert_eq!(
            reported, model,
            "case {case:?}: expected the listing and the read to report one model, got \
             {reported:?}"
        );

        // A patch is applied BY the owning service: what changes is the stack itself.
        if let Some((parameter, value)) = row.patched {
            let patch = json!([{
                "op": "replace",
                "path": format!("/Parameters/{parameter}"),
                "value": value,
            }]);
            let (status, body) = emulator.update(STACK, &identifier, &patch, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the patch to be accepted, got {status} with body \
                 {body:?}"
            );
            settled(case, &emulator, &text(&progress(&body), "RequestToken")).await;
            let patched = emulator.properties(STACK, &identifier).await;
            assert_eq!(
                patched["Parameters"][parameter],
                json!(value),
                "case {case:?}: expected the patched parameter to be what the stack now \
                 holds, got {patched:?}"
            );
            let described = emulator
                .cfn_ok("DescribeStacks", &[("StackName", row.stack)])
                .await;
            assert!(
                described.contains(value),
                "case {case:?}: expected CloudFormation's own read to report the patched \
                 value {value:?}, got {described}"
            );
        }

        // ...and the delete takes the stack down there, after which neither service reports
        // the resource.
        delete_and_settle(case, &emulator, &identifier).await;
        let gone = stack_status(&emulator, &identifier).await;
        assert_eq!(
            gone, DELETE_COMPLETE,
            "case {case:?}: expected CloudFormation to report the stack deleted, got {gone:?}"
        );
        let identifiers: Vec<String> = descriptions(&emulator.list(STACK, None, None).await)
            .iter()
            .map(|description| text(description, "Identifier"))
            .collect();
        assert!(
            !identifiers.contains(&identifier),
            "case {case:?}: expected the deleted stack to be gone from the listing, got \
             {identifiers:?}"
        );
    }
}

/// One row of the refused-work table: the desired state a create carries, and the
/// `HandlerErrorCode` the owning service's answer maps to.
struct RefusedCase {
    case: &'static str,
    /// The desired state, as the row states it — `None` where the row deletes an identifier
    /// nothing answers to instead.
    desired: Option<Value>,
    /// The handler error code the request must FAIL with.
    error_code: &'static str,
    /// Text the diagnostic must carry, which is the owning service's own wording where that
    /// service is what refused.
    detail: &'static str,
}

fn refused_cases() -> Vec<RefusedCase> {
    vec![
        RefusedCase {
            case: "a desired state carrying no template at all",
            desired: Some(json!({ "StackName": "cc-refused-no-template" })),
            error_code: "InvalidRequest",
            detail: "TemplateBody",
        },
        RefusedCase {
            case: "a desired state naming a read-only property",
            desired: Some(json!({
                "StackName": "cc-refused-read-only",
                "TemplateBody": "{}",
                "StackStatus": "CREATE_COMPLETE",
            })),
            error_code: "InvalidRequest",
            detail: "StackStatus is a read-only property of AWS::CloudFormation::Stack",
        },
        RefusedCase {
            case: "a desired state naming a property the type does not declare",
            desired: Some(json!({
                "StackName": "cc-refused-undeclared",
                "TemplateBody": "{}",
                "Rubbish": "no such property",
            })),
            error_code: "InvalidRequest",
            detail: "Rubbish is not a property of AWS::CloudFormation::Stack",
        },
        RefusedCase {
            case: "a delete naming a stack CloudFormation holds none of",
            desired: None,
            error_code: "NotFound",
            detail: "was not found",
        },
    ]
}

#[tokio::test]
async fn work_the_owning_service_will_not_do_fails_the_request_with_its_own_verdict() {
    for row in refused_cases() {
        let case = row.case;
        let emulator = Emulator::with_cloudformation();
        let accepted = match &row.desired {
            Some(desired) => emulator.create_ok(STACK, desired, None).await,
            None => {
                // An identifier of the form this type's own are, naming a stack nothing
                // created: the delete is accepted, and the owning handler's not-found is
                // what the request then reports.
                let identifier = format!(
                    "arn:aws:cloudformation:us-east-1:{}:stack/cc-refused-absent/0",
                    engine::emulator_account_id()
                );
                let (status, body) = emulator.delete(STACK, &identifier).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the delete to be accepted, got {status} with body \
                     {body:?}"
                );
                progress(&body)
            }
        };
        let token = text(&accepted, "RequestToken");
        let failed = terminal(case, &emulator, &token).await;
        assert_eq!(
            text(&failed, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {failed:?}"
        );
        assert_eq!(
            text(&failed, "ErrorCode"),
            row.error_code,
            "case {case:?}: expected the handler verdict {:?}, got {failed:?}",
            row.error_code
        );
        assert!(
            text(&failed, "StatusMessage").contains(row.detail),
            "case {case:?}: expected the diagnostic to carry {:?}, got {failed:?}",
            row.detail
        );
    }
}

#[tokio::test]
async fn a_create_of_a_name_the_owning_service_already_holds_fails_as_already_exists() {
    let case = "a second create of a name CloudFormation already holds";
    let emulator = Emulator::with_cloudformation();
    let template = ungated_template(&[]);
    let name = "cc-taken-name";
    let identifier = provision(case, &emulator, &desired_state(name, &template)).await;

    // The same name again: the resource is already there, which is the OWNING handler's
    // verdict — the request is accepted and then fails with it, carrying that service's own
    // wording.
    let accepted = emulator
        .create_ok(STACK, &desired_state(name, &template), None)
        .await;
    let failed = terminal(case, &emulator, &text(&accepted, "RequestToken")).await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "expected the second create to fail, got {failed:?}"
    );
    assert_eq!(
        text(&failed, "ErrorCode"),
        "AlreadyExists",
        "expected the already-exists verdict, got {failed:?}"
    );
    assert!(
        text(&failed, "StatusMessage").contains(name),
        "expected the diagnostic to name the stack, got {failed:?}"
    );

    // ...and the stack the first create made is untouched by it.
    let reported = stack_status(&emulator, &identifier).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "expected the refused create to leave the stack it collided with alone, got \
         {reported:?}"
    );
    let still_there = emulator.properties(STACK, &identifier).await;
    assert_eq!(
        text(&still_there, "StackName"),
        name,
        "expected the first stack to still read, got {still_there:?}"
    );
    assert_eq!(
        text(&still_there, "StackStatus"),
        CREATE_COMPLETE,
        "expected the first stack to still report its own status, got {still_there:?}"
    );
    assert_eq!(
        text(&still_there, "StackId"),
        identifier,
        "expected the first stack to answer to the identifier the create reported, got \
         {still_there:?}"
    );
}
