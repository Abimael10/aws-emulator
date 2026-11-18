// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of an Amazon SNS `Publish`, across the two services that hold the
//! documents the decision is made from: IAM, where the calling principal's identity-based
//! policies live, and SNS, where the topic's own access policy lives.
//!
//! THE RULE UNDER TEST: a signed `Publish` addressed to a topic is authorized against the
//! UNION of the caller's identity-based policies and that topic's access policy. An explicit
//! `Deny` in either document refuses whatever the other allows; otherwise an `Allow` about
//! the action on that topic in EITHER document is enough; and with no grant anywhere the call
//! falls to IAM's default deny and is refused with the error the model declares for it —
//! `AuthorizationErrorException`, HTTP 403 with the awsQuery code `AuthorizationError`
//! (`smithy_query(query="operation", operation="Publish")`) — carrying IAM's own sentence,
//! whose variable parts are the principal the signing credential speaks for and the topic ARN
//! the request named. A topic whose owner has set no policy carries the one SNS generates,
//! which grants a directly signed call nothing. A served `Publish` answers a message id: "a
//! unique identifier assigned to the published message" of at most 100 characters
//! (`smithy_query(query="doc", shape="PublishResponse", member="MessageId")`), which AWS
//! answers in the 8-4-4-4-12 hexadecimal UUID form.
//!
//! Only a CROSS-SERVICE suite can pin this rule: the principals and their documents are made
//! with IAM's and STS's OWN operations over the same engine (`CreateUser` +
//! `CreateAccessKey` + `PutUserPolicy`, `CreateRole` + `PutRolePolicy` + `AssumeRole`), so
//! the identity-based documents SNS decides on are the ones IAM wrote, and the principal a
//! refusal blames is the identity STS reports for the credential that signed.
//!
//! The rows differ along what the rule generalises over: which REQUEST MEMBER names the topic
//! (`TopicArn`, and `TargetArn`, which "is returned when making a call with the
//! CreatePlatformEndpoint action" but takes a topic ARN just as well — a publish addressed by
//! either reaches the same topic, so the same documents decide it), which document carries the
//! grant, whether either document carries an explicit deny, and the FORM of the credential
//! that signs (a long-term access key of an IAM user, whose documents are the user's; the
//! temporary credential of an assumed-role session, whose documents are the role's).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every call below is addressed to, so the topics the decisions are about are the
/// ones this Region's endpoint holds.
const REGION: &str = "us-east-1";

/// The awsQuery `Version` of each API driven here, as each model's service version states it.
const SNS_VERSION: &str = "2010-03-31";
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The access-key id standing for the account's own published credential: neither IAM nor STS
/// holds a record for it, so it speaks for the account itself, which carries no IAM policy —
/// the position AWS account root credentials are in. Every setup call is signed with it.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The IAM action a `Publish` performs, as SNS's policy actions and its refusals spell it.
const PUBLISH_ACTION: &str = "SNS:Publish";

/// The status and awsQuery error code SNS refuses an unauthorized call with.
const AUTHORIZATION_ERROR: (u16, &str) = (403, "AuthorizationError");

/// The `Id` the access policy SNS generates for a topic whose owner has set none carries.
const DEFAULT_POLICY_ID: &str = "__default_policy_ID";

/// The reason clause IAM states for the default deny a call with no grant anywhere falls to:
/// within one account the permission the principal lacks is its own.
fn no_identity_policy_allows(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason clause IAM states when the resource's own policy explicitly denies the call.
const RESOURCE_POLICY_DENY: &str = "with an explicit deny in a resource-based policy";

/// The reason clause IAM states when one of the principal's own documents explicitly denies
/// the call.
const IDENTITY_POLICY_DENY: &str = "with an explicit deny in an identity-based policy";

/// The sentence AWS refuses an unauthorized request with, stated here independently of the
/// crates under test: who was refused, which action, which resource, and what withheld it.
fn not_authorized(principal: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {PUBLISH_ACTION} on resource: \
         {resource} {reason}"
    )
}

/// The emulator these rows drive: SNS beside the IAM and STS services the principals and their
/// documents are made with, on ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope, which
    // is where a service reads the caller's access key id from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` and whose key id is
