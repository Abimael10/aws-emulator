// SPDX-License-Identifier: CC0-1.0

//! What a stack REPORTS about a custom resource once its provider has answered: the physical
//! id on the resource's own events, and the window a replacement's cleanup holds the update
//! in.
//!
//! The rule under test: **the `PhysicalResourceId` a provider's response document names is the
//! identity CloudFormation adopts for that resource whatever the response's `Status`, so the
//! resource's own stack events report that id and every later request about it carries it; a
//! response CloudFormation does not read names nothing, so the resource stays known by the id
//! CloudFormation itself assigned (`<stack name>-<logical id>-<unique segment>`); and while the
//! `Delete` a replacement owes for the resource it replaced is outstanding the stack reports
//! `UPDATE_COMPLETE_CLEANUP_IN_PROGRESS`, reaching `UPDATE_COMPLETE` — with `Ref` and
//! `Fn::GetAtt` resolving against the answer that replaced it — only once that `Delete` is
//! answered.**
//!
//! Three crates have to agree for that to hold — CloudFormation publishes the request and
//! holds the resource until the answer lands, Amazon SNS delivers the publish to the
//! subscribed endpoint, and Amazon SQS holds it for the provider to read — so the suite lives
//! here, beside [`super::cloudformation_sns_custom_resource_protocol`], whose provider harness
//! it drives: that suite pins WHICH request a landed answer owes, and this one pins what the
//! stack reports about the resource while and after it is owed.
//!
//! The rows differ along what the rule generalises over: the `Status` the answer carries, the
//! form of the id it names (a plain token and an ARN), whether the document is one
//! CloudFormation reads at all, and — for the replacement — the id and the `Data` the new
//! answer carries.
//!
//! Every assertion drives the emulator through its own wires: the stack operations over
//! CloudFormation's awsQuery endpoint, the provider's requests off an Amazon SQS queue
//! subscribed to the Amazon SNS topic the template's `ServiceToken` names, and each answer as
//! the plain, unsigned HTTP `PUT` to that request's `ResponseURL`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_sns_custom_resource_protocol::{
    answer, answer_bytes, answered_success, build_router, cloudformation, create_stack,
    next_request, padded_answer, provider, settled_with_a_responsive_provider, stack_status,
    CREATE_COMPLETE, ROLLBACK_COMPLETE, UPDATE_COMPLETE,
};
use super::query_wire::{elements, members_of, outputs_of};

/// How long this suite waits for something the emulator owes — an event becoming readable, a
/// status being reported, a stack settling — and how often it looks while waiting. Generous:
/// what is asserted is that the report appears at all, never how fast.
const WAIT: Duration = Duration::from_secs(60);
const LOOK: Duration = Duration::from_millis(25);

/// How many times a wait of [`WAIT`] looks.
fn looks() -> usize {
    (WAIT.as_millis() / LOOK.as_millis()) as usize
}

/// The status a stack carries while the `Delete` of a resource a replacement replaced is
/// outstanding: "the stack update was successful, and CloudFormation is cleaning up the old
/// resources" (`StackStatus`).
const UPDATE_COMPLETE_CLEANUP_IN_PROGRESS: &str = "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS";

/// The statuses a resource's own events are read under.
const CREATE_FAILED: &str = "CREATE_FAILED";
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical id of the custom resource every row declares.
const THING: &str = "Thing";

/// The `PhysicalResourceId` the stack reports on `logical_id`'s `status` event, polled for: a
/// stack's events become readable as its own clock reaches them.
///
/// `None` where no such event carries one inside the window.
async fn reported_event_id(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    status: &str,
) -> Option<String> {
    for _ in 0..looks() {
        let (code, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStackEvents({stack:?}) must be answered, got {code} with {body}"
        );
        let found = members_of(&body).into_iter().find_map(|member| {
            let logical = elements(member, "LogicalResourceId").into_iter().next()?;
            let reported = elements(member, "ResourceStatus").into_iter().next()?;
            if logical != logical_id || reported != status {
                return None;
            }
            elements(member, "PhysicalResourceId")
                .into_iter()
                .next()
                .filter(|id| !id.is_empty())
        });
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(LOOK).await;
    }
    None
}

