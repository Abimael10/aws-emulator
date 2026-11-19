// SPDX-License-Identifier: CC0-1.0

//! The IAM decision this service's own front door takes, over the awsQuery wire: which
//! documents decide a call, what a caller they do not cover is told, and that a refused call
//! leaves the account exactly as it was.
//!
//! RULE: a signed request to CloudFormation is decided by IAM BEFORE the operation reads or
//! writes anything, as the action `cloudformation:<the operation's own name>` on the resource
//! the request names — the stack's own ARN when it names a stack, and every resource of this
//! service in the account and the Region the request was signed for when it names none. A call
//! no readable document allows, and one a readable document denies, is refused `AccessDenied`
//! at HTTP 403 with the sentence IAM words an authorization failure in, and performs nothing;
//! a call a document in force allows is served.
//!
//! The rows differ along what the rule generalises over: WHICH action the document in force
//! names (the one the call performs, another action of the same service, every action of it),
//! WHICH resource it names (every resource, the stack the call is about, another stack), and
//! which EFFECT it states (an allow, and an allow with an explicit deny beside it). Each row
//! is asked about three calls whose action and resource differ — a read of ONE stack, a listing
//! that names no stack, and a CREATE whose stack does not exist yet — so a grant of one action
//! never covers another, a grant on one stack never covers another, and the resource a call
//! about no stack is judged on is pinned too. The write half asserts the other side of the
//! rule as well: after a refused create, the account holds no such stack.
//!
//! The decision itself, the principal it is taken for and the sentence it is worded with are
//! `src/service/identity_authorization.rs`'s; what this suite drives is the whole wire path —
//! IAM's own calls to hold the user and its policy, then the signed CloudFormation call.

use serde_json::json;

use crate::harness::{call, call_as, member, refusal, router, xml_text, REGION};
use crate::stack_lifecycle::{user_with_inline_policy, ACCOUNT_ID};
use smithy_server_runtime::MultiServiceRouter;

/// The wire error code this service publishes for an unauthorized call, and the status it
/// answers one with.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The stack every row reads, and the template it is created from: what the template declares
/// is irrelevant to the rule — what each row decides is whether the CALLER may make the call.
const HELD_STACK: &str = "cfnauth-held";

/// The one inline policy name each row's user carries its document under.
const INLINE_POLICY: &str = "grants";

fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Notices": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": "cfnauth-notices"},
            },
        },
    })
    .to_string()
}

/// One identity policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": action, "Resource": resource}],
    })
    .to_string()
}

/// One identity policy allowing EVERYTHING and denying `action` on every resource: the
/// explicit deny no grant lifts, which a single-statement document cannot express.
fn allow_everything_but(action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": "*", "Resource": "*"},
            {"Effect": "Deny", "Action": action, "Resource": "*"},
        ],
    })
    .to_string()
}

/// The ARN of the IAM user a row calls as, which is what a refusal names it by.
fn user_arn(user: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:user/{user}")
}

/// The IAM action a call of `operation` performs: this service publishes one action per API
/// operation, named by that operation.
fn action_for(operation: &str) -> String {
    format!("cloudformation:{operation}")
}

/// The sentence IAM refuses an unauthorized call with: who was refused, which action, on which
/// resource, and which of IAM's rules refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The clause closing a refusal no readable document allowed — IAM's default deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The clause closing a refusal a readable document DENIED.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// Every resource of this service in the account and the Region a call that names no stack is
/// judged against.
fn every_resource() -> String {
    format!("arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:*")
}

/// The ARN form a stack of `name` the account does not hold is judged against: the id a create
/// would mint, wildcarded, because the decision precedes the lookup.
fn absent_stack(name: &str) -> String {
    format!("arn:aws:cloudformation:{REGION}:{ACCOUNT_ID}:stack/{name}/*")
}

