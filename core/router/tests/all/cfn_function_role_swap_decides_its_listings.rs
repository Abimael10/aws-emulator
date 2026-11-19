// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that deploys a Lambda function beside the execution roles it may run
//! as SETTLES — on the create and on an update that repoints the function at a DIFFERENT one of
//! those roles — and what the deployed function's own AWS SDK calls are allowed to do follows
//! the role in force on it at the instant each call is made.
//!
//! RULE: **every stack operation CloudFormation accepts reaches a TERMINAL status
//! `DescribeStacks` publishes — `CREATE_COMPLETE` or a create rollback, `UPDATE_COMPLETE` or an
//! update rollback — so a client polling it always terminates; and a deployed function's own SDK
//! call is authorized against the identity policies in force on the execution role the function
//! runs as WHEN THE CALL IS MADE, served when some statement in force allows that action and
//! refused with `AccessDeniedException` when none does — so the decision follows WHICH role is in
//! force, however the stack put it there, and the SAME call flips between refused and served the
//! moment an update hands the function another role.**
//!
//! Five crates have to agree for that to hold — CloudFormation drives the deployment and decides
//! the stack's status, IAM holds the roles and their documents, STS vends the role's session,
//! Lambda RUNS the handler with the AWS SDK its runtime supplies, and the service the handler
//! calls takes the decision — so the suite lives here rather than in any one of them. Every
//! observation is made where a client makes one: `DescribeStacks`, `DescribeStackResources`,
//! `GetFunctionConfiguration`, `Invoke`, and the called services' own listings; never an emulator
//! internal.
//!
//! The rows differ along what the rule generalises over: WHICH role the function runs as before
//! the update and WHICH it runs as after, and so which of the two services' listings the swap
//! adds and which it takes away — a grant gained, a grant lost, and one service's listing gained
//! while the other's stays refused. Every role's document is written once by the create and never
//! touched again: the only property any update changes is the function's `Role`, so a flipped
//! verdict can only come from the role in force having changed. Each row also holds the stack's
//! own records fixed across the swap — the same function name published, the same physical id for
//! every declared resource — which is what AWS's no-interruption update of that property means.
//!
//! Every expected value is computed here: a refusal is held against the AWS-common error code, a
//! served listing's count against what the OWNING service answers that same listing with, and the
//! role Lambda reports in force against the physical id the stack reports for the role the
//! template points at — so no expectation is a literal of one run.
//!
//! `cfn_role_widening_update_decides_a_functions_listings` varies the other half of the same rule
//! — it keeps one role and rewrites its document — and owns the wire helpers this suite drives;
//! `cfn_lambda_role_grant_decides_its_secret_read` pins WHERE on one role a grant may be attached.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The CloudFormation, Lambda and template helpers, and the invocation verdict reader, owned by
// the suite that first needed them rather than written again here.
use super::cfn_role_widening_update_decides_a_functions_listings::{
    build_router, cloudformation, function_resource, lambda, output, role_document, role_resource,
    seed, settled_on, verdicts, CREATE_COMPLETE, FUNCTIONS_URI, UPDATE_COMPLETE,
};
// The awsQuery XML readers this test crate writes ONCE (`tests/all/query_wire.rs`).
use super::query_wire::{outputs_of, physical_ids_of};

/// The logical id of the one function every row deploys, the suffix its physical name carries,
/// and the output key the stack publishes that name under.
const FUNCTION: &str = "Fn";
const FUNCTION_SUFFIX: &str = "fn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";

/// Every execution role the template declares, as its logical id, the suffix of its physical
/// name, and whether its inline document allows the vector-store listing and the session listing
/// the deployed handler makes. All four are declared by every template this suite deploys and
/// none of their documents ever changes, so the only thing an update below moves is which of them
/// the function runs as.
const ROLES: [(&str, &str, bool, bool); 4] = [
    ("VectorRole", "vector-role", true, false),
    ("SessionRole", "session-role", false, true),
    ("WideRole", "wide-role", true, true),
    ("BareRole", "bare-role", false, false),
];

/// The declared roles' logical ids again, each read out of [`ROLES`] so a row names the same id
/// the template declares.
const VECTOR_ROLE: &str = ROLES[0].0;
const SESSION_ROLE: &str = ROLES[1].0;
const WIDE_ROLE: &str = ROLES[2].0;
const BARE_ROLE: &str = ROLES[3].0;

/// Which of the two listings the role `logical` declares allows, as [`ROLES`] states it.
fn grants_of(logical: &str) -> (bool, bool) {
    ROLES
        .iter()
        .find(|(declared, _, _, _)| *declared == logical)
        .map(|(_, _, vector, session)| (*vector, *session))
        .unwrap_or_else(|| panic!("{logical} is not one of this suite's declared roles"))
}

