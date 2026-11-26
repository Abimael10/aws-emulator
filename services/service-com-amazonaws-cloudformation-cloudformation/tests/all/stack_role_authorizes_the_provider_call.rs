// SPDX-License-Identifier: CC0-1.0

//! The STACK SERVICE ROLE and the call CloudFormation itself makes to the provider a custom
//! resource's `ServiceToken` names — end to end over the wire.
//!
//! The rule under test: **a stack operation carrying `RoleARN` makes CloudFormation's own call
//! to a custom resource's `ServiceToken` with that role's credentials, so the role's
//! identity-based policies have to ALLOW the IAM action that token's transport performs — the
//! invoke of a function, the publish to a topic — on the token's OWN ARN. A role no readable
//! document allows that call to reaches no provider at all: the request is never sent, the
//! resource reaches `CREATE_FAILED` carrying the sentence AWS refuses an unauthorized call
//! with (which NAMES the action and the ARN it was asked for), the create rolls back to
//! `ROLLBACK_COMPLETE` and the siblings it had already created are removed. A role that does
//! allow it — and a stack that names no role at all, which runs with the caller's own
//! permissions — reaches the provider, settles at `CREATE_COMPLETE`, and publishes every
//! declared `Output` with the `OutputValue` its expression resolved to, the provider's response
//! `Data` included.**
//!
//! That is one rule about IAM's ordinary default deny over the ONE ACTION, applied to the one
//! call CloudFormation makes on an ARN the TEMPLATE supplies: the action asked for is the
//! documented action of the service being emulated, so a role that does not allow that action
//! cannot make the call. The deployment's own provisioning calls are decided the same way, each
//! against the action it invokes (`state::service_role`) — which is why every row below grants
//! its role the two Amazon SNS actions the sibling topic the template declares needs (the topic
//! is really created there, and really removed when a create rolls back) while leaving the
//! PROVIDER call to be decided on its own.
//!
//! Four services have to agree for this to hold, and this crate's harness mounts them all on
//! one engine: IAM holds the service role whose documents decide the call, AWS Lambda holds (and
//! runs) the provider function, Amazon SNS holds the topic a token can also name and the sibling
//! the rollback removes, and Amazon CloudWatch Logs holds what the provider's run printed —
//! which is the evidence that a refused call reached no provider.
//!
//! The rows differ along what the rule generalises over: WHAT the role's documents say about the
//! call (an allow on the token's own ARN, an allow through wildcard patterns, nothing about the
//! call at all, an allow on another resource's ARN, an allow beside an explicit deny, and no
//! role at all), and WHICH transport the `ServiceToken` names — and so which IAM action reaching
//! the provider performs.
//!
//! Nothing here sleeps for a span the emulator owes: every wait is a poll for the CONDITION
//! under a generous deadline.

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_resource_provider_denied_rollback::logged_lines;
use crate::harness::{
    create_stack, failed_resource_reason, member, quoted_and_class, router_with_engine,
    settled_stack, xml_text, REGION,
};
use crate::lambda_backed_custom_resources::{given_function, given_role};
use crate::stack_lifecycle::{iam, sns, ACCOUNT_ID};
use crate::stack_outputs::{output_for, wire_outputs};

/// The logical ids every template here declares its two resources under.
const CUSTOM: &str = "Provisioned";
const SIBLING: &str = "Marker";

/// The `HandlerErrorCode` class a refused call is reported under: the class a client reads to
/// learn the resource failed because a call was not permitted.
const ACCESS_DENIED_CLASS: &str = "AccessDenied";

/// The physical resource id the provider answers with, and the `Data` key it publishes — so what
/// a completed stack reports could only have come from a run of the provider's own code.
const ANSWERED_ID: &str = "provisioned-instance";
const ANSWERED_DATA_KEY: &str = "Marker";

/// What the provider prints for every request it is sent, which is the evidence a refused call
/// never reached it.
const INVOKED_PREFIX: &str = "provider invoked for ";

