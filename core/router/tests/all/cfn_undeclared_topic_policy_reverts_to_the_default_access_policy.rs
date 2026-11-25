// SPDX-License-Identifier: CC0-1.0

//! What a topic's access policy reads as, and what it authorizes, once a CloudFormation
//! deployment STOPS declaring a policy resource for that topic.
//!
//! THE RULE UNDER TEST: **a stack-declared topic access policy is not a resource of Amazon
//! SNS's own — it IS the `Policy` attribute of the topics its declaration names — so a
//! deployment that no longer declares it for a topic leaves that topic carrying the access
//! policy Amazon SNS generates for a topic whose owner has set none, and the grant the
//! withdrawn document made is no longer in force.** The generated document carries exactly one
//! statement, whose `Sid` is the default statement id, and it grants a directly signed call
//! nothing, so a `Publish` by the principal the withdrawn document had named falls to IAM's
//! default deny: HTTP 403 with the awsQuery code `AuthorizationError`, the class the model
//! declares for an unauthorized call (`smithy_query(query="operation", operation="Publish",
//! model_path="services/service-com-amazonaws-sns-amazonsimplenotificationservice/smithy-model/\
//! com-amazonaws-sns-amazonsimplenotificationservice.json")`), carrying IAM's own sentence —
//! whose variable parts are the principal the signing credential speaks for and the topic ARN
//! the request named.
//!
//! Three services have to agree for that to hold, which is why this suite lives here rather
//! than in either crate: CloudFormation deploys and withdraws the declaration, IAM and STS vend
//! the principal it grants, and Amazon SNS both reports the attribute and decides every later
//! publish. This is the observation a client makes when it revokes a grant through
//! infrastructure: nothing else distinguishes a revocation that landed from one that did not.
//!
//! Overlap, stated rather than hidden: the sibling `cloudformation_sns_topic_policy_admits_s3_notifications`
//! suite holds the DEPLOYMENT half of the same rule — the declared document reaching the topic
//! and deciding a call made from it — and `sns_publish_authorization_spans_iam_and_the_topic_policy`
//! holds the authorization rule over documents a client writes directly. What only this suite
//! reaches is the WITHDRAWAL: what the topic reports, and what it authorizes, once a deployment
//! stops declaring the policy at all.
//!
//! The rows differ along what the rule generalises over: which resource TYPE carries the
//! document and therefore which property names the topic (`AWS::SNS::TopicInlinePolicy`, whose
//! `TopicArn` names one topic, and `AWS::SNS::TopicPolicy`, whose `Topics` names a list), the
//! FORM the declaration's `Resource` takes (one ARN, a list of them), and HOW the deployment
//! stops declaring the policy for a topic — the resource removed from the template altogether,
//! and a surviving declaration revised to name one of the two topics it used to, which is also
//! the control that the revert is the declaration's doing and not an update's.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The whole emulator over one engine, as the binary assembles it — declared `pub(crate)` by the
// suite that owns it, so no suite builds a second one.
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router;
use crate::query_wire::{decoded_entries, element, form_encode, opt_element, outputs_of};

/// The Region every call here is addressed to, and so the Region the stack, its topics and its
/// role belong to.
const REGION: &str = "us-east-1";

/// The awsQuery `Version` of each API driven here, as each model's service version states it.
const CFN_VERSION: &str = "2010-05-15";
const SNS_VERSION: &str = "2010-03-31";
const STS_VERSION: &str = "2011-06-15";

/// The access key id the stack owner's calls present: the account's own published credential,
/// which no IAM record stands for, so it speaks for the account itself.
const OWNER_KEY: &str = "AKIDEXAMPLE";

/// The `Sid` and `Id` of the access policy Amazon SNS generates for a topic whose owner has set
/// none — the document a topic that carries no customer-supplied policy reports.
const DEFAULT_STATEMENT_ID: &str = "__default_statement_ID";
const DEFAULT_POLICY_ID: &str = "__default_policy_ID";

/// The `Sid` the declaration under test labels its own statement with, which is what makes a
/// withdrawn grant distinguishable from the generated document.
const DECLARED_SID: &str = "DeploymentGrantsItsRole";

