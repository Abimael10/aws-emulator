// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::Amplify::Branch` whose create AWS Amplify answers as NOT-FOUND is reported
//! with — the `ResourceStatusReason` of the failed resource, which is what a client reading a
//! rolled-back deployment classifies the failure by.
//!
//! RULE under test: **a resource provider of an AWS Amplify type reports a call that service
//! refused as not-found in the resource-provider plugin's own uniform sentence — `Resource of
//! type '<the CloudFormation type the provider manages>' with identifier '<the identifier the
//! handler holds>' was not found.` — rather than forwarding the owning service's own wording,
//! which names the app or the branch in Amplify's own terms and no CloudFormation type at all.
//! The identifier is the ARN that service assigns, so a CREATE, which has not been answered one
//! yet, holds none and the sentence quotes the null reference.**
//!
//! Why it matters: a client — or anything reading `ResourceStatusReason` to diagnose a
//! rollback — matches the uniform sentence to conclude "a resource this template named is
//! missing" without knowing which service answered. A reason carrying the service's own
//! sentence instead never matches, and falls through to a generic failure path even though the
//! status and the `HandlerErrorCode` are right.
//!
//! The rows differ along the app id the declaration states — its LENGTH (the shortest value
//! `CreateBranch`'s own `appId` member admits and the longest, `@length 1..20` with pattern
//! `^d[a-z0-9]+$`, read with
//! `smithy_query(query="operation",operation="CreateBranch",model_path="services/service-com-amazonaws-amplify-amplify/smithy-model/com-amazonaws-amplify-amplify.json")`),
//! its CHARACTER SET (digits only after the initial, letters only, and both mixed) — and along
//! whether the stack declares an app of its own beside the branch. None of them varies the
//! sentence: that is the point of a uniform wording.
//!
//! Two more cases pin the rest of the rule: the POSITIVE CONTROL, a branch under an app the same
//! stack creates, which completes and is reported no failure at all; and the UPDATE of a branch
//! that service no longer holds, whose handler DOES hold an identifier — the ARN the stack
//! published — and quotes it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::amplify_branch_provisioning::{amplify, create_app, get_branch, list_branch_names};
use crate::harness::{
    call, failed_resource_reason, member, quoted_and_class, resource_event_reason_in, router,
    settled_stack_status, stack_event_reasons, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The resource types the templates here declare.
const BRANCH: &str = "AWS::Amplify::Branch";
const APP: &str = "AWS::Amplify::App";

/// The logical ids they declare them under.
const BRANCH_ID: &str = "Branch";
const APP_ID: &str = "App";

/// The `HandlerErrorCode` class a refusal that a resource was NOT FOUND falls in, a value of the
/// model's own `HandlerErrorCode` enum (`smithy_query(query="shape",shape="HandlerErrorCode")`).
const NOT_FOUND: &str = "NotFound";

/// The identifier the plugin's sentence quotes for a handler that holds NONE — a create, which
/// is answered the ARN it would hold by the very call being refused.
const NO_IDENTIFIER: &str = "null";

/// The plugin's own not-found sentence, whose fixed text is `Resource of type '`, `' with
/// identifier '` and `' was not found.`, and whose two variable parts are the CloudFormation type
/// of the resource the handler was asked for and the identifier it holds for it.
fn plugin_not_found(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' was not found.")
}

/// One row: the stack it deploys, the app id the branch's declaration states — a value of the
/// form that member fixes that no app of this account was created under — and whether the
/// template declares an app of its own beside the branch.
struct Row {
    case: &'static str,
    stack: &'static str,
    app_id: &'static str,
    declares_an_app: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the shortest app id that member admits",
            stack: "amplify-unknown-app-shortest",
            app_id: "d0",
            declares_an_app: false,
        },
        Row {
            case: "the longest app id that member admits",
            stack: "amplify-unknown-app-longest",
            app_id: "d123456789abcdefghi",
            declares_an_app: false,
        },
        Row {
            case: "an app id of digits after the initial",
            stack: "amplify-unknown-app-digits",
            app_id: "d00000000000000",
            declares_an_app: false,
        },
        Row {
            case: "an app id of letters after the initial",
            stack: "amplify-unknown-app-letters",
            app_id: "dnosuchappanywhere",
            declares_an_app: false,
        },
        Row {
            case: "an unknown app id beside an app the stack creates itself",
            stack: "amplify-unknown-app-with-own-app",
            app_id: "d0e8e2c3e2eef6",
            declares_an_app: true,
        },
    ]
}

/// The template of one row: the branch declared under the app id the row states — which nothing
/// creates — beside, for the row that asks for it, an app of the stack's own that the branch does
/// NOT belong to.
fn template_of(row: &Row) -> String {
    let mut resources = json!({
        BRANCH_ID: {
            "Type": BRANCH,
            "Properties": {"AppId": row.app_id, "BranchName": "main"}
        }
    });
    if row.declares_an_app {
        resources[APP_ID] = json!({
            "Type": APP,
            "Properties": {"Name": row.stack}
        });
    }
    json!({
        "Resources": resources,
        "Outputs": {"BranchRef": {"Value": {"Ref": BRANCH_ID}}}
    })
    .to_string()
}

