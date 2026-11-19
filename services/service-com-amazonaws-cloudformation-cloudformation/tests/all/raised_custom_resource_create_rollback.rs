// SPDX-License-Identifier: CC0-1.0

//! A custom resource whose PROVIDER'S OWN HANDLER RAISED on the `Create`, and where the
//! rollback of that failed create then ends.
//!
//! RULE under test: **a create a custom resource's provider answered `FAILED` fails that
//! resource with the response status and message the answer carried, and the rollback the
//! failed create owes asks that provider to `Delete` the physical resource its answer NAMED
//! and removes everything else the create had made — so the stack settles
//! `ROLLBACK_COMPLETE` once every one of those removals is carried out, and
//! `ROLLBACK_FAILED` only where one of them could not be.** `StackStatus` distinguishes the
//! two (`smithy_query(query="shape",shape="StackStatus")`): the failed rollback is the state
//! an operator has to clean up from, so a rollback whose every removal went through must not
//! report it.
//!
//! That is the rule a provider FRAMEWORK is written against: a wrapper whose inner handler
//! raised on the create answers `FAILED` under an id standing for a resource it never made,
//! and answers the `Delete` that id then arrives on without calling that handler again — so
//! the rollback is carried through. A wrapper that has its handler raise on the `Delete` too
//! is the complement: that rollback really could not be carried out.
//!
//! Every row drives the emulator through its own wire: the stack operations over
//! CloudFormation's awsQuery endpoint, the provider created in AWS Lambda by the stack
//! itself, the answers sent by that function's OWN code — nothing here writes to the
//! `ResponseURL` — and the resources the create really made read back through the service
//! that owns them (AWS Systems Manager).
//!
//! The rows differ along what the rule generalises over: the RUNTIME the provider framework
//! is written for (a Node release and a Python one), how the custom resource's type is
//! spelled (the author's own `Custom::<Name>` namespace and the generic
//! `AWS::CloudFormation::CustomResource`), whether the `FAILED` answer states a reason of its
//! own or none, how much the create had really made beside the custom resource for the
//! rollback to remove (nothing, one resource, two), and whether the provider carries the
//! rollback's own `Delete` out.

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::provisioning::custom_resource;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, failed_resource_reason, member, router_with_engine, settled_stack_status,
    xml_blocks, xml_text, REGION,
};
use crate::stack_lifecycle::resource_event_statuses;
use crate::update_rollback_recovery::{parameter, parameter_value, reason_of, template};

/// The logical ids every row's template declares its three fixed resources under: the
/// provider's execution role, the provider function itself, and the custom resource that
/// function backs.
const ROLE: &str = "ProviderRole";
const PROVIDER: &str = "Provider";
const WAITER: &str = "Waiter";

/// The `Handler` every provider here is declared with, which names the one source file of
/// its deployment package.
const HANDLER: &str = "index.handler";

/// The id a provider framework answers a `Create` its own handler raised on under: an
/// identifier standing for a resource that handler never made, which is what the `Delete` the
/// rollback then sends is addressed to.
///
/// Spelled by the PROVIDER's own code below, never by this emulator: it is the answer's
/// `PhysicalResourceId`, and what this suite asserts is that the rollback addresses the
/// resource by the id the answer named.
const CREATE_FAILED_ID: &str = "CustomResourceProviderFramework::CREATE_FAILED";

/// A provider framework written for a NODE runtime — the runtime a synthesised provider is
/// declared with: it calls the author's own handler, answers `FAILED` under
/// [`CREATE_FAILED_ID`] when that handler raises on a `Create`, and answers the `Delete` of
/// that id without calling the handler again.
///
/// What it does with each request is driven by the custom resource's OWN properties, so one
/// provider serves every row: the message its handler raises with, whether the `FAILED`
/// answer states that message as its `Reason`, and whether the framework knows the `Delete`
/// of a create that failed is nothing to do.
const NODE_FRAMEWORK: &str = r#"
'use strict';

const https = require('https');
const url = require('url');

const CREATE_FAILED = 'CustomResourceProviderFramework::CREATE_FAILED';