/// The outputs the stack publishes now, as the key/value map a client reads.
async fn reported_outputs(router: &MultiServiceRouter, stack: &str) -> BTreeMap<String, String> {
    let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "DescribeStacks({stack:?}) must be answered, got {code} with {body}"
    );
    outputs_of(&body)
}

/// A template with one custom resource of the provider `token`, carrying `marker` as the
/// property a revision changes, and publishing what `Ref` and `Fn::GetAtt` resolve to for it.
fn template(token: &str, marker: &str) -> String {
    json!({
        "Resources": {
            THING: {
                "Type": "Custom::Answered",
                "Properties": {
                    "ServiceToken": token,
                    "Marker": marker,
                },
            },
        },
        "Outputs": {
            "ThingRef": { "Value": { "Ref": THING } },
            "ThingVersion": { "Value": { "Fn::GetAtt": [THING, "Version"] } },
        },
    })
    .to_string()
}

/// One row of the identity table: the `Status` the provider answers the `Create` with and the
/// `PhysicalResourceId` it names.
struct Answered {
    case: &'static str,
    stack: &'static str,
    status: &'static str,
    physical_resource_id: &'static str,
    /// The event the resource ends up on, which must report that id.
    event: &'static str,
    /// The terminal status of the stack behind it.
    settles: &'static str,
}

const ANSWERS: &[Answered] = &[
    Answered {
        case: "a create answered SUCCESS under a plain id",
        stack: "cr-id-success-plain",
        status: "SUCCESS",
        physical_resource_id: "answered-instance-1",
        event: RESOURCE_CREATE_COMPLETE,
        settles: CREATE_COMPLETE,
    },
    Answered {
        case: "a create answered SUCCESS under an ARN",
        stack: "cr-id-success-arn",
        status: "SUCCESS",
        physical_resource_id: "arn:aws:service:us-east-1:000000000000:thing/answered-1",
        event: RESOURCE_CREATE_COMPLETE,
        settles: CREATE_COMPLETE,
    },
    Answered {
        case: "a create answered FAILED under a plain id",
        stack: "cr-id-failed-plain",
        status: "FAILED",
        physical_resource_id: "answered-instance-2",
        event: CREATE_FAILED,
        settles: ROLLBACK_COMPLETE,
    },
    Answered {
        case: "a create answered FAILED under an ARN",
        stack: "cr-id-failed-arn",
        status: "FAILED",
        physical_resource_id: "arn:aws:service:us-east-1:000000000000:thing/answered-2",
        event: CREATE_FAILED,
        settles: ROLLBACK_COMPLETE,
    },
];

/// Every row: the id the answer named is the id the resource's own event reports, and — for a
/// `FAILED` answer — the id the rollback's `Delete` addresses, whatever the `Status` said and
/// whatever form the provider chose for it.
#[tokio::test]
async fn the_id_an_answer_names_is_the_id_the_resources_events_report() {
    for row in ANSWERS {
        let case = row.case;
        let router = build_router();
        let mut provider = provider(&router, row.stack).await;
        let body = template(&provider.topic_arn, "v1");
        create_stack(&router, case, row.stack, &body).await;

        let created = next_request(&router, &mut provider, case, "the Create").await;
        let (code, answered) = answer(
            &router,
            &created,
            &json!({
                "Status": row.status,
                "Reason": "the row's own answer",
                "PhysicalResourceId": row.physical_resource_id,
                "Data": { "Version": "1" },
            }),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the provider's answer must be accepted, got {code} with {answered}"
        );

        // A failed create is rolled back, and the rollback addresses the physical resource the
        // provider named — not the stand-in CloudFormation had computed for the record.
        if row.status == "FAILED" {
            let deleted = next_request(&router, &mut provider, case, "the rollback's Delete").await;
            assert_eq!(
                (
                    deleted["RequestType"].as_str(),
                    deleted["PhysicalResourceId"].as_str(),
                ),
                (Some("Delete"), Some(row.physical_resource_id)),
                "case {case:?}: expected the rollback to delete the id the answer named, got \
                 {deleted}"
            );
            answered_success(&router, case, &deleted, row.physical_resource_id, &Value::Null).await;
        }

        let settled = settled_with_a_responsive_provider(
            &router,
            &mut provider,
            case,
            row.stack,
            row.settles,
        )
        .await;
        assert_eq!(
            settled, row.settles,
            "case {case:?}: expected the stack to settle at {}, got {settled:?}",
            row.settles
        );

        let reported = reported_event_id(&router, row.stack, THING, row.event).await;
        assert_eq!(
            reported.as_deref(),
            Some(row.physical_resource_id),
            "case {case:?}: expected the {} event to report the id the answer named, got \
             {reported:?}",
            row.event
        );
    }
}