/// The provider created outside the stack: it prints the request type it was sent, then PUTs its
/// answer to the request's own `ResponseURL` with the standard library's HTTP client, publishing
/// the marker its properties carried.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    print("provider invoked for %s" % event["RequestType"])
    document = json.dumps(
        {
            "Status": "SUCCESS",
            "PhysicalResourceId": "provisioned-instance",
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": {"Marker": event["ResourceProperties"].get("Marker", "")},
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=document,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// Which transport a `ServiceToken` names, and so which IAM action reaching the provider
/// performs: invoking a function "requires permission for the `lambda:InvokeFunction` action"
/// (`Invoke`) and publishing to a topic requires `sns:Publish` (`Publish`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Function,
    Topic,
}

impl Token {
    /// The fully qualified IAM action reaching a provider over this transport asks for.
    fn action(self) -> &'static str {
        match self {
            Self::Function => "lambda:InvokeFunction",
            Self::Topic => "sns:Publish",
        }
    }
}

/// What the stack's service role's identity policies say about that call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The stack names no `RoleARN` at all, so it runs with the caller's own permissions.
    NoRole,
    /// An allow of the action on the token's own ARN.
    OnTheToken,
    /// An allow whose action and resource are patterns the call matches.
    Wildcards,
    /// Documents that say nothing about the call.
    Unrelated,
    /// An allow of the action on ANOTHER resource of the same service.
    OnAnother,
    /// An allow of everything beside an explicit deny of the call.
    DeniedExplicitly,
}

/// The reason clause the refusal names, as IAM's rules spell them: a call no document allows is
/// withheld by the absence of a grant, and one a document denies by that deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    NoGrant,
    ExplicitDeny,
}

/// One row: the names its records are created under, which transport its `ServiceToken` names,
/// what its service role says about the call, and the clause the refusal is expected to name —
/// `None` for a row the role permits.
struct Row {
    case: &'static str,
    stack: &'static str,
    role_name: &'static str,
    function_name: &'static str,
    topic_name: &'static str,
    marker: &'static str,
    token: Token,
    grant: Grant,
    refused_with: Option<Clause>,
}

/// The rows whose role PERMITS the call: the provider is reached and the stack completes.
const PERMITTED: &[Row] = &[
    Row {
        case: "a role allowing the invoke on the provider's own ARN",
        stack: "role-allows-the-token",
        role_name: "role-allows-the-token-deployer",
        function_name: "role-allows-the-token-provider",
        topic_name: "role-allows-the-token-marker",
        marker: "allowed on the token",
        token: Token::Function,
        grant: Grant::OnTheToken,
        refused_with: None,
    },
    Row {
        case: "a role allowing it through patterns the call matches",
        stack: "role-allows-by-pattern",
        role_name: "role-allows-by-pattern-deployer",
        function_name: "role-allows-by-pattern-provider",
        topic_name: "role-allows-by-pattern-marker",
        marker: "allowed by a pattern",
        token: Token::Function,
        grant: Grant::Wildcards,
        refused_with: None,
    },
    Row {
        case: "a stack naming no role, which runs with the caller's own permissions",
        stack: "no-role-at-all",
        role_name: "no-role-at-all-unused",
        function_name: "no-role-at-all-provider",
        topic_name: "no-role-at-all-marker",
        marker: "no role to judge the call on",
        token: Token::Function,
        grant: Grant::NoRole,
        refused_with: None,
    },
];

