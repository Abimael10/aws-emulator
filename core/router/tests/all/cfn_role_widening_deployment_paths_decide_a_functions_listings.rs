// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that widens the IAM execution role of a Lambda function it deployed
//! SETTLES whatever PATH the client applied the revision by, and the statements the role then
//! carries decide the deployed function's own AWS SDK calls.
//!
//! RULE: **every stack operation CloudFormation accepts reaches a TERMINAL status
//! `DescribeStacks` publishes — `CREATE_COMPLETE` or a create rollback, `UPDATE_COMPLETE` or an
//! update rollback — whichever way the client stated the revision (a new `TemplateBody` on
//! `UpdateStack`, an `UPDATE` change set created and then executed, or the stack's previous
//! template reused with a changed parameter value), so a client polling it always terminates;
//! and the configuration that operation leaves IN FORCE is what decides a deployed function's
//! own SDK call — served when some statement in force on its execution role allows that action,
//! refused with `AccessDeniedException` while none does — so the SAME call flips from refused to
//! served once the revision has been applied, and only once it has been.**
//!
//! Five crates have to agree for that to hold — CloudFormation drives the deployment and decides
//! the stack's status, IAM holds the role and its documents, STS vends the role's session, Lambda
//! RUNS the handler with the AWS SDK its runtime supplies, and the service the handler calls
//! takes the decision — so the suite lives here rather than in any one of them. Every observation
//! is made where a client makes one: `DescribeStacks`, `DescribeStackResources`,
//! `DescribeChangeSet`, `GetFunctionConfiguration`, `Invoke` and the called services' own
//! listings; never an emulator internal.
//!
//! The rows differ along what the rule generalises over: the PATH the revision travels — the
//! operation the client calls to apply it, how many calls that takes, and whether the widening
//! is carried by the template body or by a parameter value over a template the stack already has.
//! The recorded case (one `UpdateStack` carrying the revised body) is one row of it; the
//! change-set path is the one `aws cloudformation deploy` and `cdk deploy` take, and the
//! previous-template path is the one a parameter override takes.
//!
//! Every expected value is computed here: a refusal is held against the AWS-common error code,
//! and a served listing's count against what the OWNING service answers that same listing with,
//! read through its own API — so no count and no refusal is a literal of one run.
//!
//! `cfn_role_widening_update_decides_a_functions_listings` varies WHICH action the role's
//! document decides and in which direction one `UpdateStack` moves it, and owns the wire helpers
//! this suite drives; `cfn_widened_role_listing_counts_are_stable` varies how much there is to
//! count and how often; `cfn_function_role_swap_decides_its_listings` moves the function between
//! roles instead. All three apply their revision by the one path this suite varies.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The assembled router, the awsQuery caller, the template fragments, the stack-settling poll and
// the invocation verdict reader, owned by the suite that first needed them rather than written
// again here.
use super::cfn_role_widening_update_decides_a_functions_listings::{
    build_router, cloudformation, function_resource, output, role_document, role_resource, seed,
    settled_on, verdicts, CREATE_COMPLETE, UPDATE_COMPLETE,
};
// The awsQuery XML readers this test crate writes ONCE (`tests/all/query_wire.rs`).
use super::query_wire::{opt_element, outputs_of, physical_ids_of};

/// The logical ids the templates declare, and the output key they publish the function under.
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Fn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";

/// The name every row creates its plan under — one name per stack, the way a deploying tool
/// reuses its own.
const CHANGE_SET: &str = "widen-role";

/// The template parameter the previous-template row carries the widening in, and the two values
/// it takes: the action the handler's listing stands on (Amazon S3 Vectors publishes its actions
/// under its own prefix — `ListVectorBuckets` is the operation the handler calls), and an
/// unrelated read of another service, which allows nothing the handler does.
const ACTION_PARAMETER: &str = "ListingAction";
const VECTOR_LISTING_ACTION: &str = "s3vectors:ListVectorBuckets";
const UNRELATED_ACTION: &str = "logs:DescribeLogGroups";

/// The statuses a change-set creation settles on, and the `ExecutionStatus` of a plan that is
/// offered for execution and of one that has been carried through
/// (`ChangeSetSummary$ExecutionStatus`: `AVAILABLE` | `EXECUTE_COMPLETE` | …).
const CHANGE_SET_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CHANGE_SET_FAILED: &str = "FAILED";
const AVAILABLE: &str = "AVAILABLE";
const EXECUTE_COMPLETE: &str = "EXECUTE_COMPLETE";