/// A response document CloudFormation does NOT read names no physical resource, so the
/// resource stays known by the identifier CloudFormation itself assigned it — which is the id
/// its rollback deletes it under and the id its failure event reports.
///
/// The bound is the documented one for a response object ("the total size of the response body
/// can't exceed 4096 bytes"), so the document this row answers with is one byte past it.
#[tokio::test]
async fn an_unread_response_leaves_the_resource_under_the_id_cloudformation_assigned() {
    let case = "a response document one byte past the bound";
    let stack = "cr-id-unread";
    let router = build_router();
    let mut provider = provider(&router, stack).await;
    let body = template(&provider.topic_arn, "v1");
    create_stack(&router, case, stack, &body).await;

    let created = next_request(&router, &mut provider, case, "the Create").await;
    let named = "never-read-instance";
    let (code, answered) = answer_bytes(&router, &created, padded_answer(named, 4097)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the write is accepted whatever the document's size, got {code} with \
         {answered}"
    );

    let deleted = next_request(&router, &mut provider, case, "the rollback's Delete").await;
    assert_eq!(
        deleted["RequestType"].as_str(),
        Some("Delete"),
        "case {case:?}: the failure the unread document caused owes a Delete, got {deleted}"
    );
    let assigned = deleted["PhysicalResourceId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        assigned, named,
        "case {case:?}: a document CloudFormation did not read names nothing, so the Delete \
         may not carry the id inside it, got {deleted}"
    );
    // `<stack name>-<logical id>-<unique segment>` is the name CloudFormation generates for a
    // resource nothing else named, and the unique segment is what keeps two of them apart.
    let prefix = format!("{stack}-{THING}-");
    let unique = assigned.strip_prefix(&prefix).unwrap_or_default();
    assert!(
        !unique.is_empty() && unique.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case:?}: expected a CloudFormation-assigned id of the form {prefix:?} plus a \
         unique segment, got {assigned:?}"
    );
    answered_success(&router, case, &deleted, &assigned, &Value::Null).await;

    let settled =
        settled_with_a_responsive_provider(&router, &mut provider, case, stack, ROLLBACK_COMPLETE)
            .await;
    assert_eq!(
        settled, ROLLBACK_COMPLETE,
        "case {case:?}: a document past the bound rolls the stack back, got {settled:?}"
    );
    let reported = reported_event_id(&router, stack, THING, CREATE_FAILED).await;
    assert_eq!(
        reported.as_deref(),
        Some(assigned.as_str()),
        "case {case:?}: expected the failure event to report the id CloudFormation assigned, \
         got {reported:?}"
    );
}

/// One row of the replacement table: the id and the `Data` the `Update` is answered with.
struct Replaced {
    case: &'static str,
    stack: &'static str,
    /// The `PhysicalResourceId` the `Update` is answered with — a new one, which replaces.
    physical_resource_id: &'static str,
    /// The `Version` its `Data` carries, which the stack's own output reads.
    version: &'static str,
}

const REPLACEMENTS: &[Replaced] = &[
    Replaced {
        case: "a replacement answered under a plain id",
        stack: "cr-cleanup-plain",
        physical_resource_id: "answered-instance-2",
        version: "2",
    },
    Replaced {
        case: "a replacement answered under an ARN",
        stack: "cr-cleanup-arn",
        physical_resource_id: "arn:aws:service:us-east-1:000000000000:thing/answered-2",
        version: "7",
    },
];

/// The id the create is answered with for every row of the table above.
const FIRST_ID: &str = "answered-instance-1";

