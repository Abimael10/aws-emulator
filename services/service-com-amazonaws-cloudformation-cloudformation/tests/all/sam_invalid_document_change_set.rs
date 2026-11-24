// SPDX-License-Identifier: CC0-1.0

//! An INVALID AWS SAM document submitted to `CreateChangeSet`, and what the change set and the
//! shell stack behind it report — end to end over the awsQuery wire.
//!
//! RULE under test: **the `AWS::Serverless-2016-10-31` transform VALIDATES the document it is
//! handed, and a document it cannot process fails the change set: `Status` `FAILED` carrying the
//! macro's own aggregated diagnostic as the `StatusReason` — `Transform <name> failed with:
//! Invalid Serverless Application Specification document. Number of errors found: <n>.` followed
//! by one cause per faulty declaration, each naming the logical id and the property it is about —
//! with NO `Changes` planned, the shell stack left at `REVIEW_IN_PROGRESS`, and nothing the
//! document would have created materialised in the service that would own it. A declaration whose
//! type is in the Serverless namespace but is no type the macro defines is CloudFormation's own
//! refusal instead, in the words it refuses every unrecognised type in.**
//!
//! Why it matters: creating a change set is how a client ASKS whether a template is deployable,
//! and SAM is the ordinary authoring path for a serverless stack. A change set that reports a
//! broken document as plannable tells the author the opposite of the truth, and a diagnostic that
//! does not name the offending resource and property leaves nothing for a tool to report.
//!
//! The rows differ along what the rule generalises over: WHICH fault the document carries (a
//! `Policies` entry naming no published template, a `Zip` function with no handler to run, an
//! `Events` entry naming no event source type, a `SimpleTable` whose primary key states no type),
//! HOW MANY declarations are faulty (one and two, which is what the count is of), and whose
//! refusal it is (the macro's own and CloudFormation's for a type that does not exist).
//!
//! `src/state/sam/validation.rs` holds the same taxonomy against the validator directly; this
//! suite is about what a CLIENT reads back from the change set, the stack and the owning service.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::harness::{
    call, member, router_with_engine, xml_blocks, xml_text, xml_unescape, REGION,
};
use crate::hotswapped_function_code::lambda;
use crate::stack_lifecycle::ACCOUNT_ID;

/// The macro the AWS Serverless Application Model is published as.
const TRANSFORM: &str = "AWS::Serverless-2016-10-31";

/// The status a change set the macro failed on settles in.
const FAILED: &str = "FAILED";

/// The name every row gives its change set.
const PLAN: &str = "plan";

/// One leg: the stack the shell is created under, the document it submits, the causes the
/// diagnostic must name in order, and the function the document would have created — `None` for a
/// document that declares none.
struct Leg {
    case: &'static str,
    stack: &'static str,
    resources: Value,
    /// The causes the macro's diagnostic names, each as it words it. The count it reports is how
    /// many there are, since at most one fault is reported per declaration.
    causes: &'static [&'static str],
    /// CloudFormation's OWN refusal, for a document whose types do not exist — answered instead
    /// of the macro's diagnostic, because no macro is asked about such a document.
    unrecognised: Option<&'static str>,
    function_name: Option<&'static str>,
}

/// The `FunctionName` the rows that declare a function give it, so the absence of that function
/// is readable through AWS Lambda's own API rather than inferred.
const DECLARED_FUNCTION: &str = "sam-invalid-document-fn";

fn legs() -> Vec<Leg> {
    vec![
        Leg {
            case: "a Policies entry naming no published policy template",
            stack: "sam-invalid-policy-name",
            resources: json!({
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "FunctionName": DECLARED_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "InlineCode": "def handler(event, context): pass",
                    "Policies": [{"NotAPolicyTemplate": {"TableName": "items"}}],
                }},
            }),
            causes: &[
                "Resource with id [Fn] is invalid. Policy at index 0 in the 'Policies' property \
                 is not valid",
            ],
            unrecognised: None,
            function_name: Some(DECLARED_FUNCTION),
        },
        Leg {
            case: "a Zip function with no handler to run",
            stack: "sam-invalid-no-handler",
            resources: json!({
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "FunctionName": DECLARED_FUNCTION,
                    "Runtime": "python3.12",
                    "InlineCode": "def handler(event, context): pass",
                }},
            }),
            causes: &[
                "Resource with id [Fn] is invalid. Runtime and Handler needs to be present when \
                 PackageType is of type `Zip`",
            ],
            unrecognised: None,
            function_name: Some(DECLARED_FUNCTION),
        },
        Leg {
            case: "an Events entry naming no event source type",
            stack: "sam-invalid-event-type",
            resources: json!({
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "FunctionName": DECLARED_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "InlineCode": "def handler(event, context): pass",
                    "Events": {"Ev": {"Type": "NotAnEventType"}},
                }},
            }),
            causes: &[
                "Resource with id [Fn] is invalid. Event with id [Ev] is invalid. Resource dict \
                 has missing or invalid value for key Type. Event Type is: NotAnEventType.",
            ],
            unrecognised: None,
            function_name: Some(DECLARED_FUNCTION),
        },
        Leg {
            case: "a SimpleTable whose primary key states no type",
            stack: "sam-invalid-table-key",
            resources: json!({
                "Table": {"Type": "AWS::Serverless::SimpleTable", "Properties": {
                    "PrimaryKey": {"Name": "id"},
                }},
            }),
            causes: &["Resource with id [Table] is invalid. Property 'PrimaryKey.Type' is required."],
            unrecognised: None,
            function_name: None,
        },
        // TWO faulty declarations: the count the diagnostic reports is the number of faulty
        // DECLARATIONS, and every one of them is named.
        Leg {
            case: "two faulty declarations in one document",
            stack: "sam-invalid-two-faults",
            resources: json!({
                "Fn": {"Type": "AWS::Serverless::Function", "Properties": {
                    "FunctionName": DECLARED_FUNCTION,
                    "Runtime": "python3.12",
                    "InlineCode": "def handler(event, context): pass",
                }},
                "Table": {"Type": "AWS::Serverless::SimpleTable", "Properties": {
                    "PrimaryKey": {"Name": "id"},
                }},
            }),
            causes: &[
                "Resource with id [Fn] is invalid. Runtime and Handler needs to be present when \
                 PackageType is of type `Zip`",
                "Resource with id [Table] is invalid. Property 'PrimaryKey.Type' is required.",
            ],
            unrecognised: None,
            function_name: Some(DECLARED_FUNCTION),
        },
        // A type in the Serverless namespace that the macro does not define: no macro is asked
        // about a document whose resource types do not exist, so the refusal is CloudFormation's
        // own rather than the validator's.
        Leg {
            case: "a type in the Serverless namespace the macro does not define",
            stack: "sam-invalid-unknown-type",
            resources: json!({
                "Thing": {"Type": "AWS::Serverless::NotAThing", "Properties": {}},
            }),
            causes: &[],
            unrecognised: Some(
                "Template format error: Unrecognized resource types: [AWS::Serverless::NotAThing]",
            ),
            function_name: None,
        },
    ]
}