/// One call a row makes: the operation, the members the request carries, whether this row's
/// document serves it, and the resource it is judged on.
type Call = (&'static str, Vec<(String, String)>, bool, String);

/// One row: the document in force on the caller, and whether each of the three calls is served.
struct Row {
    case: &'static str,
    user: &'static str,
    /// The document, built from the ARN of the stack the read is about.
    document: fn(&str) -> String,
    /// Whether the read of that one stack is served.
    read_served: bool,
    /// Whether the listing that names no stack is served.
    listing_served: bool,
    /// Whether the create of a stack that does not exist yet is served.
    create_served: bool,
    /// Whether a refusal of this row is IAM's explicit deny rather than its default deny.
    denied: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a caller holding no grant on this service at all",
        user: "cfnauth-nogrant",
        document: |_stack| allow("s3:GetObject", "*"),
        read_served: false,
        listing_served: false,
        create_served: false,
        denied: false,
    },
    Row {
        case: "a grant of every action of the service on every resource",
        user: "cfnauth-wildcard",
        document: |_stack| allow("cloudformation:*", "*"),
        read_served: true,
        listing_served: true,
        create_served: true,
        denied: false,
    },
    Row {
        case: "a grant of the stack read alone, on that stack's own ARN",
        user: "cfnauth-oneaction",
        document: |stack| allow("cloudformation:DescribeStacks", stack),
        read_served: true,
        listing_served: false,
        create_served: false,
        denied: false,
    },
    Row {
        case: "a grant of that same action, on another stack's ARN",
        user: "cfnauth-otherstack",
        document: |stack| {
            allow(
                "cloudformation:DescribeStacks",
                &stack.replace("stack/", "stack/elsewhere-"),
            )
        },
        read_served: false,
        listing_served: false,
        create_served: false,
        denied: false,
    },
    Row {
        case: "a grant of everything with an explicit deny of the listing",
        user: "cfnauth-denied",
        document: |_stack| allow_everything_but("cloudformation:ListStacks"),
        read_served: true,
        listing_served: false,
        create_served: true,
        denied: true,
    },
];

/// Whether the account holds a stack named `name`, read as the account's own credential — which
/// no identity policy bounds, so this reads what the account really holds whatever a row's
/// caller may do.
async fn holds_stack(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, _body) = call(router, "DescribeStacks", &[member("StackName", name)]).await;
    status == 200
}

/// RULE: every call is decided by the documents in force on the calling principal, as this
/// service's action for that operation on the resource the request names; a refused call is
/// answered `AccessDenied` at 403 with IAM's own sentence and performs nothing.
#[tokio::test]
async fn a_call_is_decided_by_the_documents_in_force_on_the_caller() {
    let router = router();
    let (status, body) = call(
        &router,
        "CreateStack",
        &[
            member("StackName", HELD_STACK),
            member("TemplateBody", &template()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the stack every row reads must be created, got {status} {body}"
    );
    let stack_arn = xml_text(&body, "StackId")
        .unwrap_or_else(|| panic!("setup: expected the stack's own id, got {body}"));

    for row in ROWS {
        let access_key_id = user_with_inline_policy(
            &router,
            row.case,
            row.user,
            INLINE_POLICY,
            &(row.document)(&stack_arn),
        )
        .await;
        // The stack this row's create names: its own, so a served create cannot be mistaken
        // for another row's.
        let created = format!("{}-made", row.user);

        // (the operation, its members, whether it is served, the resource it is judged on)
        let calls: [Call; 3] = [
            (
                "DescribeStacks",
                vec![member("StackName", HELD_STACK)],
                row.read_served,
                stack_arn.clone(),
            ),
            ("ListStacks", Vec::new(), row.listing_served, every_resource()),
            (
                "CreateStack",
                vec![
                    member("StackName", &created),
                    member("TemplateBody", &template()),
                ],
                row.create_served,
                absent_stack(&created),
            ),
        ];
        for (operation, members, served, resource) in calls {
            let case = format!("{} / {operation}", row.case);
            let (status, body) = call_as(&router, &access_key_id, operation, &members).await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the granted call to be served, got {status} {body}"
                );
                continue;
            }
            let action = action_for(operation);
            let reason = if row.denied {
                EXPLICIT_DENY.to_string()
            } else {
                no_grant(&action)
            };
            assert_eq!(
                status, ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} {body}"
            );
            assert_eq!(
                refusal(&body),
                (
                    ACCESS_DENIED.to_string(),
                    not_authorized(&user_arn(row.user), &action, &resource, &reason),
                ),
                "case {case:?}: expected {ACCESS_DENIED} and IAM's sentence naming {action} on \
                 {resource}, got {body}"
            );
            assert!(
                is_a_request_id_in(&body),
                "case {case:?}: expected the refusal to carry a request id, got {body}"
            );
        }

        // The other half of the write row: the account holds the stack a served create made,
        // and nothing a refused one asked for.
        let held = holds_stack(&router, &created).await;
        assert_eq!(
            held, row.create_served,
            "case {:?}: expected the account to hold {created:?} = {}, got {held}",
            row.case, row.create_served
        );
    }

    // The account's own published credential holds no IAM policies — the position AWS account
    // root credentials are in — so it is not bounded by this gate: the positive control that
    // the service itself serves the very calls every refused row made.
    let (status, body) = call(&router, "ListStacks", &[]).await;
    assert_eq!(
        status, 200,
        "case \"the account's own published credential\": expected the listing to be served, \
         got {status} {body}"
    );
}

/// Whether a refusal envelope carries the `<RequestId>` every answer of this endpoint reports,
/// which an authorization refusal is answered through the same envelope as every other error.
fn is_a_request_id_in(body: &str) -> bool {
    xml_text(body, "RequestId").is_some_and(|id| crate::harness::is_a_request_id(&id))
}