function submit(event, status, physicalResourceId, reason) {
    const answer = {
        Status: status,
        PhysicalResourceId: physicalResourceId,
        StackId: event.StackId,
        RequestId: event.RequestId,
        LogicalResourceId: event.LogicalResourceId,
        Data: {},
    };
    if (reason) {
        answer.Reason = reason;
    }
    const document = JSON.stringify(answer);
    const parsed = url.parse(event.ResponseURL);
    return new Promise((resolve, reject) => {
        const answering = https.request({
            hostname: parsed.hostname,
            port: parsed.port,
            path: parsed.path,
            method: 'PUT',
            headers: {
                'content-type': '',
                'content-length': Buffer.byteLength(document, 'utf8'),
            },
        }, () => resolve({}));
        answering.on('error', reject);
        answering.write(document);
        answering.end();
    });
}

const onEvent = async (event) => {
    throw new Error((event.ResourceProperties || {}).FailWith);
};

exports.handler = async (event) => {
    const properties = event.ResourceProperties || {};
    if (event.RequestType === 'Delete'
        && event.PhysicalResourceId === CREATE_FAILED
        && properties.IgnoresTheDeleteOfAFailedCreate === 'true') {
        return submit(event, 'SUCCESS', CREATE_FAILED);
    }
    try {
        await onEvent(event);
        return submit(event, 'SUCCESS', event.RequestId);
    } catch (error) {
        const id = event.RequestType === 'Create' ? CREATE_FAILED : event.PhysicalResourceId;
        const reason = properties.ReportsTheReason === 'true' ? error.message : undefined;
        return submit(event, 'FAILED', id, reason);
    }
};
"#;

/// The same framework written for a PYTHON runtime, answering with that runtime's own HTTP
/// client: what fails a resource is the provider's answer, whatever language the provider is
/// written in.
const PYTHON_FRAMEWORK: &str = r#"
import json
import urllib.request

CREATE_FAILED = "CustomResourceProviderFramework::CREATE_FAILED"


def submit(event, status, physical_resource_id, reason=None):
    answer = {
        "Status": status,
        "PhysicalResourceId": physical_resource_id,
        "StackId": event["StackId"],
        "RequestId": event["RequestId"],
        "LogicalResourceId": event["LogicalResourceId"],
        "Data": {},
    }
    if reason:
        answer["Reason"] = reason
    request = urllib.request.Request(
        event["ResponseURL"],
        data=json.dumps(answer).encode("utf-8"),
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}


def on_event(event):
    properties = event.get("ResourceProperties") or {}
    raise RuntimeError(properties.get("FailWith", ""))


def handler(event, context):
    properties = event.get("ResourceProperties") or {}
    ignores = properties.get("IgnoresTheDeleteOfAFailedCreate") == "true"
    if (
        event["RequestType"] == "Delete"
        and event.get("PhysicalResourceId") == CREATE_FAILED
        and ignores
    ):
        return submit(event, "SUCCESS", CREATE_FAILED)
    try:
        on_event(event)
    except Exception as error:
        if event["RequestType"] == "Create":
            physical = CREATE_FAILED
        else:
            physical = event.get("PhysicalResourceId")
        reason = None
        if properties.get("ReportsTheReason") == "true":
            reason = str(error)
        return submit(event, "FAILED", physical, reason)
    return submit(event, "SUCCESS", event["RequestId"])
"#;

/// One row: the stack a failing create leaves behind, the provider backing its custom
/// resource, and where the rollback of that create has to end.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The custom resource's `Type` — the author's own namespace or the generic spelling.
    resource_type: &'static str,
    /// The provider framework's source, and the runtime it is declared for.
    source: &'static str,
    runtime: &'static str,
    /// The message the provider's own handler raises with.
    fail_with: &'static str,
    /// Whether the `FAILED` answer states that message as its `Reason`.
    reports_the_reason: bool,
    /// Whether the framework answers the `Delete` of a create that failed without calling its
    /// handler again, which is what carries the rollback through.
    ignores_the_delete: bool,
    /// The Parameter Store names the create really made beside the custom resource, which the
    /// rollback has to delete: what proves the rollback RAN rather than merely reporting a
    /// status.
    siblings: &'static [&'static str],
    /// Where the rollback ends.
    settles: &'static str,
    /// The event the rollback's own `Delete` of the custom resource records where it carried
    /// that removal out, and `None` for the row whose provider refused it — the rollback that
    /// could not be carried out.
    removal: Option<&'static str>,
}