/// `access_key_id` — the only part of it that matters here, because the key id is what names
/// the principal a request speaks for and this router runs with request authentication off.
fn authorization(signing_name: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// One form-encoded (awsQuery) call signed with `access_key_id`, with whatever status it
/// answered.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, access_key_id))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// One SNS call signed with `access_key_id`, with whatever status it answered.
async fn sns_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query(router, "sns", SNS_VERSION, access_key_id, action, params).await
}

/// One setup call to `signing_name`, made with the account's own credential, which must
/// succeed.
async fn setup(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        signing_name,
        version,
        ACCOUNT_KEY,
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "the setup {signing_name} {action} must succeed, got {status} with {body}"
    );
    body
}

/// A topic of this account, with its ARN — and the assertion that it carries the access policy
/// SNS generates for a topic whose owner has set none, which is the document in force on every
/// row that writes no policy of its own.
async fn given_topic(router: &MultiServiceRouter, name: &str) -> String {
    let created = setup(router, "sns", SNS_VERSION, "CreateTopic", &[("Name", name)]).await;
    let arn = element(&created, "TopicArn");
    let attributes = setup(
        router,
        "sns",
        SNS_VERSION,
        "GetTopicAttributes",
        &[("TopicArn", &arn)],
    )
    .await;
    assert!(
        attributes.contains(DEFAULT_POLICY_ID),
        "topic {name:?}: a topic whose owner set no policy must report the generated default, \
         got {attributes}"
    );
    arn
}

/// Write a topic's access policy, as its owner does.
async fn set_topic_policy(router: &MultiServiceRouter, topic: &str, document: &str) {
    setup(
        router,
        "sns",
        SNS_VERSION,
        "SetTopicAttributes",
        &[
            ("TopicArn", topic),
            ("AttributeName", "Policy"),
            ("AttributeValue", document),
        ],
    )
    .await;
}

/// An IAM user with a long-term access key: the ARN a refusal must blame it by, and the key id
/// a request carries to speak for it.
async fn given_user(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = setup(
        router,
        "iam",
        IAM_VERSION,
        "CreateUser",
        &[("UserName", name)],
    )
    .await;
    let arn = element(&created, "Arn");
    let key = setup(
        router,
        "iam",
        IAM_VERSION,
        "CreateAccessKey",
        &[("UserName", name)],
    )
    .await;
    (arn, element(&key, "AccessKeyId"))
}

/// An assumed-role session: the ROLE's own ARN (the spelling a resource policy names the
/// caller by), the ARN STS reports for the session (what a refusal blames), and the key id a
/// request carries to speak for it.
///
/// The role trusts this account's root principal, the spelling that admits every principal of
/// the account, so the setup call may assume it.
async fn given_assumed_role(router: &MultiServiceRouter, name: &str) -> (String, String, String) {
    let trust = policy(json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
        "Action": "sts:AssumeRole",
    }));
    let created = setup(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let role_arn = element(&created, "Arn");
    let assumed = setup(
        router,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", "publishing")],
    )
    .await;
    (
        role_arn,
        element(&assumed, "Arn"),
        element(&assumed, "AccessKeyId"),
    )
}

/// One policy document around one statement.
fn policy(statement: serde_json::Value) -> String {
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// Whether `value` is a message id in the 8-4-4-4-12 hexadecimal UUID form AWS answers a
/// published message's id in: 36 characters, four hyphens, hexadecimal everywhere else.
fn is_uuid_shaped(value: &str) -> bool {
    let widths = [8usize, 4, 4, 4, 12];
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == widths.len()
        && groups.iter().zip(widths).all(|(group, width)| {
            group.len() == width && group.chars().all(|digit| digit.is_ascii_hexdigit())
        })
}

/// What the documents in force must decide for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The publish is served, and answers a message id.
    Served,
    /// The publish is refused `403 AuthorizationError`, with this reason clause.
    Refused(Reason),
}

/// Which document a refusal's clause names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// No readable document granted the action.
    NoIdentityGrant,
    /// The topic's own policy explicitly denies it.
    ResourcePolicyDeny,
    /// One of the principal's own documents explicitly denies it.
    IdentityPolicyDeny,
}