/// The rows whose role does NOT permit the call: nothing is sent, and the stack rolls back.
const REFUSED: &[Row] = &[
    Row {
        case: "a role whose documents say nothing about the invoke",
        stack: "role-says-nothing",
        role_name: "role-says-nothing-deployer",
        function_name: "role-says-nothing-provider",
        topic_name: "role-says-nothing-marker",
        marker: "never answered",
        token: Token::Function,
        grant: Grant::Unrelated,
        refused_with: Some(Clause::NoGrant),
    },
    Row {
        case: "a role allowing the invoke on another function's ARN",
        stack: "role-allows-another",
        role_name: "role-allows-another-deployer",
        function_name: "role-allows-another-provider",
        topic_name: "role-allows-another-marker",
        marker: "never answered",
        token: Token::Function,
        grant: Grant::OnAnother,
        refused_with: Some(Clause::NoGrant),
    },
    Row {
        case: "a role allowing everything beside an explicit deny of the invoke",
        stack: "role-denies-the-invoke",
        role_name: "role-denies-the-invoke-deployer",
        function_name: "role-denies-the-invoke-provider",
        topic_name: "role-denies-the-invoke-marker",
        marker: "never answered",
        token: Token::Function,
        grant: Grant::DeniedExplicitly,
        refused_with: Some(Clause::ExplicitDeny),
    },
    // The same rule over the OTHER transport a `ServiceToken` can name: the action the role has
    // to allow is the one reaching the provider over that transport performs, so a topic-backed
    // provider is refused on the publish rather than on the invoke.
    Row {
        case: "a topic-backed provider whose role says nothing about the publish",
        stack: "role-says-nothing-topic",
        role_name: "role-says-nothing-topic-deployer",
        function_name: "role-says-nothing-topic-provider",
        topic_name: "role-says-nothing-topic-marker",
        marker: "never answered",
        token: Token::Topic,
        grant: Grant::Unrelated,
        refused_with: Some(Clause::NoGrant),
    },
];

/// The ARN Amazon SNS keys one row's topic by, which is both the sibling the rollback removes and
/// the `ServiceToken` of the row whose provider is a topic.
fn topic_arn(row: &Row) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:{}", row.topic_name)
}

/// One statement of an identity policy, as IAM's own grammar writes it.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({ "Effect": effect, "Action": action, "Resource": resource })
}

/// The identity-policy document one row's service role carries, or `None` for a row that names no
/// role: what it says about the call is the row's `grant`, stated over the action that row's
/// transport performs and the ARN its token names.
///
/// Every document also allows the two Amazon SNS actions the SIBLING topic's own lifecycle
/// performs: that topic is created in Amazon SNS by the deployment itself and removed again when
/// a failed create rolls back, and a deployment's call whose action no readable document allows
/// is refused (`state::service_role`) — so a role allowed neither would fail the sibling instead
/// of reaching the call each row is about. They are `sns:CreateTopic` and `sns:DeleteTopic`
/// rather than a wildcard, so the `sns:Publish` a topic-backed `ServiceToken` performs is still a
/// call no document allows.
fn service_role_document(row: &Row, token: &str) -> Option<Value> {
    let action = row.token.action();
    let namespace = action.split(':').next().unwrap_or_default();
    let mut statements = match row.grant {
        Grant::NoRole => return None,
        Grant::OnTheToken => vec![statement("Allow", action, token)],
        Grant::Wildcards => vec![statement("Allow", &format!("{namespace}:*"), "*")],
        // A grant about another call entirely: IAM's default deny is what withholds this one.
        Grant::Unrelated => vec![statement("Allow", "ssm:GetParameter", "*")],
        Grant::OnAnother => vec![statement("Allow", action, &format!("{token}-elsewhere"))],
        Grant::DeniedExplicitly => vec![
            statement("Allow", "*", "*"),
            statement("Deny", action, token),
        ],
    };
    statements.push(statement("Allow", "sns:CreateTopic", "*"));
    statements.push(statement("Allow", "sns:DeleteTopic", "*"));
    Some(json!({ "Version": "2012-10-17", "Statement": statements }))
}

/// The service role one row deploys under, created through IAM's own wire: a role CloudFormation
/// may assume, carrying the row's one inline policy. Answers the ARN IAM minted for it, or `None`
/// for a row that names no role.
async fn given_service_role(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    token: &str,
) -> Option<String> {
    let document = service_role_document(row, token)?;
    // `RoleARN` is the role "that CloudFormation assumes", so the role has to trust that
    // principal for `sts:AssumeRole` or the operation is refused before it deploys anything.
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "cloudformation.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (code, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", row.role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateRole({:?}) must answer 200, got {code} {created}",
        row.role_name
    );
    let (code, answered) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", row.role_name),
            ("PolicyName", "what-the-deployment-may-do"),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: PutRolePolicy({:?}) must answer 200, got {code} {answered}",
        row.role_name
    );
    Some(xml_text(&created, "Arn").unwrap_or_else(|| {
        panic!("case {case:?}: expected CreateRole to report an ARN, got {created}")
    }))
}