/// The IAM action a `Publish` performs, as SNS's policy actions and its refusals spell it.
const PUBLISH_ACTION: &str = "SNS:Publish";

/// The status and awsQuery error code SNS refuses an unauthorized call with.
const AUTHORIZATION_ERROR: (u16, &str) = (403, "AuthorizationError");

/// The statuses a stack operation reports when it is over, and the suffix every status of one
/// still running carries.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// How long a wait here runs before the case fails, and how often it looks again.
///
/// Every wait is for work ANOTHER service performs — a stack's deployment and its update, each
/// with the span the clock owes it — so the budget is far above anything that flow takes, on a
/// host running the whole workspace's tests at once included. No case waits for a span and none
/// is about how fast a deployment settles: each re-reads until what it is after is there, or
/// this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` whose credential scope names `signing_name` — which is what routes
/// the request — and carries the access key id the called service resolves the caller from. The
/// signature is never checked: the router runs with request authentication off.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call presenting `access_key_id`, with whatever status it answered.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call, made with the owner's own credential, which must be accepted.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, String)],
) -> String {
    let (status, body) = query(
        router,
        OWNER_KEY,
        "cloudformation",
        CFN_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// Read the stack until it reports a status that is no longer in progress, answering the whole
/// `DescribeStacks` answer it reported then.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let started = Instant::now();
    loop {
        let body = cloudformation(
            router,
            case,
            "DescribeStacks",
            &[("StackName", stack.to_string())],
        )
        .await;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with(IN_PROGRESS_SUFFIX) {
            return body;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the stack to reach a terminal status within {BUDGET:?}, it \
             is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Deploy `template` with `action` (`CreateStack` or `UpdateStack`), wait for the stack to
/// settle, assert it reached `expected`, and answer its settled `DescribeStacks` answer.
///
/// `CAPABILITY_NAMED_IAM` is stated because the template declares an IAM role under a name of
/// its own, which is the capability CloudFormation requires for that.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    expected: &str,
    stack: &str,
    template: &Value,
) -> String {
    cloudformation(
        router,
        case,
        action,
        &[
            ("StackName", stack.to_string()),
            ("TemplateBody", template.to_string()),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM".to_string()),
        ],
    )
    .await;
    let described = settled_stack(router, case, stack).await;
    let reached = element(&described, "StackStatus");
    assert_eq!(
        reached, expected,
        "case {case:?}: expected {action} to reach {expected}, got {reached:?} with {described}"
    );
    described
}

/// One output of a settled stack, which a case needs to address what the stack made.
fn output(described: &str, case: &str, key: &str) -> String {
    outputs_of(described)
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        .clone()
}

/// The access policy the topic reports NOW, as the document it is: the `Policy` entry of
/// `GetTopicAttributes`, "the JSON serialization of the topic's access control policy".
async fn reported_policy(router: &MultiServiceRouter, case: &str, topic: &str) -> Value {
    let (status, body) = query(
        router,
        OWNER_KEY,
        "sns",
        SNS_VERSION,
        "GetTopicAttributes",
        &[("TopicArn", topic.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetTopicAttributes, got {status} with {body}"
    );
    let document = decoded_entries(&body)
        .get("Policy")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the topic reports no Policy at all: {body}"));
    serde_json::from_str(&document).unwrap_or_else(|_| {
        panic!("case {case:?}: the reported Policy must be a document, got {document:?}")
    })
}

/// The `Sid` of every statement of a policy document, in order — the list a client watches a
/// topic's access policy by. A statement carrying none contributes the empty string, so a
/// document that lost its labels is a mismatch rather than an empty list.
fn statement_sids(document: &Value) -> Vec<String> {
    document
        .get("Statement")
        .and_then(Value::as_array)
        .map(|statements| {
            statements
                .iter()
                .map(|statement| {
                    statement
                        .get("Sid")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read the topic's access policy until its statement `Sid` list is `expected`, answering the
/// document it reported then.
async fn policy_whose_sids_are(
    router: &MultiServiceRouter,
    case: &str,
    topic: &str,
    expected: &[&str],
) -> Value {
    let started = Instant::now();
    loop {
        let document = reported_policy(router, case, topic).await;
        let sids = statement_sids(&document);
        if sids == expected {
            return document;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the topic's policy statements to become {expected:?} within \
             {BUDGET:?}, they are {sids:?} in {document}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assume the role at `role_arn`, answering the ARN STS reports for the session — the principal
/// a refusal blames — and the access key id the session signs with.
async fn assumed_session(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
) -> (String, String) {
    let (status, body) = query(
        router,
        OWNER_KEY,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[
            ("RoleArn", role_arn.to_string()),
            ("RoleSessionName", "revocation-probe".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from AssumeRole, got {status} with {body}"
    );
    (element(&body, "Arn"), element(&body, "AccessKeyId"))
}

/// One `Publish` to `topic` signed by `access_key_id`, with whatever status it answered.
async fn publish(
    router: &MultiServiceRouter,
    access_key_id: &str,
    topic: &str,
) -> (u16, String) {
    query(
        router,
        access_key_id,
        "sns",
        SNS_VERSION,
        "Publish",
        &[
            ("TopicArn", topic.to_string()),
            ("Message", "through the deployment's grant".to_string()),
        ],
    )
    .await
}

/// The sentence AWS refuses an unauthorized request with, stated here independently of the
/// crates under test: who was refused, which action, which resource, and what withheld it —
/// which for a principal no document grants is its own account's missing permission.
fn not_authorized(principal: &str, topic: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {PUBLISH_ACTION} on resource: {topic} \
         because no identity-based policy allows the {PUBLISH_ACTION} action"
    )
}

/// Assert the session's publish to `topic` is SERVED, which is what makes a later refusal about
/// the withdrawn grant.
async fn assert_publish_served(
    router: &MultiServiceRouter,
    case: &str,
    session_key: &str,
    topic: &str,
) {
    let (status, body) = publish(router, session_key, topic).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the granted session's publish to {topic} must be served, got {status} \
         with {body}"
    );
    assert!(
        opt_element(&body, "MessageId").is_some(),
        "case {case:?}: a served publish answers a message id, got {body}"
    );
}

/// Assert the session's publish to `topic` is refused with the modelled class and AWS's own
/// sentence, naming the session STS reported and the topic the request named.
async fn assert_publish_refused(
    router: &MultiServiceRouter,
    case: &str,
    session_key: &str,
    session_arn: &str,
    topic: &str,
) {
    let (status, body) = publish(router, session_key, topic).await;
    let code = opt_element(&body, "Code").unwrap_or_default();
    assert_eq!(
        (status, code.as_str()),
        AUTHORIZATION_ERROR,
        "case {case:?}: expected the modelled refusal of the publish to {topic}, got {status} \
         with {body}"
    );
    let message = opt_element(&body, "Message").unwrap_or_default();
    assert_eq!(
        message,
        not_authorized(session_arn, topic),
        "case {case:?}: expected IAM's refusal wording, got {message:?}"
    );
    assert!(
        opt_element(&body, "MessageId").is_none(),
        "case {case:?}: a refused publish answers no message id, got {body}"
    );
}

/// The trust policy that lets this account assume the role the template declares.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The document a policy resource declares: one labelled statement granting the stack's own role
/// `sns:Publish` on the topics `resource` names, written as intrinsics so the deployed document
/// carries the real ARNs rather than the template's references.
fn grant(resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": DECLARED_SID,
            "Effect": "Allow",
            "Principal": {"AWS": {"Fn::GetAtt": ["Role", "Arn"]}},
            "Action": "sns:Publish",
            "Resource": resource,
        }],
    })
}

/// The role every template here declares, under a name of its own so a session can be assumed
/// into it by ARN.
fn role(name: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {"RoleName": name, "AssumeRolePolicyDocument": trust_policy()},
    })
}

/// One row of the type table: the case, the resource type that carries the document, the
/// property that names the topic, and whether that property takes a LIST of topic ARNs.
struct Row {
    case: &'static str,
    resource_type: &'static str,
    topic_property: &'static str,
    names_a_list: bool,
}

/// The two resource types a template declares a topic's access policy with. Both are the topic's
/// `Policy` attribute; they differ only in which property carries the topic, so each is driven
/// through the whole grant-and-revoke flow.
const ROWS: &[Row] = &[
    Row {
        case: "an inline policy, which names the one topic it belongs to",
        resource_type: "AWS::SNS::TopicInlinePolicy",
        topic_property: "TopicArn",
        names_a_list: false,
    },
    Row {
        case: "a topic policy, which names a list of topics",
        resource_type: "AWS::SNS::TopicPolicy",
        topic_property: "Topics",
        names_a_list: true,
    },
];

/// RULE: a deployment that stops declaring a topic's access policy leaves the topic reporting the
/// document Amazon SNS generates for a topic whose owner set none, and the grant it had made is
/// no longer in force — for either resource type that carries such a document.
#[tokio::test]
async fn a_withdrawn_policy_resource_leaves_the_topic_on_its_generated_default_policy() {
    for (index, row) in ROWS.iter().enumerate() {
        // One emulator per row: a document a row deploys is in force until it is withdrawn, so
        // no row may inherit another's topics, role or policy.
        let router = router();
        let case = row.case;
        let stack = format!("sns-policy-withdrawal-{index}");
        let role_name = format!("sns-policy-withdrawal-{index}-role");
        let topic_name = format!("sns-policy-withdrawal-{index}-topic");

        // The `Resource` of the declared statement takes the form the row names, so neither the
        // single-ARN nor the list spelling is the one the rule is read off.
        let resource = if row.names_a_list {
            json!([{"Ref": "Topic"}])
        } else {
            json!({"Ref": "Topic"})
        };
        let topics = if row.names_a_list {
            json!([{"Ref": "Topic"}])
        } else {
            json!({"Ref": "Topic"})
        };
        let template = |declared: bool| {
            let mut resources = json!({
                "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": topic_name}},
                "Role": role(&role_name),
            });
            if declared {
                let mut properties = serde_json::Map::new();
                properties.insert(row.topic_property.to_string(), topics.clone());
                properties.insert("PolicyDocument".to_string(), grant(resource.clone()));
                resources["Policy"] = json!({
                    "Type": row.resource_type,
                    "Properties": Value::Object(properties),
                });
            }
            json!({
                "Resources": resources,
                "Outputs": {
                    "TopicArn": {"Value": {"Ref": "Topic"}},
                    "RoleArn": {"Value": {"Fn::GetAtt": ["Role", "Arn"]}},
                },
            })
        };

        let described = deployed(
            &router,
            case,
            "CreateStack",
            CREATE_COMPLETE,
            &stack,
            &template(true),
        )
        .await;
        let topic = output(&described, case, "TopicArn");
        let role_arn = output(&described, case, "RoleArn");

        // The declared document really landed on the topic, rendered: its own statement, under
        // the label the template gave it, naming the role the stack created.
        let deployed_document = policy_whose_sids_are(&router, case, &topic, &[DECLARED_SID]).await;
        assert_eq!(
            deployed_document["Statement"][0]["Principal"]["AWS"],
            json!(role_arn),
            "case {case:?}: the deployed document must name the stack's role, got \
             {deployed_document}"
        );

        // And the grant is real, which is what makes the refusal below about its withdrawal.
        let (session_arn, session_key) = assumed_session(&router, case, &role_arn).await;
        assert_publish_served(&router, case, &session_key, &topic).await;

        // The same template, minus the policy resource.
        deployed(
            &router,
            case,
            "UpdateStack",
            UPDATE_COMPLETE,
            &stack,
            &template(false),
        )
        .await;

        let default_document =
            policy_whose_sids_are(&router, case, &topic, &[DEFAULT_STATEMENT_ID]).await;
        assert_eq!(
            default_document["Id"],
            json!(DEFAULT_POLICY_ID),
            "case {case:?}: the topic must carry the document SNS generates, got \
             {default_document}"
        );
        assert_ne!(
            default_document["Statement"][0]["Principal"]["AWS"],
            json!(role_arn),
            "case {case:?}: the generated document must not name the withdrawn grant's role, got \
             {default_document}"
        );
        assert_publish_refused(&router, case, &session_key, &session_arn, &topic).await;
    }
}

/// RULE, on the same declaration rather than on its removal: a policy resource that stops naming
/// one of the topics it used to leaves THAT topic on the generated default and revokes its grant,
/// while the topic it still names keeps the document and the grant — so the revert follows the
/// declaration and not the fact that an update ran.
#[tokio::test]
async fn a_revised_declaration_reverts_only_the_topic_it_stopped_naming() {
    let router = router();
    let case = "a topic policy revised to name one of its two topics";
    let stack = "sns-policy-narrowing";
    let role_name = "sns-policy-narrowing-role";

    // The declared `Resource` names BOTH topics in either revision, so what changes between them
    // is only which topics the declaration APPLIES to.
    let document = grant(json!([{"Ref": "Kept"}, {"Ref": "Dropped"}]));
    let template = |topics: Value| {
        json!({
            "Resources": {
                "Kept": {
                    "Type": "AWS::SNS::Topic",
                    "Properties": {"TopicName": "sns-policy-narrowing-kept"},
                },
                "Dropped": {
                    "Type": "AWS::SNS::Topic",
                    "Properties": {"TopicName": "sns-policy-narrowing-dropped"},
                },
                "Role": role(role_name),
                "Policy": {
                    "Type": "AWS::SNS::TopicPolicy",
                    "Properties": {"Topics": topics, "PolicyDocument": document.clone()},
                },
            },
            "Outputs": {
                "KeptArn": {"Value": {"Ref": "Kept"}},
                "DroppedArn": {"Value": {"Ref": "Dropped"}},
                "RoleArn": {"Value": {"Fn::GetAtt": ["Role", "Arn"]}},
            },
        })
    };

    let described = deployed(
        &router,
        case,
        "CreateStack",
        CREATE_COMPLETE,
        stack,
        &template(json!([{"Ref": "Kept"}, {"Ref": "Dropped"}])),
    )
    .await;
    let kept = output(&described, case, "KeptArn");
    let dropped = output(&described, case, "DroppedArn");
    let role_arn = output(&described, case, "RoleArn");
    let (session_arn, session_key) = assumed_session(&router, case, &role_arn).await;

    for topic in [&kept, &dropped] {
        policy_whose_sids_are(&router, case, topic, &[DECLARED_SID]).await;
        assert_publish_served(&router, case, &session_key, topic).await;
    }

    deployed(
        &router,
        case,
        "UpdateStack",
        UPDATE_COMPLETE,
        stack,
        &template(json!([{"Ref": "Kept"}])),
    )
    .await;

    // The topic the declaration no longer names is back on the generated default, and the grant
    // it carried is gone with it.
    policy_whose_sids_are(&router, case, &dropped, &[DEFAULT_STATEMENT_ID]).await;
    assert_publish_refused(&router, case, &session_key, &session_arn, &dropped).await;

    // The topic it still names kept both.
    policy_whose_sids_are(&router, case, &kept, &[DECLARED_SID]).await;
    assert_publish_served(&router, case, &session_key, &kept).await;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::statement_sids;

    /// The statement reader answers one entry per statement, in document order, over the ways a
    /// document can carry them: several labelled statements, one, a statement carrying no label
    /// at all, and a document with no statement list.
    #[test]
    fn the_statement_reader_answers_one_entry_per_statement() {
        // (case, the document, the expected labels)
        let rows: &[(&str, serde_json::Value, Vec<&str>)] = &[
            (
                "two labelled statements",
                json!({"Statement": [{"Sid": "first"}, {"Sid": "second"}]}),
                vec!["first", "second"],
            ),
            (
                "one labelled statement",
                json!({"Statement": [{"Sid": "only"}]}),
                vec!["only"],
            ),
            (
                "a statement carrying no label",
                json!({"Statement": [{"Effect": "Allow"}]}),
                vec![""],
            ),
            ("no statement list at all", json!({"Id": "empty"}), vec![]),
        ];

        for (case, document, expected) in rows {
            let read = statement_sids(document);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?} for {document}"
            );
        }
    }
}