/// How often to look again for a change-set creation the clock owes, and how long to go on
/// looking. The span is the service's own, so no fixed sleep names the instant it ends; the
/// deadline is generous because every case asserts the CONDITION and never the time it took.
const LOOK: Duration = Duration::from_millis(50);
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The template the body-carried rows deploy: the execution role always grants the session
/// listing, and grants the vector listing only where `vector_listing` asks for it.
fn body_template(stack: &str, vector_listing: bool) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: role_resource(stack, "role", role_document(vector_listing, true)),
            FUNCTION: function_resource(stack, "fn", ROLE),
        },
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The template the previous-template row deploys: the same two resources, except that the action
/// the role's extra statement allows is whatever the stack's [`ACTION_PARAMETER`] is set to — so
/// a changed parameter value, over the template the stack already carries, is the whole of the
/// revision.
fn parameterised_template(stack: &str) -> String {
    let mut grants = role_document(false, true);
    grants["Statement"]
        .as_array_mut()
        .expect("a role document states its statements as a list")
        .push(json!({
            "Effect": "Allow",
            "Action": { "Ref": ACTION_PARAMETER },
            "Resource": "*",
        }));
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            ACTION_PARAMETER: { "Type": "String" },
        },
        "Resources": {
            ROLE: role_resource(stack, "role", grants),
            FUNCTION: function_resource(stack, "fn", ROLE),
        },
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The members every operation of this suite that submits a template sends: the body and the two
/// capabilities a template declaring NAMED IAM resources needs acknowledged.
fn template_members<'a>(stack: &'a str, body: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_IAM"),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
    ]
}