/// The principal a session of `role_arn` is, which is what a refusal of its calls names the caller
/// by: the role session CloudFormation makes a stack's calls under.
fn session_principal(role_arn: &str, role_name: &str) -> String {
    let account = ::service_common::arn::parse(role_arn).map_or("", |arn| arn.account);
    format!("arn:aws:sts::{account}:assumed-role/{role_name}/AWSCloudFormation")
}

/// The provider function one row names, created through AWS Lambda's own wire before any stack
/// names it; answers the ARN Lambda minted for it.
///
/// Its execution role is granted the log writes every function needs, so what the run PRINTED is
/// readable — which is how a row says whether the provider was reached at all.
async fn given_provider(router: &MultiServiceRouter, case: &str, row: &Row) -> String {
    let execution_role = format!("{}-execution", row.function_name);
    let role_arn = given_role(router, case, &execution_role).await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [statement("Allow", "logs:*", "*")],
    })
    .to_string();
    let (code, answered) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", &execution_role),
            ("PolicyName", "what-the-provider-may-do"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: PutRolePolicy({execution_role:?}) must answer 200, got {code} {answered}"
    );
    given_function(router, case, row.function_name, &role_arn, PROVIDER_SOURCE).await
}

/// The template one row deploys: the sibling topic, and the custom resource the row's token backs,
/// with one output per value the stack publishes.
fn template(row: &Row, service_token: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            SIBLING: {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": row.topic_name },
            },
            CUSTOM: {
                "Type": "AWS::CloudFormation::CustomResource",
                // The sibling is created FIRST, so a row whose call is refused has one already
                // created resource for the rollback to remove — and the row whose token is that
                // topic names a topic that is really there.
                "DependsOn": [SIBLING],
                "Properties": {
                    "ServiceToken": service_token,
                    "Marker": row.marker,
                },
            },
        },
        "Outputs": {
            "ProviderRef": {"Value": {"Ref": CUSTOM}},
            "AnsweredMarker": {"Value": {"Fn::GetAtt": [CUSTOM, ANSWERED_DATA_KEY]}},
            "SiblingTopic": {"Value": {"Ref": SIBLING}},
        },
    })
    .to_string()
}

/// Stand one row up and deploy it: the provider function, the service role, and the `CreateStack`
/// that must be ACCEPTED — the decision about the provider call is the resource's, not the
/// request's. Answers the ARN of the token the custom resource names and the ARN of the service
/// role the stack deploys under, where the row names one.
async fn deploy(router: &MultiServiceRouter, row: &Row) -> (String, Option<String>) {
    let case = row.case;
    let function_arn = given_provider(router, case, row).await;
    let service_token = match row.token {
        Token::Function => function_arn,
        Token::Topic => topic_arn(row),
    };
    let role_arn = given_service_role(router, case, row, &service_token).await;
    let extra: Vec<(String, String)> = role_arn
        .iter()
        .map(|arn| member("RoleARN", arn))
        .collect();
    let (code, created) =
        create_stack(router, row.stack, &template(row, &service_token), &extra).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must be accepted — the decision about the provider call is \
         the resource's — got {code} {created}"
    );
    (service_token, role_arn)
}

/// Whether Amazon SNS holds the topic `arn` names right now.
async fn topic_is_held(router: &MultiServiceRouter, arn: &str) -> (u16, String) {
    sns(router, "GetTopicAttributes", &[("TopicArn", arn)]).await
}