/// The template: every role of [`ROLES`] with the document it carries, and the one function,
/// running as the role `role` names. Nothing else differs between a row's create and its update.
fn template(stack: &str, role: &str) -> String {
    let mut resources = serde_json::Map::new();
    for (logical, suffix, vector, session) in ROLES {
        resources.insert(
            logical.to_string(),
            role_resource(stack, suffix, role_document(vector, session)),
        );
    }
    resources.insert(
        FUNCTION.to_string(),
        function_resource(stack, FUNCTION_SUFFIX, role),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The members every stack operation of this suite sends: the body, the two capabilities a
/// template declaring NAMED IAM resources needs acknowledged, and a tag.
fn stack_members<'a>(stack: &'a str, body: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_IAM"),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ("Tags.member.1.Key", "suite"),
        ("Tags.member.1.Value", "execution-role-swap"),
    ]
}

/// The physical id the stack reports for each resource it declared, read where a client reads it.
async fn physical_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    physical_ids_of(&resources)
}

/// The execution role AWS Lambda itself reports the function running as, as the role NAME its
/// configuration's `Role` ARN names (`arn:…:role/<name>`) — which is the physical id the stack
/// reports for an `AWS::IAM::Role`, so the two are comparable without writing either out.
async fn role_in_force(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let (status, _, configuration) = lambda(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {function:?}, got \
         {status} with {configuration}"
    );
    let arn = configuration["Role"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Role in {configuration}"));
    arn.split('/')
        .next_back()
        .unwrap_or_else(|| panic!("case {case:?}: {arn:?} names no role"))
        .to_string()
}

/// The id the stack reports for one declared resource.
fn reported(physical: &BTreeMap<String, String>, case: &str, logical: &str) -> String {
    physical
        .get(logical)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for {logical}, got {physical:?}"))
}

/// One row: which role the create points the function at, and which role the update repoints it
/// at.
struct Row {
    case: &'static str,
    at_create: &'static str,
    after_swap: &'static str,
}

/// The rows: a swap that gains a grant, one that loses grants, and one that gains a single
/// service's listing while the other service's stays refused.
const ROWS: &[Row] = &[
    Row {
        case: "swapped from the role granting only the vector listing to the role granting both",
        at_create: VECTOR_ROLE,
        after_swap: WIDE_ROLE,
    },
    Row {
        case: "swapped from the role granting both listings to the role granting neither",
        at_create: WIDE_ROLE,
        after_swap: BARE_ROLE,
    },
    Row {
        case: "swapped from the role granting neither listing to the role granting only the \
               session listing",
        at_create: BARE_ROLE,
        after_swap: SESSION_ROLE,
    },
];

/// One create, one invocation, one update that moves only the function's `Role`, and one further
/// invocation per row.
#[tokio::test]
async fn an_execution_role_swap_decides_the_deployed_handlers_own_listings() {
    // ONE emulator for every row, so the refused rows and the served ones are stacks of the same
    // account: a refusal here cannot be an account in which nothing works.
    let router = build_router();
    seed(&router, "the resources every row counts", "role-swap-store").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-role-swap-{index}");

        let created = template(&stack, row.at_create);
        let (status, answered) =
            cloudformation(&router, "CreateStack", &stack_members(&stack, &created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, &stack, CREATE_COMPLETE).await;

        // What the stack HAS: a physical id for every resource it declared, in the services that
        // own them.
        let before = physical_ids(&router, case, &stack).await;
        for (logical, _, _, _) in ROLES {
            assert!(
                before.contains_key(logical),
                "case {case:?}: expected a physical id for {logical}, got {before:?}"
            );
        }
        let function = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);
        assert_eq!(
            reported(&before, case, FUNCTION),
            function,
            "case {case:?}: expected the published name to be the function the stack reports, \
             got {before:?}"
        );

        let (vector, session) = grants_of(row.at_create);
        verdicts(&router, case, "as created", &function, vector, session).await;
        let in_force = role_in_force(&router, case, &function).await;
        assert_eq!(
            in_force,
            reported(&before, case, row.at_create),
            "case {case:?}: expected Lambda to report {function:?} running as the role the \
             template points at, got {in_force:?}"
        );

        // The update: the SAME template, every role's document untouched, with the function
        // repointed at another of the roles the stack already carries.
        let updated = template(&stack, row.after_swap);
        let (status, answered) =
            cloudformation(&router, "UpdateStack", &stack_members(&stack, &updated)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, &stack, UPDATE_COMPLETE).await;

        // The swap changes a property AWS updates without interruption, so the stack still has
        // the same resources: the same function name published, and the same physical id for
        // every declared resource.
        let republished = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);
        assert_eq!(
            republished, function,
            "case {case:?}: expected the swap to leave the published function name alone, got \
             {republished:?}"
        );
        let after = physical_ids(&router, case, &stack).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the swap to replace no declared resource, got {after:?}"
        );

        let (vector, session) = grants_of(row.after_swap);
        verdicts(
            &router,
            case,
            "after the execution-role swap",
            &function,
            vector,
            session,
        )
        .await;
        let in_force = role_in_force(&router, case, &function).await;
        assert_eq!(
            in_force,
            reported(&after, case, row.after_swap),
            "case {case:?}: expected Lambda to report {function:?} running as the role the swap \
             pointed it at, got {in_force:?}"
        );
    }
}