impl Reason {
    fn clause(self) -> String {
        match self {
            Self::NoIdentityGrant => no_identity_policy_allows(PUBLISH_ACTION),
            Self::ResourcePolicyDeny => RESOURCE_POLICY_DENY.to_string(),
            Self::IdentityPolicyDeny => IDENTITY_POLICY_DENY.to_string(),
        }
    }
}

/// A document built from the topic's ARN and the ARN a policy names the caller by, or `None`
/// for a row that writes none.
type DocumentOf = fn(&str, &str) -> Option<String>;

/// Assert the answer to a publish is what the documents in force decide, naming `principal` in
/// a refusal and `topic` as the resource.
fn assert_decided(
    case: &str,
    verdict: Verdict,
    principal: &str,
    topic: &str,
    status: u16,
    body: &str,
) {
    match verdict {
        Verdict::Served => {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the publish to be served, got {status} with {body}"
            );
            let message_id = element(body, "MessageId");
            assert!(
                is_uuid_shaped(&message_id),
                "case {case:?}: expected a UUID message id, got {message_id:?}"
            );
        }
        Verdict::Refused(reason) => {
            let code = opt_element(body, "Code").unwrap_or_default();
            assert_eq!(
                (status, code.as_str()),
                AUTHORIZATION_ERROR,
                "case {case:?}: expected the modelled refusal, got {status} with {body}"
            );
            let message = opt_element(body, "Message").unwrap_or_default();
            assert_eq!(
                message,
                not_authorized(principal, topic, &reason.clause()),
                "case {case:?}: expected IAM's refusal wording, got {message:?}"
            );
            assert!(
                opt_element(body, "MessageId").is_none(),
                "case {case:?}: a refused publish answers no message id, got {body}"
            );
        }
    }
}

/// RULE: the union of the caller's identity-based policies and the topic's access policy
/// decides a `Publish`, with an explicit deny in either winning — whichever request member
/// named the topic.
#[tokio::test]
async fn a_publish_is_decided_by_both_documents_whichever_member_names_the_topic() {
    // (case, the user's inline identity policy, the topic's `Policy`, the verdict)
    let rows: &[(&str, DocumentOf, DocumentOf, Verdict)] = &[
        (
            "neither document grants the action",
            |_, _| None,
            |_, _| None,
            Verdict::Refused(Reason::NoIdentityGrant),
        ),
        (
            "the identity policy alone allows the action on this topic",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Publish",
                    "Resource": topic,
                })))
            },
            |_, _| None,
            Verdict::Served,
        ),
        (
            "the topic policy alone names the caller",
            |_, _| None,
            |topic, caller| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": caller},
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                })))
            },
            Verdict::Served,
        ),
        (
            "an explicit deny in the topic policy over an identity grant",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Publish",
                    "Resource": topic,
                })))
            },
            |topic, caller| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Principal": {"AWS": caller},
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                })))
            },
            Verdict::Refused(Reason::ResourcePolicyDeny),
        ),
        (
            "an explicit deny in the identity policy over a topic grant",
            |topic, _| {
                Some(policy(json!({
                    "Effect": "Deny",
                    "Action": "sns:Publish",
                    "Resource": topic,
                })))
            },
            |topic, caller| {
                Some(policy(json!({
                    "Effect": "Allow",
                    "Principal": {"AWS": caller},
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                })))
            },
            Verdict::Refused(Reason::IdentityPolicyDeny),
        ),
    ];

    // "If you don't specify a value for the TopicArn parameter, you must specify a value for
    // the PhoneNumber or TargetArn parameters" (`PublishInput$TopicArn`): a topic ARN is the
    // destination either member carries, so the same documents decide both.
    for member in ["TopicArn", "TargetArn"] {
        for (row, (case, identity_document, topic_document, verdict)) in rows.iter().enumerate() {
            let case = format!("{case} addressed by {member}");
            // One router per row: a document a row writes is in force until it is replaced, so
            // no row may inherit another's.
            let router = build_router();
            let topic = given_topic(&router, &format!("orders-{row}")).await;
            let user_name = format!("publisher-{row}");
            let (user, access_key_id) = given_user(&router, &user_name).await;
            if let Some(document) = identity_document(&topic, &user) {
                setup(
                    &router,
                    "iam",
                    IAM_VERSION,
                    "PutUserPolicy",
                    &[
                        ("UserName", &user_name),
                        ("PolicyName", "inline"),
                        ("PolicyDocument", &document),
                    ],
                )
                .await;
            }
            if let Some(document) = topic_document(&topic, &user) {
                set_topic_policy(&router, &topic, &document).await;
            }

            let (status, body) = sns_as(
                &router,
                &access_key_id,
                "Publish",
                &[(member, &topic), ("Message", "probe")],
            )
            .await;
            assert_decided(&case, *verdict, &user, &topic, status, &body);
        }
    }
}