/// The class the rule covers: any create a provider answered `FAILED`, however the stack
/// declares it, whatever else the create had made, and whichever way the provider then treats
/// the rollback's own `Delete`.
const ROWS: &[Row] = &[
    Row {
        case: "a node framework whose handler raised, with one resource beside it",
        stack: "raised-create-node",
        resource_type: "Custom::Waiter",
        source: NODE_FRAMEWORK,
        runtime: "nodejs20.x",
        fail_with: "the waiter could not be created",
        reports_the_reason: true,
        ignores_the_delete: true,
        siblings: &["/raised-create-node/sibling"],
        settles: status::ROLLBACK_COMPLETE,
        removal: Some(resource_status::DELETE_COMPLETE),
    },
    Row {
        case: "a python framework under the generic type, stating no reason, with two \
               resources beside it",
        stack: "raised-create-python",
        resource_type: "AWS::CloudFormation::CustomResource",
        source: PYTHON_FRAMEWORK,
        runtime: "python3.12",
        fail_with: "the probe could not be created",
        reports_the_reason: false,
        ignores_the_delete: true,
        siblings: &[
            "/raised-create-python/first",
            "/raised-create-python/second",
        ],
        settles: status::ROLLBACK_COMPLETE,
        removal: Some(resource_status::DELETE_COMPLETE),
    },
    // The complement, and what makes the rows above about a rollback that was CARRIED OUT: a
    // framework that has its handler raise on the rollback's own `Delete` as well leaves a
    // resource nothing removed, so that rollback could not be carried out and the stack rests
    // in the failed rollback status.
    Row {
        case: "a framework that raises on the rollback's own Delete as well",
        stack: "raised-delete-too",
        resource_type: "Custom::StubbornWaiter",
        source: NODE_FRAMEWORK,
        runtime: "nodejs18.x",
        fail_with: "the waiter is broken either way",
        reports_the_reason: true,
        ignores_the_delete: false,
        siblings: &[],
        settles: status::ROLLBACK_FAILED,
        removal: None,
    },
];

/// The logical ids one row's siblings are declared under.
fn sibling_ids(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("Sibling{index}")).collect()
}

/// The template one row deploys: the provider's execution role, the provider function with
/// that row's own framework as its inline code, the Parameter Store resources the create
/// really makes, and the custom resource that provider backs — declared LAST, by depending on
/// every sibling, which is the shape a stack that deploys its own provider has.
fn template_of(row: &Row) -> String {
    let mut resources = Map::new();
    resources.insert(
        ROLE.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }],
                },
            },
        }),
    );
    resources.insert(
        PROVIDER.to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Code": {"ZipFile": row.source},
                "Handler": HANDLER,
                "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                "Runtime": row.runtime,
            },
        }),
    );
    let ids = sibling_ids(row.siblings.len());
    for (logical_id, name) in ids.iter().zip(row.siblings) {
        resources.insert(logical_id.clone(), parameter(name, "made-by-the-create"));
    }
    let mut declaration = json!({
        "Type": row.resource_type,
        "Properties": {
            "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
            "FailWith": row.fail_with,
            "ReportsTheReason": row.reports_the_reason.to_string(),
            "IgnoresTheDeleteOfAFailedCreate": row.ignores_the_delete.to_string(),
        },
    });
    if !ids.is_empty() {
        declaration["DependsOn"] = json!(ids);
    }
    resources.insert(WAITER.to_string(), declaration);
    template(Value::Object(resources))
}

/// What the stack's event stream reports as the reason the custom resource's create failed:
/// polled, because the failure is recorded at the instant the deployment reaches it.
async fn create_failure_reason(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    failed_resource_reason(
        case,
        router,
        REGION,
        stack,
        WAITER,
        resource_status::CREATE_FAILED,
    )
    .await
}