/// Submit one template for one stack and wait for the stack to settle; answers the `StackStatus`
/// it settled on.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    template: &str,
) -> String {
    let (code, body) = call(
        router,
        action,
        &[
            member("StackName", stack),
            member("TemplateBody", template),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {code} with body {body}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

#[tokio::test]
async fn an_unknown_app_is_reported_in_the_plugins_own_not_found_sentence() {
    for row in rows() {
        let router = router();
        let reached = deployed(
            &router,
            row.case,
            "CreateStack",
            row.stack,
            &template_of(&row),
        )
        .await;
        assert_eq!(
            reached,
            status::ROLLBACK_COMPLETE,
            "case {:?}: expected the refused create to roll the stack back, got {reached} with \
             failure reasons {:?}",
            row.case,
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            BRANCH_ID,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        let expected = plugin_not_found(BRANCH, NO_IDENTIFIER);
        assert_eq!(
            quoted, expected,
            "case {:?}: expected the quoted message to be {expected:?}, got {quoted:?} in \
             {reason:?}",
            row.case
        );
        assert_eq!(
            class, NOT_FOUND,
            "case {:?}: expected the {NOT_FOUND} class, got {class:?} in {reason:?}",
            row.case
        );
        // The sentence is the plugin's, so the owning service's own wording — which names the app
        // the request stated — reaches no reason the stack records.
        let forwarded = format!("App {} not found.", row.app_id);
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert!(
            !reasons.iter().any(|recorded| recorded.contains(&forwarded)),
            "case {:?}: expected no reason to carry the owning service's own sentence \
             {forwarded:?}, got {reasons:?}",
            row.case
        );
        // Nothing was created for the branch either: the app id it named holds no branch.
        let listed = list_branch_names(&router, row.app_id).await;
        assert!(
            listed.is_empty(),
            "case {:?}: expected no branch under an app that does not exist, got {listed:?}",
            row.case
        );
    }
}

/// The control: a branch under an app the same stack creates completes, and the stack records no
/// failure for it at all — so the sentence above is what a REFUSED create is reported with rather
/// than what every deployment of this type produces.
#[tokio::test]
async fn a_branch_under_the_stacks_own_app_is_reported_no_failure() {
    let case = "a branch under the app the same stack creates";
    let stack = "amplify-unknown-app-control";
    let router = router();
    let template = json!({
        "Resources": {
            APP_ID: {"Type": APP, "Properties": {"Name": stack}},
            BRANCH_ID: {
                "Type": BRANCH,
                "Properties": {
                    "AppId": {"Fn::GetAtt": [APP_ID, "AppId"]},
                    "BranchName": "main"
                }
            }
        },
        "Outputs": {"AppIdAtt": {"Value": {"Fn::GetAtt": [APP_ID, "AppId"]}}}
    })
    .to_string();

    let reached = deployed(&router, case, "CreateStack", stack, &template).await;
    assert_eq!(
        reached,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let failed = resource_event_reason_in(
        &router,
        REGION,
        stack,
        BRANCH_ID,
        resource_status::CREATE_FAILED,
    )
    .await;
    assert!(
        failed.is_empty(),
        "case {case:?}: expected no failed create for the branch, got {failed:?}"
    );
}

/// The other half of the rule: the handler of an UPDATE holds an identifier for the branch — the
/// ARN AWS Amplify assigned, which the stack published — so the sentence quotes that ARN rather
/// than the null reference.
#[tokio::test]
async fn an_update_of_a_branch_that_is_gone_quotes_the_arn_the_handler_holds() {
    let case = "a branch deleted out of band and then re-declared";
    let stack = "amplify-branch-gone-update";
    let router = router();
    let app_id = create_app(&router, "branch-gone-update").await;
    let declaration = |description: &str| {
        json!({
            "Resources": {
                BRANCH_ID: {
                    "Type": BRANCH,
                    "Properties": {
                        "AppId": app_id,
                        "BranchName": "main",
                        "Description": description
                    }
                }
            },
            "Outputs": {"BranchRef": {"Value": {"Ref": BRANCH_ID}}}
        })
        .to_string()
    };

    let reached = deployed(
        &router,
        case,
        "CreateStack",
        stack,
        &declaration("the first description"),
    )
    .await;
    assert_eq!(
        reached,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let (status_code, reported) = get_branch(&router, &app_id, "main").await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the branch to exist before the update, got {status_code} with \
         {reported}"
    );
    let branch_arn = reported
        .get("branchArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no branchArn in {reported}"))
        .to_string();

    // A client deletes the branch through that service's own API, so the stack's record names a
    // branch AWS Amplify no longer holds.
    let (status_code, answer) = amplify(
        &router,
        "DELETE",
        &format!("/apps/{app_id}/branches/main"),
        None,
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected DeleteBranch to answer 200, got {status_code} with {answer}"
    );

    let reached = deployed(
        &router,
        case,
        "UpdateStack",
        stack,
        &declaration("the second description"),
    )
    .await;
    assert!(
        reached.starts_with("UPDATE_ROLLBACK") || reached == status::UPDATE_FAILED,
        "case {case:?}: expected the refused update to fail the stack, got {reached} with failure \
         reasons {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );

    let reason = failed_resource_reason(
        case,
        &router,
        REGION,
        stack,
        BRANCH_ID,
        resource_status::UPDATE_FAILED,
    )
    .await;
    let (quoted, class) = quoted_and_class(case, &reason);
    let expected = plugin_not_found(BRANCH, &branch_arn);
    assert_eq!(
        quoted, expected,
        "case {case:?}: expected the quoted message to be {expected:?}, got {quoted:?} in \
         {reason:?}"
    );
    assert_eq!(
        class, NOT_FOUND,
        "case {case:?}: expected the {NOT_FOUND} class, got {class:?} in {reason:?}"
    );
}