/// RULE: the same union decides a publish signed by the temporary credential of an assumed-role
/// session — the documents in force are the ROLE's ("the temporary security credentials …
/// have the permissions of the role", `AssumeRole`), the topic's policy names the caller by the
/// role's ARN, and the principal a refusal blames is the session STS reported.
#[tokio::test]
async fn a_role_sessions_publish_is_decided_by_the_roles_documents_and_the_topic_policy() {
    let router = build_router();
    let topic = given_topic(&router, "role-published").await;
    let role_name = "publishing-role";
    let (role_arn, session_arn, access_key_id) = given_assumed_role(&router, role_name).await;
    let publish = [("TargetArn", topic.as_str()), ("Message", "probe")];

    // The session holds no document at all: IAM's default deny, blaming the session.
    let (status, body) = sns_as(&router, &access_key_id, "Publish", &publish).await;
    assert_decided(
        "a role session holding no document",
        Verdict::Refused(Reason::NoIdentityGrant),
        &session_arn,
        &topic,
        status,
        &body,
    );

    // The ROLE's own document grants the action on that topic, which the session carries.
    setup(
        &router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "inline"),
            (
                "PolicyDocument",
                &policy(json!({
                    "Effect": "Allow",
                    "Action": "sns:Publish",
                    "Resource": topic,
                })),
            ),
        ],
    )
    .await;
    let (status, body) = sns_as(&router, &access_key_id, "Publish", &publish).await;
    assert_decided(
        "a role session whose role grants the action",
        Verdict::Served,
        &session_arn,
        &topic,
        status,
        &body,
    );

    // An explicit deny in the topic's policy, naming the role: it wins over the grant the
    // role's own document gave.
    set_topic_policy(
        &router,
        &topic,
        &policy(json!({
            "Effect": "Deny",
            "Principal": {"AWS": role_arn},
            "Action": PUBLISH_ACTION,
            "Resource": topic,
        })),
    )
    .await;
    let (status, body) = sns_as(&router, &access_key_id, "Publish", &publish).await;
    assert_decided(
        "a topic deny of the role over the role's own grant",
        Verdict::Refused(Reason::ResourcePolicyDeny),
        &session_arn,
        &topic,
        status,
        &body,
    );
}

#[cfg(test)]
mod tests {
    use super::is_uuid_shaped;

    /// The message-id reader accepts the 8-4-4-4-12 hexadecimal form and nothing else, over the
    /// ways a value can miss it: a group of the wrong width, a non-hexadecimal character, too
    /// few groups, and no hyphen at all.
    #[test]
    fn the_message_id_reader_accepts_only_the_uuid_form() {
        // (case, the value, whether it is a UUID)
        let rows: [(&str, &str, bool); 6] = [
            ("a lowercase UUID", "1b4e28ba-2fa1-11d2-883f-0016d3cca427", true),
            ("an uppercase UUID", "1B4E28BA-2FA1-11D2-883F-0016D3CCA427", true),
            (
                "a group one character short",
                "1b4e28b-2fa1-11d2-883f-0016d3cca427",
                false,
            ),
            (
                "a non-hexadecimal character",
                "1b4e28bz-2fa1-11d2-883f-0016d3cca427",
                false,
            ),
            ("four groups", "1b4e28ba-2fa1-11d2-883f", false),
            ("36 characters with no hyphen", &"a".repeat(36), false),
        ];

        for (case, value, expected) in rows {
            let read = is_uuid_shaped(value);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected}, got {read} for {value:?}"
            );
        }
    }
}
