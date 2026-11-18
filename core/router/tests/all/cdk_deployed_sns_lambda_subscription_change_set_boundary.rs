// SPDX-License-Identifier: CC0-1.0

//! The SNS -> Lambda permission boundary holds over the deployment seam a CDK app is rolled
//! out through: a CHANGE SET, created and then executed, rather than a direct `CreateStack` or
//! `UpdateStack`.
//!
//! The rule under test: **a publish to a topic delivers the message to every confirmed
//! `lambda`-protocol subscription of that topic ("lambda – delivery of JSON-encoded message to
//! an Lambda function", `SubscribeInput$Protocol`) by invoking the subscribed function
//! asynchronously, and that invocation is made exactly while the FUNCTION's own resource-based
//! policy grants `lambda:InvokeFunction` to the service principal `sns.amazonaws.com` for that
//! topic — whatever call put the declaration into the stack.** A declaration applied by
//! `ExecuteChangeSet` is the same declaration as one applied by `CreateStack`, so what a client
//! can observe downstream of the function must not depend on which of the two it used.
//!
//! Why the seam matters: a CDK app is never deployed by `CreateStack`. `cdk deploy` creates a
//! change set over the synthesized template and executes it, every time — so a wiring that
//! works when the template is handed straight to `CreateStack` and not when the same template
//! travels through a change set is a wiring no CDK-deployed stack ever gets. The sibling suites
//! read this boundary over `CreateStack`/`UpdateStack`
//! ([`crate::sns_lambda_subscription_permission_boundary`]) and over a synthesized template's
//! own shape ([`crate::sns_lambda_cdk_subscription_delivery`]); what this one adds is the
//! change-set seam, and the REMOVAL direction — the grant withdrawn again by a later
//! deployment, after which the deliveries have to stop.
//!
//! Four crates have to agree for the rule to hold (CloudFormation plans and executes the change
//! set, Lambda holds the policy and runs the handler, SNS fans the publish out, Amazon SQS holds
//! what the handler wrote), so the suite lives here rather than in any one crate. The
//! observation is made where a client makes it: on the sink queue the function forwards to, and
//! on the function's own policy as Lambda reports it — never on an emulator internal.
//!
//! The rows differ along exactly what the rule generalises over: which change-set type carried
//! the declaration (a `CREATE` change set, then `UPDATE` change sets) and whether that
//! declaration states the `AWS::Lambda::Permission`, added and withdrawn and added again — so
//! neither direction can pass by a grant that was simply never applied or never removed.