/// How long a condition another part of the emulator owes is waited for, and how often it is
/// looked at: a provider's log is delivered by the run that wrote it, so what a case waits for is
/// the line appearing rather than a span passing.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
const POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Look again at the provider's log group until it carries a line holding `needle`, and answer
/// every line it then holds.
async fn logged_until(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    needle: &str,
) -> Vec<String> {
    let giving_up_at = std::time::Instant::now() + DEADLINE;
    loop {
        let held = logged_lines(router, group).await;
        if held.iter().any(|line| line.contains(needle)) {
            return held;
        }
        assert!(
            std::time::Instant::now() < giving_up_at,
            "case {case:?}: expected the provider's log {group:?} to carry {needle:?}, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_role_that_does_not_allow_the_provider_call_fails_the_resource_and_rolls_back() {
    for row in REFUSED {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let (service_token, role_arn) = deploy(&router, row).await;
        let role_arn = role_arn.unwrap_or_else(|| {
            panic!("case {case:?}: every refused row deploys under a service role")
        });

        // 1. The resource failed on the call that was never made, with the sentence AWS refuses
        // an unauthorized call with: the principal the stack's calls are made as, the action
        // reaching the provider performs, the ARN the token names, and which of IAM's rules
        // withheld the grant.
        let recorded = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            CUSTOM,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(case, &recorded);
        let action = row.token.action();
        let clause = match row.refused_with {
            Some(Clause::ExplicitDeny) => {
                ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
            }
            _ => ::service_common::iam_authorization::no_identity_policy_allows(action),
        };
        let expected = ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &session_principal(&role_arn, row.role_name),
            action,
            &service_token,
            &clause,
        );
        assert_eq!(
            quoted, expected,
            "case {case:?}: expected the refusal {expected:?}, got {quoted:?} out of {recorded:?}"
        );
        assert_eq!(
            class, ACCESS_DENIED_CLASS,
            "case {case:?}: expected the refusal to be classified as a call that was not \
             permitted, got {class:?} out of {recorded:?}"
        );

        // 2. The create rolled back, and the sibling the deployment had already created is gone.
        let body = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the create to roll back, got {body}"
        );
        let arn = topic_arn(row);
        let (code, attributes) = topic_is_held(&router, &arn).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected the rollback to remove the sibling topic {arn:?}, got \
             {code} {attributes}"
        );

        // 3. ...and the provider was never reached: nothing its own code would have printed was
        // logged. The positive control in this same suite is what makes that absence evidence —
        // the same provider, created the same way under the same log group, logs that line on
        // every request it IS sent.
        let logged = logged_lines(&router, &format!("/aws/lambda/{}", row.function_name)).await;
        assert!(
            !logged.iter().any(|line| line.contains(INVOKED_PREFIX)),
            "case {case:?}: expected the refused call to reach no provider, got {logged:?}"
        );
    }
}

#[tokio::test]
async fn a_role_that_allows_the_provider_call_reaches_it_and_publishes_what_it_answered() {
    for row in PERMITTED {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let _deployed = deploy(&router, row).await;

        let body = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the stack to complete on the provider's answer, got {body}"
        );

        // Every declared output is published with the value its expression resolved to — the
        // provider's own answer included, which is what a client reads a stack's handoff out of.
        let outputs = wire_outputs(&body);
        for output in &outputs {
            assert!(
                output.value.is_some(),
                "case {case:?}: expected every output to carry its value, got {output:?} in \
                 {outputs:?}"
            );
        }
        let answered = output_for(&outputs, "ProviderRef").and_then(|out| out.value.clone());
        assert_eq!(
            answered.as_deref(),
            Some(ANSWERED_ID),
            "case {case:?}: expected Ref to resolve to the id the provider answered, got \
             {outputs:?}"
        );
        let marker = output_for(&outputs, "AnsweredMarker").and_then(|out| out.value.clone());
        assert_eq!(
            marker.as_deref(),
            Some(row.marker),
            "case {case:?}: expected Fn::GetAtt to resolve to the Data the provider answered, \
             got {outputs:?}"
        );
        let published = output_for(&outputs, "SiblingTopic").and_then(|out| out.value.clone());
        assert_eq!(
            published.as_deref(),
            Some(topic_arn(row).as_str()),
            "case {case:?}: expected the sibling's own handle to be published, got {outputs:?}"
        );

        // ...and the sibling and the provider are both really there: Amazon SNS answers for the
        // topic, and the provider's own log carries the `Create` it was sent.
        let arn = topic_arn(row);
        let (code, attributes) = topic_is_held(&router, &arn).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected Amazon SNS to hold the sibling topic {arn:?}, got {code} \
             {attributes}"
        );
        // The provider's own log says it was really sent the `Create`: the wait ends as soon as
        // the line the run printed is readable.
        logged_until(
            &router,
            case,
            &format!("/aws/lambda/{}", row.function_name),
            &format!("{INVOKED_PREFIX}Create"),
        )
        .await;
    }
}