/// The whole `StatusReason` one leg's change set must carry: CloudFormation's own refusal where
/// the document's types do not exist, and otherwise the macro's aggregated diagnostic — the fixed
/// wrapper, the number of faults found, and the causes in order.
fn expected_reason(leg: &Leg) -> String {
    if let Some(refusal) = leg.unrecognised {
        return refusal.to_string();
    }
    format!(
        "Transform {TRANSFORM} failed with: Invalid Serverless Application Specification \
         document. Number of errors found: {}. {}",
        leg.causes.len(),
        leg.causes.join(" ")
    )
}

/// One leg's template body, with the Serverless transform declared.
fn template(leg: &Leg) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Transform": TRANSFORM,
        "Resources": leg.resources,
    })
    .to_string()
}

/// `DescribeChangeSet` for one leg's plan.
async fn describe_plan(router: &MultiServiceRouter, leg: &Leg) -> String {
    let (code, body) = call(
        router,
        "DescribeChangeSet",
        &[
            member("StackName", leg.stack),
            member("ChangeSetName", PLAN),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: DescribeChangeSet must answer 200, got {code} {body}",
        leg.case
    );
    body
}

/// Submit one leg's document as a CREATE change set on a shell stack of its own, which must be
/// ACCEPTED: a transform's failure is not a bad request, so what reports it is the change set.
async fn submit(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>, leg: &Leg) {
    let (code, body) = call(
        router,
        "CreateChangeSet",
        &[
            member("StackName", leg.stack),
            member("ChangeSetName", PLAN),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(leg)),
            member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
            member("Capabilities.member.2", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: a transform failure is no bad request, got {code} {body}",
        leg.case
    );
    settle_the_store(router, engine).await;
}

#[tokio::test]
async fn an_invalid_serverless_document_fails_the_change_set_with_the_macros_own_diagnostic() {
    for leg in legs() {
        let case = leg.case;
        let (router, engine) = router_with_engine();
        submit(&router, &engine, &leg).await;

        let described = describe_plan(&router, &leg).await;
        assert_eq!(
            xml_text(&described, "Status").as_deref(),
            Some(FAILED),
            "case {case:?}: expected the change set to fail, got {described}"
        );
        let expected = expected_reason(&leg);
        assert_eq!(
            xml_text(&described, "StatusReason")
                .map(|reason| xml_unescape(&reason))
                .as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the diagnostic {expected:?}, got {described}"
        );

        // Nothing was PLANNED: a document the transform failed on never reached the stage that
        // computes a change, so the change set carries no change at all.
        let changes = xml_blocks(&described, "Changes");
        assert!(
            changes.iter().all(|block| !block.contains("<member")),
            "case {case:?}: expected a failed change set to plan nothing, got {changes:?}"
        );
    }
}

#[tokio::test]
async fn an_invalid_serverless_document_leaves_the_shell_stack_in_review_and_creates_nothing() {
    for leg in legs() {
        let case = leg.case;
        let (router, engine) = router_with_engine();
        submit(&router, &engine, &leg).await;

        // The shell stack the CREATE change set opened is still waiting to be executed: a change
        // set that failed executes nothing, so the stack stays in the status it was opened in.
        let (code, body) = call(&router, "DescribeStacks", &[member("StackName", leg.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the shell stack to be describable, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(status::REVIEW_IN_PROGRESS),
            "case {case:?}: expected the shell stack to stay under review, got {body}"
        );

        // ...and nothing the document would have created exists: AWS Lambda holds no function at
        // the name the faulty declaration named, and says so in its own words.
        let Some(name) = leg.function_name else {
            continue;
        };
        let answered = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{name}"),
            &[],
            &[],
        )
        .await;
        assert_eq!(
            answered.status, 404,
            "case {case:?}: expected AWS Lambda to hold no function {name:?}, got {} with {}",
            answered.status, answered.body
        );
        let expected = format!("Function not found: arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{name}");
        assert_eq!(
            answered
                .body
                .get("Message")
                .or_else(|| answered.body.get("message"))
                .and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the absence to be worded {expected:?}, got {}",
            answered.body
        );
    }
}