use std::time::{Duration, Instant};

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, outputs_of};
// The emulator, the wires and the delivery observations the sibling boundary suite already
// owns: one harness for this rule, so the two suites cannot drift about what the boundary is
// or about what "nothing arrived" means.
use crate::sns_lambda_subscription_permission_boundary::{
    build_router, cloudformation, execution_role, lambda, sns, stayed_quiet_about, template,
    wait_for, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The stack status an executed `UPDATE` change set settles the stack on.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The `Status` a change set whose plan is ready reports.
const CHANGE_SET_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `ExecutionStatus` a change set that may be executed reports.
const CHANGE_SET_AVAILABLE: &str = "AVAILABLE";

/// The service principal a delivery to a subscribed function is performed as, which the
/// function's own policy has to admit.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action such a delivery performs on the function.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `State` AWS Lambda reports for a function it takes invocations on: "During this time,
/// you can't invoke or modify the function" is what it reports `Pending` for
/// (`CreateFunction`), so a row that must observe NOTHING waits for this first — otherwise an
/// absent delivery says only that the function was not up yet.
const ACTIVE: &str = "Active";

/// How long either wait on a condition another service owes keeps looking. Generous on
/// purpose: nothing is asserted about how long any of it takes, only about what holds in the
/// end.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);

/// One deployment of the app, as a change set: what it declares and what must follow.
#[derive(Debug, Clone, Copy)]
struct Phase {
    case: &'static str,
    /// The change set's own name, which is what `cdk deploy` passes.
    change_set: &'static str,
    /// `CREATE` for the deployment that brings the stack into being, `UPDATE` for every later
    /// one — the `ChangeSetType` a client states.
    change_set_type: &'static str,
    /// Whether this deployment's template declares the `AWS::Lambda::Permission`.
    with_permission: bool,
    /// The status the stack settles on once the change set is executed.
    settled: &'static str,
}

/// The rows: one app, deployed four times through change sets, with the grant declared,
/// withdrawn and declared again.
const PHASES: &[Phase] = &[
    Phase {
        case: "a CREATE change set whose template declares no grant",
        change_set: "cdk-deploy-1",
        change_set_type: "CREATE",
        with_permission: false,
        settled: CREATE_COMPLETE,
    },
    Phase {
        case: "an UPDATE change set that adds the grant",
        change_set: "cdk-deploy-2",
        change_set_type: "UPDATE",
        with_permission: true,
        settled: UPDATE_COMPLETE,
    },
    Phase {
        case: "an UPDATE change set that withdraws the grant again",
        change_set: "cdk-deploy-3",
        change_set_type: "UPDATE",
        with_permission: false,
        settled: UPDATE_COMPLETE,
    },
    Phase {
        case: "an UPDATE change set that declares the grant a second time",
        change_set: "cdk-deploy-4",
        change_set_type: "UPDATE",
        with_permission: true,
        settled: UPDATE_COMPLETE,
    },
];

/// Create the change set `phase` states over `template`, wait for its plan, execute it, and
/// answer the status the stack settled on with the `DescribeStacks` answer it was read from —
/// the three calls `cdk deploy` makes, in that order.
async fn deploy_through_a_change_set(
    router: &MultiServiceRouter,
    phase: &Phase,
    stack: &str,
    body: &str,
) -> (String, String) {
    let case = phase.case;
    let created = cloudformation(
        router,
        "CreateChangeSet",
        &[
            ("StackName", stack),
            ("ChangeSetName", phase.change_set),
            ("ChangeSetType", phase.change_set_type),
            ("TemplateBody", body),
        ],
    )
    .await;
    // A client addresses the change set by the `Id` the create answered with, which is what its
    // own waiter and its `ExecuteChangeSet` carry.
    let id = element(&created, "Id");
    assert!(
        !id.is_empty(),
        "case {case:?}: CreateChangeSet must answer the new change set's Id, got {created}"
    );

    let (planned, described) = wait_for_change_set_plan(router, &id, stack).await;
    assert_eq!(
        planned,
        (
            CHANGE_SET_CREATE_COMPLETE.to_string(),
            CHANGE_SET_AVAILABLE.to_string()
        ),
        "case {case:?}: expected the change set to be created and executable, got {described}"
    );

    cloudformation(
        router,
        "ExecuteChangeSet",
        &[("ChangeSetName", &id), ("StackName", stack)],
    )
    .await;
    wait_for_stack_status(router, stack, phase.settled).await
}

/// Poll `DescribeChangeSet` until the change set is created and executable, and answer the
/// `(Status, ExecutionStatus)` it reported last with the answer they were read from. `Status`
/// is the change set's own creation status and `ExecutionStatus` is whether it may be
/// executed, so both are read. The wait is on that CONDITION, looking again until it holds or
/// [`SETTLE_BUDGET`] passes — after which the LAST pair seen is answered, so the caller's own
/// assertion is what reports a change set that never became executable.
///
/// The sibling poller `crate::cloudformation_domain_access_policy_revision::settled_change_set`
/// reads the same two members, but through a harness bound to its own Region, and the stack
/// here lives in the Region the shared stack waiter polls — so it would describe another
/// Region's stack.
async fn wait_for_change_set_plan(
    router: &MultiServiceRouter,
    id: &str,
    stack: &str,
) -> ((String, String), String) {
    let giving_up_at = Instant::now() + SETTLE_BUDGET;
    loop {
        let described = cloudformation(
            router,
            "DescribeChangeSet",
            &[("ChangeSetName", id), ("StackName", stack)],
        )
        .await;
        let reported = (
            element(&described, "Status"),
            element(&described, "ExecutionStatus"),
        );
        let ready = reported.0 == CHANGE_SET_CREATE_COMPLETE && reported.1 == CHANGE_SET_AVAILABLE;
        if ready || Instant::now() >= giving_up_at {
            return (reported, described);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Poll AWS Lambda until it reports `function` [`ACTIVE`], and answer the state it reported
/// then. No fixed sleep names the instant a created function becomes invocable: the wait is on
/// the state that service itself publishes.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let giving_up_at = Instant::now() + SETTLE_BUDGET;
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AWS Lambda to report {function}, got {status} with \
             {configuration}"
        );
        let last = configuration["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if last == ACTIVE || Instant::now() >= giving_up_at {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The resource-based policy AWS Lambda reports for `function`, or `None` where it reports
/// none at all — which is what a function whose only statement was withdrawn carries.
async fn resource_policy(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> Option<String> {
    let (status, answered) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/policy"),
        None,
    )
    .await;
    if status == 200 {
        return Some(
            answered["Policy"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a Policy document, got {answered}")
                })
                .to_string(),
        );
    }
    // Lambda publishes `ResourceNotFoundException` for a function carrying no policy; any
    // other refusal is this suite's own fault rather than the answer "there is no policy",
    // so it fails the case instead of reading as an absence.
    let code = answered["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected the policy or the modelled absence of one, got {status} with \
         {answered}"
    );
    None
}

/// The condition operator and key AWS Lambda's `AddPermission` expresses a `SourceArn` as:
/// "specify the ARN of the associated resource as the SourceArn" (`AddPermission`), compared
/// as an ARN rather than as a string — so the one spelling a grant scoped to a topic carries.
const SOURCE_ARN_OPERATOR: &str = "ArnLike";
const SOURCE_ARN_KEY: &str = "AWS:SourceArn";

/// Whether `policy` states a grant of [`INVOKE_ACTION`] to [`SNS_PRINCIPAL`] scoped to `topic`
/// — the statement an `AWS::Lambda::Permission` for a subscribed topic declares.
///
/// A policy document states `Statement` as one statement or as a list of them, and each
/// statement's `Action` as one action or as a list, so both forms are read: a grant stated in
/// the other form would otherwise read as no grant at all.
fn grants_sns_the_invoke(policy: Option<&str>, topic: &str) -> bool {
    let Some(document) = policy else {
        return false;
    };
    let parsed: Value = serde_json::from_str(document).unwrap_or(Value::Null);
    one_or_more(&parsed["Statement"]).iter().any(|statement| {
        let principal = statement["Principal"]["Service"].as_str().unwrap_or_default();
        let names_the_action = one_or_more(&statement["Action"])
            .iter()
            .any(|action| action.as_str() == Some(INVOKE_ACTION));
        let source = statement["Condition"][SOURCE_ARN_OPERATOR][SOURCE_ARN_KEY]
            .as_str()
            .unwrap_or_default();
        principal == SNS_PRINCIPAL && names_the_action && source == topic
    })
}

/// A policy document's member that is either one value or a list of them, as the list a
/// reader walks: nothing at all for a member the document does not carry.
fn one_or_more(member: &Value) -> Vec<Value> {
    match member {
        Value::Array(values) => values.clone(),
        Value::Null => Vec::new(),
        single => vec![single.clone()],
    }
}

/// Every phase: the same app deployed through a change set, with the grant declared, withdrawn
/// and declared again — and the deliveries following the grant each time.
#[tokio::test]
async fn a_change_set_deployed_sns_lambda_subscription_delivers_exactly_while_sns_is_granted() {
    let router = build_router();
    let stack = "sns-lambda-change-set";

    // The execution role the template names, created where that service holds it.
    execution_role(&router).await;

    let function = format!("{stack}-forwarder");
    let mut topic = String::new();
    let mut queue_url = String::new();

    for (index, phase) in PHASES.iter().enumerate() {
        let case = phase.case;
        let body = template(stack, phase.with_permission);
        let (settled, described) = deploy_through_a_change_set(&router, phase, stack, &body).await;
        let failures = if settled == phase.settled {
            Vec::new()
        } else {
            let events =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
            elements(&events, "ResourceStatusReason")
        };
        assert_eq!(
            settled, phase.settled,
            "case {case:?}: expected the executed change set to settle the stack on {}, got \
             {described} with resource failures {failures:?}",
            phase.settled
        );

        // The topic and the sink are the stack's own, published by it: read on the first
        // deployment and the SAME resources for every later one, so each phase is observed
        // through the handles a client already holds.
        let outputs = outputs_of(&described);
        let published_topic = outputs.get("TopicArn").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish its TopicArn, got {described}")
        });
        let published_queue = outputs.get("QueueUrl").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish its QueueUrl, got {described}")
        });
        if topic.is_empty() {
            topic = published_topic.clone();
            queue_url = published_queue.clone();
        }
        assert_eq!(
            (published_topic.as_str(), published_queue.as_str()),
            (topic.as_str(), queue_url.as_str()),
            "case {case:?}: a deployment that only changes the grant must leave the topic and \
             the sink where they were, got {described}"
        );

        // The function the stack reported created is one AWS Lambda takes invocations on, so a
        // phase that observes NOTHING observes a withheld invocation rather than a function
        // that was not up yet.
        let state = wait_until_active(&router, case, &function).await;
        assert_eq!(
            state, ACTIVE,
            "case {case:?}: expected AWS Lambda to report {function} {ACTIVE}, got {state:?}"
        );

        // The grant as the OWNING service reports it: the deployment's declaration really
        // reached the function's own policy, or really left it.
        let policy = resource_policy(&router, case, &function).await;
        assert_eq!(
            grants_sns_the_invoke(policy.as_deref(), &topic),
            phase.with_permission,
            "case {case:?}: expected the function's policy to grant {INVOKE_ACTION} to \
             {SNS_PRINCIPAL} for {topic}: {}, got {policy:?}",
            phase.with_permission
        );

        // And the delivery the publish owes, observed where a client observes it.
        let marker = format!("marker-{index}");
        let answered = sns(
            &router,
            "Publish",
            &[("TopicArn", &topic), ("Message", &marker)],
        )
        .await;
        assert!(
            !element(&answered, "MessageId").is_empty(),
            "case {case:?}: the publish is accepted whether or not the delivery is permitted, \
             got {answered}"
        );
        if phase.with_permission {
            let delivered = wait_for(&router, &queue_url, &marker).await;
            assert!(
                delivered.iter().any(|held| held.contains(&marker)),
                "case {case:?}: expected the subscribed function to be invoked and to forward \
                 {marker:?} to the sink queue, got {delivered:?}"
            );
        } else {
            // Only THIS phase's publication counts: an earlier phase's body becomes visible
            // again on the sink, and would otherwise read as this phase's delivery.
            let quiet = stayed_quiet_about(&router, &queue_url, Some(&marker)).await;
            assert!(
                quiet.is_empty(),
                "case {case:?}: expected the function never to be invoked, so nothing carrying \
                 {marker:?} reaches the sink queue, got {quiet:?}"
            );
        }
    }
}