/// The part of the sentence CloudFormation reports a provider's `FAILED` answer with that
/// does NOT depend on which request was answered — the common head of the service's own
/// wording of it for two different request ids.
///
/// Read from the service's own wording rather than spelled again here, and without the
/// `RequestId` it names, which a client reads off the sentence and no suite can predict.
fn fixed_text_of_the_answered_failure(message: &str) -> String {
    let one = custom_resource::answered_failure_reason(message, "one-request");
    let other = custom_resource::answered_failure_reason(message, "another-request");
    one.chars()
        .zip(other.chars())
        .take_while(|(left, right)| left == right)
        .map(|(left, _)| left)
        .collect()
}

/// Every `PhysicalResourceId` the stack's event stream reports for one of its resources —
/// which is what says WHICH physical resource CloudFormation addressed about it.
async fn reported_physical_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<String> {
    let (code, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackEvents to answer, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|entry| xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id))
        .filter_map(|entry| xml_text(entry, "PhysicalResourceId"))
        .collect()
}

#[tokio::test]
async fn a_create_the_provider_answered_failed_is_rolled_back_by_a_delete_of_the_id_it_named() {
    for row in ROWS {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let (code, created) = create_stack(
            &router,
            row.stack,
            &template_of(row),
            &[member("Capabilities.member.1", "CAPABILITY_IAM")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} {created}"
        );

        // The provider's answer is what fails the resource, and the reason the stack reports
        // for it is the response status CloudFormation received with the message that answer
        // carried — the provider's own, or the service's sentence for an answer that stated
        // none.
        let reported = create_failure_reason(&router, case, row.stack).await;
        let message = match row.reports_the_reason {
            true => row.fail_with,
            false => custom_resource::UNSTATED_REASON,
        };
        let fixed = fixed_text_of_the_answered_failure(message);
        assert!(
            reported.starts_with(&fixed),
            "case {case:?}: expected the failed create to report {fixed:?}, got {reported:?}"
        );

        // ...and the rollback that failure owes ends where the removals it owes ended: the
        // complete status only where every one of them was carried out.
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, row.settles,
            "case {case:?}: expected the rollback of the failed create to settle {:?}, got \
             {settled:?} with the reason {:?}",
            row.settles,
            reason_of(&router, row.stack).await
        );

        // The custom resource's own event stream says its create FAILED — the answer its
        // provider sent, not a completion nothing carried out — and, where the rollback's own
        // `Delete` of it was carried out, that removal.
        let statuses = resource_event_statuses(&router, case, row.stack, WAITER).await;
        for expected in [Some(resource_status::CREATE_FAILED), row.removal]
            .into_iter()
            .flatten()
        {
            assert!(
                statuses.iter().any(|status| status == expected),
                "case {case:?}: expected {WAITER} to report {expected:?}, got {statuses:?}"
            );
        }

        // ...and the resource the rollback addressed is the one the provider's answer NAMED:
        // a provider that could not carry the create out still says which physical resource it
        // is responsible for, and that is the id the `Delete` carries — which is exactly what
        // lets a framework recognise the delete of a create that failed.
        let addressed = reported_physical_ids(&router, case, row.stack, WAITER).await;
        assert!(
            addressed.iter().any(|id| id == CREATE_FAILED_ID),
            "case {case:?}: expected the rollback to address {WAITER} by the id its provider \
             answered with ({CREATE_FAILED_ID:?}), got {addressed:?}"
        );

        // ...and everything the create REALLY made beside it is gone from the service that
        // owns it: a rollback reporting itself complete while leaving one behind would leave a
        // live, name-claiming resource.
        if row.settles == status::ROLLBACK_COMPLETE {
            for sibling in row.siblings {
                let held = parameter_value(&router, sibling).await;
                assert_eq!(
                    held, None,
                    "case {case:?}: expected the rollback to delete the sibling {sibling:?} the \
                     create had made, got {held:?}"
                );
            }
        }

        // The failed rollback names WHICH resource it could not delete, which is what an
        // operator cleaning up after it reads.
        if row.settles == status::ROLLBACK_FAILED {
            let summary = reason_of(&router, row.stack).await;
            assert!(
                summary.contains(WAITER),
                "case {case:?}: expected the failed rollback to name {WAITER} as the resource \
                 it could not delete, got {summary:?}"
            );
        }
    }
}