/// Poll `DescribeChangeSet` the way a deploying tool's waiter does and answer the status the
/// creation SETTLED on, its `ExecutionStatus` and the answer both were read from.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (http_status, body) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected HTTP 200 from DescribeChangeSet, got {http_status} with \
             {body}"
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        let execution = opt_element(&body, "ExecutionStatus").unwrap_or_default();
        if status == CHANGE_SET_CREATE_COMPLETE || status == CHANGE_SET_FAILED {
            return (status, execution, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the change set of {stack} to settle, last saw \
             {status:?}/{execution:?} in {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Which way a row states the revision that widens the role.
#[derive(Clone, Copy, Debug)]
enum Path {
    /// `UpdateStack` carrying the revised template body — what a client calling the operation
    /// directly does.
    RevisedBody,
    /// `CreateChangeSet(ChangeSetType=UPDATE)` followed by `ExecuteChangeSet` — the path a
    /// deploying tool takes.
    ChangeSet,
    /// `UpdateStack` with `UsePreviousTemplate` and a changed parameter value — the path a
    /// parameter override takes.
    PreviousTemplate,
}

/// One row: the path the revision travels, and the stack name it travels over.
struct Row {
    case: &'static str,
    stack: &'static str,
    path: Path,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded case.
        case: "the revised template body, on UpdateStack",
        stack: "cfn-widen-body",
        path: Path::RevisedBody,
    },
    Row {
        case: "an UPDATE change set, created and then executed",
        stack: "cfn-widen-change-set",
        path: Path::ChangeSet,
    },
    Row {
        case: "the stack's previous template, with a changed parameter value",
        stack: "cfn-widen-parameter",
        path: Path::PreviousTemplate,
    },
];

/// The template and the members a row's CREATE submits.
fn create_members(row: &Row, body: &str) -> Vec<(String, String)> {
    let mut members: Vec<(String, String)> = template_members(row.stack, body)
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    if matches!(row.path, Path::PreviousTemplate) {
        members.push((
            "Parameters.member.1.ParameterKey".to_string(),
            ACTION_PARAMETER.to_string(),
        ));
        members.push((
            "Parameters.member.1.ParameterValue".to_string(),
            UNRELATED_ACTION.to_string(),
        ));
    }
    members
}

/// Borrow an owned member list as the pairs [`cloudformation`] takes.
fn borrowed(members: &[(String, String)]) -> Vec<(&str, &str)> {
    members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// RULE: a stack operation that widens a deployed function's execution role settles terminally
/// whichever way the client stated the revision, and the function's own listing is served from
/// the moment the revision is in force and refused before it.
#[tokio::test]
async fn every_path_that_widens_a_deployed_role_settles_and_decides_its_listings() {
    // ONE emulator for every row, so the refused observations and the served ones are stacks of
    // the same account: a refusal here cannot be an account in which nothing works.
    let router = build_router();
    seed(&router, "the resources every row counts", "widen-paths-store").await;

    for row in ROWS {
        let case = row.case;
        let stack = row.stack;

        // ---- The create: a least-privilege role that does not allow the vector listing. ----
        let created_body = match row.path {
            Path::PreviousTemplate => parameterised_template(stack),
            _ => body_template(stack, false),
        };
        let members = create_members(row, &created_body);
        let (status, answered) =
            cloudformation(&router, "CreateStack", &borrowed(&members)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, stack, CREATE_COMPLETE).await;

        // What the stack HAS: a physical id for each resource it declared, in the services that
        // own them.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let physical = physical_ids_of(&resources);
        for logical in [ROLE, FUNCTION] {
            assert!(
                physical
                    .get(logical)
                    .is_some_and(|id| !id.is_empty() && id != logical),
                "case {case:?}: expected a physical id for {logical}, got {physical:?}"
            );
        }

        let function = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);
        verdicts(&router, case, "as created", &function, false, true).await;

        // ---- The revision, stated the way this row states it. ----
        let revised_body = body_template(stack, true);
        match row.path {
            Path::RevisedBody => {
                let (status, answered) = cloudformation(
                    &router,
                    "UpdateStack",
                    &template_members(stack, &revised_body),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with \
                     {answered}"
                );
            }
            Path::ChangeSet => {
                let mut members = template_members(stack, &revised_body);
                members.push(("ChangeSetName", CHANGE_SET));
                members.push(("ChangeSetType", "UPDATE"));
                let (status, answered) =
                    cloudformation(&router, "CreateChangeSet", &members).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from CreateChangeSet, got {status} with \
                     {answered}"
                );
                let (settled, execution, plan) = settled_change_set(&router, case, stack).await;
                assert_eq!(
                    (settled.as_str(), execution.as_str()),
                    (CHANGE_SET_CREATE_COMPLETE, AVAILABLE),
                    "case {case:?}: the plan must be offered for execution, got {plan}"
                );

                // A plan that was CREATED and not executed has applied nothing: the stack is the
                // one the create left and the function's listing is still refused.
                let still = settled_on(&router, case, stack, CREATE_COMPLETE).await;
                assert!(
                    !still.is_empty(),
                    "case {case:?}: DescribeStacks must answer while a plan is pending"
                );
                verdicts(&router, case, "the plan pending", &function, false, true).await;

                let (status, answered) = cloudformation(
                    &router,
                    "ExecuteChangeSet",
                    &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from ExecuteChangeSet, got {status} with \
                     {answered}"
                );
            }
            Path::PreviousTemplate => {
                let (status, answered) = cloudformation(
                    &router,
                    "UpdateStack",
                    &[
                        ("StackName", stack),
                        ("UsePreviousTemplate", "true"),
                        ("Capabilities.member.1", "CAPABILITY_IAM"),
                        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
                        ("Parameters.member.1.ParameterKey", ACTION_PARAMETER),
                        ("Parameters.member.1.ParameterValue", VECTOR_LISTING_ACTION),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with \
                     {answered}"
                );
            }
        }

        // ---- Whatever the path, the stack settles terminally... ----
        settled_on(&router, case, stack, UPDATE_COMPLETE).await;
        if matches!(row.path, Path::ChangeSet) {
            let (status, plan) = cloudformation(
                &router,
                "DescribeChangeSet",
                &[("StackName", stack), ("ChangeSetName", CHANGE_SET)],
            )
            .await;
            assert_eq!(
                (status, opt_element(&plan, "ExecutionStatus")),
                (200, Some(EXECUTE_COMPLETE.to_string())),
                "case {case:?}: the executed plan must report it was carried through, got \
                 {status} with {plan}"
            );
        }

        // ...and the identical invocation is served from then on, with the count the owning
        // service itself holds — the same answer when it is made again.
        verdicts(&router, case, "after the revision", &function, true, true).await;
        verdicts(
            &router,
            case,
            "the same invocation again",
            &function,
            true,
            true,
        )
        .await;
    }
}

/// The stack keeps publishing the SAME function for every path: a widening is a change to what
/// the function may do and never a change to which function the stack deployed, so a client's
/// handle on it survives the revision. Checked over the one path whose revision does not restate
/// the template at all, which is where a re-created resource would be hardest to notice.
#[tokio::test]
async fn a_parameter_only_revision_keeps_the_stacks_published_handles() {
    let case = "a parameter-only revision over the previous template";
    let router = build_router();
    seed(&router, case, "widen-handles-store").await;

    let stack = "cfn-widen-handles";
    let body = parameterised_template(stack);
    let mut members = template_members(stack, &body);
    members.push(("Parameters.member.1.ParameterKey", ACTION_PARAMETER));
    members.push(("Parameters.member.1.ParameterValue", UNRELATED_ACTION));
    let (status, answered) = cloudformation(&router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
    );
    let described = settled_on(&router, case, stack, CREATE_COMPLETE).await;
    let before = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    let ids_before = physical_ids_of(&resources);

    let (status, answered) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("UsePreviousTemplate", "true"),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
            ("Parameters.member.1.ParameterKey", ACTION_PARAMETER),
            ("Parameters.member.1.ParameterValue", VECTOR_LISTING_ACTION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
    );
    let described = settled_on(&router, case, stack, UPDATE_COMPLETE).await;
    let after = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);
    assert_eq!(
        after, before,
        "case {case:?}: expected the stack to go on publishing {before:?}, got {after:?}"
    );
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    let ids_after = physical_ids_of(&resources);
    assert_eq!(
        ids_after, ids_before,
        "case {case:?}: expected every declared resource to keep its physical id, got \
         {ids_after:?} against {ids_before:?}"
    );

    // The widened grant is in force on the role the stack still holds.
    verdicts(&router, case, "after the revision", &after, true, true).await;
}