/// Poll the stack until it reports `UPDATE_COMPLETE_CLEANUP_IN_PROGRESS` — the window a
/// replacement's outstanding cleanup holds the update in — failing the row if it reports the
/// update COMPLETE while that `Delete` has not been answered.
async fn cleanup_window(router: &MultiServiceRouter, case: &str, stack: &str) {
    let mut reported = String::new();
    for _ in 0..looks() {
        reported = stack_status(router, stack).await;
        if reported == UPDATE_COMPLETE_CLEANUP_IN_PROGRESS {
            return;
        }
        assert_ne!(
            reported, UPDATE_COMPLETE,
            "case {case:?}: the update may not be reported complete while the cleanup of the \
             resource it replaced is unanswered"
        );
        tokio::time::sleep(LOOK).await;
    }
    panic!(
        "case {case:?}: expected the stack to report {UPDATE_COMPLETE_CLEANUP_IN_PROGRESS} while \
         the cleanup Delete was outstanding, last reported {reported:?}"
    );
}

/// Every row: an `Update` answered with a new `PhysicalResourceId` holds the stack at
/// `UPDATE_COMPLETE_CLEANUP_IN_PROGRESS` until the cleanup `Delete` of the resource it replaced
/// is answered, and the stack then publishes what the replacing answer said the resource is.
#[tokio::test]
async fn a_replacements_cleanup_window_precedes_the_updates_completion() {
    for row in REPLACEMENTS {
        let case = row.case;
        let router = build_router();
        let mut provider = provider(&router, row.stack).await;
        let token = provider.topic_arn.clone();
        create_stack(&router, case, row.stack, &template(&token, "v1")).await;

        let created = next_request(&router, &mut provider, case, "the Create").await;
        answered_success(
            &router,
            case,
            &created,
            FIRST_ID,
            &json!({ "Version": "1" }),
        )
        .await;
        let settled = settled_with_a_responsive_provider(
            &router,
            &mut provider,
            case,
            row.stack,
            CREATE_COMPLETE,
        )
        .await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: the created resource must complete the stack, got {settled:?}"
        );
        let published = reported_outputs(&router, row.stack).await;
        assert_eq!(
            (
                published.get("ThingRef").map(String::as_str),
                published.get("ThingVersion").map(String::as_str),
            ),
            (Some(FIRST_ID), Some("1")),
            "case {case:?}: the stack must publish what the create was answered with, got \
             {published:?}"
        );

        let (code, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(&token, "v2")),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: UpdateStack must be accepted, got {code} with {body}"
        );
        let update = next_request(&router, &mut provider, case, "the Update").await;
        assert_eq!(
            update["RequestType"].as_str(),
            Some("Update"),
            "case {case:?}: expected an Update, got {update}"
        );
        answered_success(
            &router,
            case,
            &update,
            row.physical_resource_id,
            &json!({ "Version": row.version }),
        )
        .await;

        // The cleanup of the resource the answer replaced is CloudFormation's own to publish,
        // and it holds the update open: the stack reports the cleanup window until the provider
        // answers that `Delete`.
        let cleanup = next_request(&router, &mut provider, case, "the cleanup Delete").await;
        assert_eq!(
            (
                cleanup["RequestType"].as_str(),
                cleanup["PhysicalResourceId"].as_str(),
            ),
            (Some("Delete"), Some(FIRST_ID)),
            "case {case:?}: the cleanup addresses the resource that was replaced, got {cleanup}"
        );
        cleanup_window(&router, case, row.stack).await;
        answered_success(&router, case, &cleanup, FIRST_ID, &Value::Null).await;

        let settled = settled_with_a_responsive_provider(
            &router,
            &mut provider,
            case,
            row.stack,
            UPDATE_COMPLETE,
        )
        .await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: the answered cleanup must complete the update, got {settled:?}"
        );
        let published = reported_outputs(&router, row.stack).await;
        assert_eq!(
            (
                published.get("ThingRef").map(String::as_str),
                published.get("ThingVersion").map(String::as_str),
            ),
            (Some(row.physical_resource_id), Some(row.version)),
            "case {case:?}: the stack must publish what the replacing answer said the resource \
             is, got {published:?}"
        );
    }
}
