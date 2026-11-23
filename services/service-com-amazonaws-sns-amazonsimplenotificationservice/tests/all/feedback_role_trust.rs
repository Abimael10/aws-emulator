// SPDX-License-Identifier: CC0-1.0

//! THE TRUST RELATIONSHIP a delivery-status feedback role has to carry before SNS stores it,
//! driven end-to-end over the awsQuery wire with IAM mounted on the same engine — so the trust
//! document SNS reads is the one IAM wrote.
//!
//! The RULE asserted here: "The <ENDPOINT>SuccessFeedbackRoleArn and
//! <ENDPOINT>FailureFeedbackRoleArn attributes are used to give Amazon SNS write access to use
//! CloudWatch Logs on your behalf" (`SetTopicAttributesInput$AttributeName` doc) — the value
//! names an IAM role Amazon SNS ASSUMES FOR THE TOPIC being configured, so a write of one is
//! refused `400 InvalidParameter` when the role this account's IAM holds under that name carries
//! a trust policy that does not grant `sns.amazonaws.com` that assume, and the attribute does NOT
//! land on the topic. The assume is made for one topic and carries the keys a trust statement may
//! be narrowed by — `aws:SourceArn` naming the topic, `aws:SourceAccount` naming its owner — so a
//! statement's `Condition` is part of its grant: a role scoped to one topic is accepted on that
//! topic and refused on a sibling. A role whose trust policy grants the assume is accepted and
//! reads back. The rule is the same whichever call carried the write, so `CreateTopic` refuses the
//! same value and leaves no topic behind.
//!
//! The rows differ along what the rule generalises over: which of the ten feedback-role
//! attributes the write carried (the catalog of the doc above), what the role's trust policy
//! says — this service named alone, among several, through an action wildcard, the account root
//! instead, another service, an explicit deny beside the allow, and another STS action only —
//! plus a role this account's IAM holds nothing under, which is no evidence about a role
//! provisioned out of band and is therefore not refused; and, for the conditioned grant, the
//! operator the clause is written under, which key it is over, which topic or account it names,
//! negation, several clauses at once, and a key the assume carries no value for.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::create_platform_application::{attribute_params, credentials};
use super::topics_subscriptions_publish::{attribute_map, percent_encode, Answer};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The awsQuery `Version` of the SNS API, as this service's model states it.
const SNS_VERSION: &str = "2010-03-31";

/// The awsQuery `Version` of the IAM API.
const IAM_VERSION: &str = "2010-05-08";

/// The service principal a feedback role has to admit for Amazon SNS to assume it.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// Every attribute of a topic that names a delivery-status feedback role: the per-protocol
/// `<Protocol>SuccessFeedbackRoleArn` / `<Protocol>FailureFeedbackRoleArn` pairs of the
/// `SetTopicAttributesInput$AttributeName` doc, whose protocol prefixes are `HTTP`, `Firehose`,
/// `Lambda`, `Application` and `SQS`.
const FEEDBACK_ROLE_ATTRIBUTES: &[&str] = &[
    "ApplicationFailureFeedbackRoleArn",
    "ApplicationSuccessFeedbackRoleArn",
    "FirehoseFailureFeedbackRoleArn",
    "FirehoseSuccessFeedbackRoleArn",
    "HTTPFailureFeedbackRoleArn",
    "HTTPSuccessFeedbackRoleArn",
    "LambdaFailureFeedbackRoleArn",
    "LambdaSuccessFeedbackRoleArn",
    "SQSFailureFeedbackRoleArn",
    "SQSSuccessFeedbackRoleArn",
];

/// Every attribute of a PLATFORM APPLICATION that names a delivery-status feedback role: the
/// pair `SetPlatformApplicationAttributesInput$Attributes` documents with the same sentence the
/// topic's attributes carry ("`SuccessFeedbackRoleArn` – IAM role ARN used to give Amazon SNS
/// write access to use CloudWatch Logs on your behalf", and the same for
/// `FailureFeedbackRoleArn`). The application's names carry no protocol prefix: the
/// application IS the kind of delivery they are about.
const APPLICATION_FEEDBACK_ROLE_ATTRIBUTES: &[&str] =
    &["FailureFeedbackRoleArn", "SuccessFeedbackRoleArn"];

/// The push platform the applications registered here are for, whose documented credential
/// material the sibling suite's fixture builds.
const PLATFORM: &str = "GCM";

/// Another service's principal, which a role names instead of this service's: it is THIS
/// service's principal, and not merely some service principal, that decides the assume.
const OTHER_SERVICE_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The emulator under test: one shared engine carrying SNS beside the IAM service the feedback
/// roles are made with.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here signs: the `Authorization` header carries the credential scope only,
        // which is what routes the request and names the Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One awsQuery call, as the answered response.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, &str)],
    ) -> Answer {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&percent_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.amazonaws.com/"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let request_id_header = response
            .headers()
            .get("x-amzn-requestid")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Answer {
            status,
            body: String::from_utf8_lossy(response.body()).to_string(),
            request_id_header,
        }
    }

    /// One SNS call.
    async fn sns(&self, action: &str, params: &[(&str, &str)]) -> Answer {
        self.query("sns", SNS_VERSION, action, params).await
    }

    /// One IAM fixture call, which must succeed.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> Answer {
        let answer = self.query("iam", IAM_VERSION, action, params).await;
        assert_eq!(
            answer.status, 200,
            "the fixture {action} must succeed, got {} / {}",
            answer.status, answer.body
        );
        answer
    }

    /// A topic, with its ARN.
    async fn topic(&self, name: &str) -> String {
        let answer = self.sns("CreateTopic", &[("Name", name)]).await;
        assert_eq!(
            answer.status, 200,
            "the fixture CreateTopic {name} must succeed, got {} / {}",
            answer.status, answer.body
        );
        answer
            .element("TopicArn")
            .unwrap_or_else(|| panic!("CreateTopic {name} answers a TopicArn, got {}", answer.body))
    }

    /// A role IAM holds under `name` with the trust policy `document`, and its ARN.
    async fn role(&self, name: &str, document: &str) -> String {
        let answer = self
            .iam(
                "CreateRole",
                &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
            )
            .await;
        answer
            .element("Arn")
            .unwrap_or_else(|| panic!("CreateRole {name} answers an Arn, got {}", answer.body))
    }

    /// Write one topic attribute.
    async fn set_attribute(&self, topic: &str, name: &str, value: &str) -> Answer {
        self.sns(
            "SetTopicAttributes",
            &[
                ("TopicArn", topic),
                ("AttributeName", name),
                ("AttributeValue", value),
            ],
        )
        .await
    }

    /// The attribute map a topic reports.
    async fn attributes(&self, topic: &str) -> std::collections::HashMap<String, String> {
        let answer = self.sns("GetTopicAttributes", &[("TopicArn", topic)]).await;
        assert_eq!(
            answer.status, 200,
            "GetTopicAttributes {topic} must succeed, got {} / {}",
            answer.status, answer.body
        );
        attribute_map(&answer)
    }

    /// Create a topic carrying ONE attribute, which is the other path a feedback role is
    /// written by.
    async fn create_topic_with(&self, name: &str, attribute: &str, value: &str) -> Answer {
        self.sns(
            "CreateTopic",
            &[
                ("Name", name),
                ("Attributes.entry.1.key", attribute),
                ("Attributes.entry.1.value", value),
            ],
        )
        .await
    }

    /// Register a platform application of [`PLATFORM`] under `name`, carrying the credential
    /// material that platform documents and the attribute `extra` when a case names one;
    /// answers the response as it came back.
    async fn create_application_with(
        &self,
        name: &str,
        extra: Option<(&str, &str)>,
    ) -> Answer {
        let mut attributes = credentials(PLATFORM);
        if let Some((attribute, value)) = extra {
            attributes.push((attribute.to_string(), value.to_string()));
        }
        let encoded = attribute_params(&attributes);
        let mut params: Vec<(&str, &str)> = vec![("Name", name), ("Platform", PLATFORM)];
        params.extend(
            encoded
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        self.sns("CreatePlatformApplication", &params).await
    }

    /// A registered platform application, with its ARN.
    async fn platform_application(&self, name: &str) -> String {
        let answer = self.create_application_with(name, None).await;
        assert_eq!(
            answer.status, 200,
            "the fixture CreatePlatformApplication {name} must succeed, got {} / {}",
            answer.status, answer.body
        );
        answer
            .element("PlatformApplicationArn")
            .unwrap_or_else(|| {
                panic!(
                    "CreatePlatformApplication {name} answers a PlatformApplicationArn, got {}",
                    answer.body
                )
            })
    }

    /// Write ONE attribute of a platform application.
    async fn set_application_attribute(
        &self,
        application: &str,
        name: &str,
        value: &str,
    ) -> Answer {
        self.sns(
            "SetPlatformApplicationAttributes",
            &[
                ("PlatformApplicationArn", application),
                ("Attributes.entry.1.key", name),
                ("Attributes.entry.1.value", value),
            ],
        )
        .await
    }

    /// The attribute map a platform application reports.
    async fn application_attributes(
        &self,
        application: &str,
    ) -> std::collections::HashMap<String, String> {
        let answer = self
            .sns(
                "GetPlatformApplicationAttributes",
                &[("PlatformApplicationArn", application)],
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "GetPlatformApplicationAttributes {application} must succeed, got {} / {}",
            answer.status, answer.body
        );
        attribute_map(&answer)
    }
}

/// A trust policy whose single `Allow` statement names `principal` as a service that may
/// perform `action` on the role.
fn trusts(principal: serde_json::Value, action: serde_json::Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": principal,
            "Action": action,
        }],
    })
    .to_string()
}

/// The same policy, with `condition` as the statement's `Condition` element — the narrowing a
/// least-privilege logging role carries, and `None` for a statement that carries none.
fn trusts_under(condition: Option<serde_json::Value>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": {"Service": SNS_PRINCIPAL},
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement
            .as_object_mut()
            .expect("a statement is a JSON object")
            .insert("Condition".to_string(), condition);
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// The sentence SNS states for a value it cannot use as a delivery-status logging role, stated
/// here independently of the crate under test.
fn not_a_valid_logging_role(attribute: &str, value: &str) -> String {
    format!(
        "Invalid parameter: {attribute}: {value} is not a valid role to allow SNS to write to \
         Cloudwatch Logs"
    )
}

/// Assert that `answer` is the parameter refusal SNS states for `attribute` naming `value`.
fn assert_refused(case: &str, answer: &Answer, attribute: &str, value: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.code(),
        "InvalidParameter",
        "case {case:?}: expected the InvalidParameter code, got {}",
        answer.body
    );
    assert_eq!(
        answer.element("Message").as_deref(),
        Some(not_a_valid_logging_role(attribute, value).as_str()),
        "case {case:?}: expected the service's own sentence, got {}",
        answer.body
    );
}

/// The account id an ARN this emulator minted carries: the fifth colon-separated field.
fn account_of(arn: &str) -> String {
    arn.split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("an ARN names the account it belongs to, got {arn}"))
        .to_string()
}

/// RULE, over the whole attribute catalog: EVERY `<Protocol>SuccessFeedbackRoleArn` /
/// `<Protocol>FailureFeedbackRoleArn` attribute is refused when the role it names is one this
/// service cannot assume, and accepted when that role's trust policy admits this service — so
/// the topic reads back carrying the role it can log through and none it cannot.
#[tokio::test]
async fn every_feedback_role_attribute_is_held_to_the_roles_trust_policy() {
    let emulator = Emulator::new();
    let topic = emulator.topic("feedback-role-catalog").await;
    let trusted = emulator
        .role(
            "sns-logs-trusted",
            &trusts(json!({"Service": SNS_PRINCIPAL}), json!("sts:AssumeRole")),
        )
        .await;
    let account = account_of(&trusted);
    let untrusted = emulator
        .role(
            "sns-logs-root-only",
            &trusts(
                json!({"AWS": format!("arn:aws:iam::{account}:root")}),
                json!("sts:AssumeRole"),
            ),
        )
        .await;

    for attribute in FEEDBACK_ROLE_ATTRIBUTES {
        let case = format!("{attribute} naming a role that trusts only the account root");
        let answer = emulator.set_attribute(&topic, attribute, &untrusted).await;
        assert_refused(&case, &answer, attribute, &untrusted);
        let held = emulator.attributes(&topic).await;
        assert!(
            !held.contains_key(*attribute),
            "case {case:?}: a refused write must leave the topic carrying no {attribute}, got \
             {held:?}"
        );

        let case = format!("{attribute} naming a role that trusts this service");
        let answer = emulator.set_attribute(&topic, attribute, &trusted).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );
        let held = emulator.attributes(&topic).await;
        assert_eq!(
            held.get(*attribute).map(String::as_str),
            Some(trusted.as_str()),
            "case {case:?}: the accepted role must read back, got {held:?}"
        );
    }
}

/// One row of the trust-document table: the case, the trust policy the role carries (`None` for
/// a role this account's IAM holds nothing under) and whether the write must be refused.
type TrustCase = (&'static str, Option<String>, bool);

/// The trust documents a feedback role is judged by, each with the answer it must produce.
///
/// `account` is the account every ARN here belongs to, which is what makes the root principal a
/// principal of THIS account rather than a foreign one.
fn trust_rows(account: &str) -> Vec<TrustCase> {
    vec![
        (
            "a role trusting this service",
            Some(trusts(
                json!({"Service": SNS_PRINCIPAL}),
                json!("sts:AssumeRole"),
            )),
            false,
        ),
        (
            "a role trusting this service among several",
            Some(trusts(
                json!({"Service": ["events.amazonaws.com", SNS_PRINCIPAL]}),
                json!("sts:AssumeRole"),
            )),
            false,
        ),
        (
            "a role trusting this service through an action wildcard",
            Some(trusts(json!({"Service": SNS_PRINCIPAL}), json!("sts:*"))),
            false,
        ),
        (
            "a role trusting only the account root",
            Some(trusts(
                json!({"AWS": format!("arn:aws:iam::{account}:root")}),
                json!("sts:AssumeRole"),
            )),
            true,
        ),
        (
            "a role trusting another service",
            Some(trusts(
                json!({"Service": "events.amazonaws.com"}),
                json!("sts:AssumeRole"),
            )),
            true,
        ),
        (
            "a role trusting this service for another STS action only",
            Some(trusts(
                json!({"Service": SNS_PRINCIPAL}),
                json!("sts:TagSession"),
            )),
            true,
        ),
        (
            "a role that denies this service beside the allow",
            Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": {"Service": SNS_PRINCIPAL},
                            "Action": "sts:AssumeRole",
                        },
                        {
                            "Effect": "Deny",
                            "Principal": {"Service": SNS_PRINCIPAL},
                            "Action": "sts:AssumeRole",
                        },
                    ],
                })
                .to_string(),
            ),
            true,
        ),
        // A role this emulator's IAM holds nothing under is no evidence about the role the
        // caller named — a logging role is routinely provisioned out of band — so the write is
        // served.
        ("a role IAM holds nothing under", None, false),
    ]
}

/// RULE, over the trust document: what decides a feedback role is the document IAM holds for it,
/// read as IAM reads a trust policy — the principal it names, the action it names, and an
/// explicit deny winning over the allow — and the SAME answer comes of the write whichever call
/// carried it, so `CreateTopic` refuses what `SetTopicAttributes` refuses and leaves no topic.
///
/// The attribute written is `LambdaFailureFeedbackRoleArn`: one entry of the catalog above that
/// no recorded case of this rule cited, so the rule is asserted away from its one instance.
#[tokio::test]
async fn the_trust_document_decides_which_feedback_role_a_topic_may_carry() {
    const ATTRIBUTE: &str = "LambdaFailureFeedbackRoleArn";

    let emulator = Emulator::new();
    let topic = emulator.topic("feedback-role-trust").await;
    // The account every role ARN below belongs to, taken from an ARN the emulator minted.
    let account = account_of(&topic);

    for (index, (case, document, refused)) in trust_rows(&account).into_iter().enumerate() {
        let role_name = format!("sns-logs-{index}");
        let role = match &document {
            Some(document) => emulator.role(&role_name, document).await,
            None => format!("arn:aws:iam::{account}:role/{role_name}"),
        };

        let answer = emulator.set_attribute(&topic, ATTRIBUTE, &role).await;
        let created = emulator
            .create_topic_with(&format!("feedback-role-created-{index}"), ATTRIBUTE, &role)
            .await;
        let held = emulator.attributes(&topic).await;
        if refused {
            assert_refused(case, &answer, ATTRIBUTE, &role);
            assert_eq!(
                held.get(ATTRIBUTE).map(String::as_str),
                None,
                "case {case:?}: a refused write must leave the topic carrying no {ATTRIBUTE}, \
                 got {held:?}"
            );
            assert_refused(case, &created, ATTRIBUTE, &role);
            // The refused create left no topic behind, so the name it asked for names nothing.
            let read = emulator
                .sns(
                    "GetTopicAttributes",
                    &[(
                        "TopicArn",
                        &format!("arn:aws:sns:{REGION}:{account}:feedback-role-created-{index}"),
                    )],
                )
                .await;
            assert_eq!(
                read.code(),
                "NotFound",
                "case {case:?}: a refused create must leave no topic, got {} / {}",
                read.status,
                read.body
            );
        } else {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {} / {}",
                answer.status, answer.body
            );
            assert_eq!(
                held.get(ATTRIBUTE).map(String::as_str),
                Some(role.as_str()),
                "case {case:?}: the accepted role must read back, got {held:?}"
            );
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreateTopic must accept the same value, got {} / {}",
                created.status, created.body
            );
            // The attribute is CLEARED again, so the next row starts from a topic carrying
            // none of it and its assertion is about its own write.
            let cleared = emulator
                .sns(
                    "SetTopicAttributes",
                    &[("TopicArn", &topic), ("AttributeName", ATTRIBUTE)],
                )
                .await;
            assert_eq!(
                cleared.status, 200,
                "case {case:?}: clearing {ATTRIBUTE} must succeed, got {} / {}",
                cleared.status, cleared.body
            );
        }
    }
}

/// RULE, in time: the trust relationship is read at EACH write rather than once per role — so a
/// role whose trust policy was narrowed after it was accepted takes no further write, while the
/// attribute the earlier write landed stays exactly as it was (SNS does not reconfigure a topic
/// because IAM changed).
#[tokio::test]
async fn the_trust_relationship_is_read_at_every_write() {
    let emulator = Emulator::new();
    let topic = emulator.topic("feedback-role-retracted").await;
    let role = emulator
        .role(
            "sns-logs-retracted",
            &trusts(json!({"Service": SNS_PRINCIPAL}), json!("sts:AssumeRole")),
        )
        .await;
    let account = account_of(&role);

    let case = "a trusted role, written while the trust held";
    let answer = emulator
        .set_attribute(&topic, "SQSSuccessFeedbackRoleArn", &role)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status, answer.body
    );

    emulator
        .iam(
            "UpdateAssumeRolePolicy",
            &[
                ("RoleName", "sns-logs-retracted"),
                (
                    "PolicyDocument",
                    &trusts(
                        json!({"AWS": format!("arn:aws:iam::{account}:root")}),
                        json!("sts:AssumeRole"),
                    ),
                ),
            ],
        )
        .await;

    let case = "the same role, written after its trust policy was narrowed";
    let answer = emulator
        .set_attribute(&topic, "SQSFailureFeedbackRoleArn", &role)
        .await;
    assert_refused(case, &answer, "SQSFailureFeedbackRoleArn", &role);
    let held = emulator.attributes(&topic).await;
    assert_eq!(
        held.get("SQSSuccessFeedbackRoleArn").map(String::as_str),
        Some(role.as_str()),
        "case {case:?}: the attribute written while the trust held must stay, got {held:?}"
    );
    assert!(
        !held.contains_key("SQSFailureFeedbackRoleArn"),
        "case {case:?}: the refused write must leave no SQSFailureFeedbackRoleArn, got {held:?}"
    );
}

/// One row of the conditioned-trust table: the case, the `Condition` element the role's trust
/// statement carries (`None` for a statement that carries none) and whether a write naming that
/// role on the configured topic must be refused.
type ConditionCase = (&'static str, Option<serde_json::Value>, bool);

/// The conditioned trust grants a feedback role is judged by, each with the answer a write on
/// `topic` must produce.
///
/// `sibling` is another topic of the same account and Region, and `account` is the account both
/// belong to — the two values a confused-deputy clause on a logging role is written over.
fn condition_rows(topic: &str, sibling: &str, account: &str) -> Vec<ConditionCase> {
    let every_topic = format!("arn:aws:sns:{REGION}:{account}:*");
    let other_names = format!("arn:aws:sns:{REGION}:{account}:feedback-condition-other-*");
    vec![
        ("an unconditioned grant", None, false),
        (
            "ArnEquals on the very topic the write configures",
            Some(json!({"ArnEquals": {"aws:SourceArn": topic}})),
            false,
        ),
        (
            "ArnEquals on a sibling topic of the same account",
            Some(json!({"ArnEquals": {"aws:SourceArn": sibling}})),
            true,
        ),
        (
            "ArnLike over every topic of this account and Region",
            Some(json!({"ArnLike": {"aws:SourceArn": every_topic}})),
            false,
        ),
        (
            "ArnLike over another family of topic names",
            Some(json!({"ArnLike": {"aws:SourceArn": other_names}})),
            true,
        ),
        (
            "StringEquals on the account the topic belongs to",
            Some(json!({"StringEquals": {"aws:SourceAccount": account}})),
            false,
        ),
        (
            "StringEquals on another account",
            Some(json!({"StringEquals": {"aws:SourceAccount": "999999999999"}})),
            true,
        ),
        (
            "ArnNotEquals on a sibling topic, which this topic is not",
            Some(json!({"ArnNotEquals": {"aws:SourceArn": sibling}})),
            false,
        ),
        (
            "ArnNotEquals on the topic itself",
            Some(json!({"ArnNotEquals": {"aws:SourceArn": topic}})),
            true,
        ),
        (
            "both keys the assume carries, each satisfied",
            Some(json!({
                "ArnEquals": {"aws:SourceArn": topic},
                "StringEquals": {"aws:SourceAccount": account},
            })),
            false,
        ),
        (
            "this topic's own ARN beside another account",
            Some(json!({
                "ArnEquals": {"aws:SourceArn": topic},
                "StringEquals": {"aws:SourceAccount": "999999999999"},
            })),
            true,
        ),
        (
            "a clause over a key the assume carries no value for",
            Some(json!({"StringEquals": {"aws:SourceVpc": "vpc-1"}})),
            true,
        ),
    ]
}

/// RULE, over the CONDITION on an otherwise-trusting role: the grant has to cover the assume SNS
/// would make FOR THE TOPIC in the request, so a trust statement's `Condition` is decided against
/// the keys that assume carries — the topic's own ARN and the account it belongs to — and a clause
/// the assume cannot be shown to satisfy is no grant. The write is refused with the service's own
/// parameter sentence and the attribute does not land.
///
/// The attribute written is `HTTPSuccessFeedbackRoleArn`: an entry of the catalog above that no
/// recorded case of this rule cited, so the rule is asserted away from its one instance.
#[tokio::test]
async fn a_trust_condition_is_decided_against_the_topic_the_write_configures() {
    const ATTRIBUTE: &str = "HTTPSuccessFeedbackRoleArn";

    let emulator = Emulator::new();
    let topic = emulator.topic("feedback-condition-configured").await;
    let sibling = emulator.topic("feedback-condition-sibling").await;
    let account = account_of(&topic);

    for (index, (case, condition, refused)) in condition_rows(&topic, &sibling, &account)
        .into_iter()
        .enumerate()
    {
        let role = emulator
            .role(
                &format!("sns-logs-conditioned-{index}"),
                &trusts_under(condition),
            )
            .await;
        let answer = emulator.set_attribute(&topic, ATTRIBUTE, &role).await;
        let held = emulator.attributes(&topic).await;
        if refused {
            assert_refused(case, &answer, ATTRIBUTE, &role);
            assert!(
                !held.contains_key(ATTRIBUTE),
                "case {case:?}: a refused write must leave the topic carrying no {ATTRIBUTE}, \
                 got {held:?}"
            );
        } else {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {} / {}",
                answer.status, answer.body
            );
            assert_eq!(
                held.get(ATTRIBUTE).map(String::as_str),
                Some(role.as_str()),
                "case {case:?}: the accepted role must read back, got {held:?}"
            );
            // Cleared again, so the next row's assertion is about its own write.
            let cleared = emulator
                .sns(
                    "SetTopicAttributes",
                    &[("TopicArn", &topic), ("AttributeName", ATTRIBUTE)],
                )
                .await;
            assert_eq!(
                cleared.status, 200,
                "case {case:?}: clearing {ATTRIBUTE} must succeed, got {} / {}",
                cleared.status, cleared.body
            );
        }
    }
}

/// RULE, per topic: ONE role scoped by `aws:SourceArn` to one topic is accepted on that topic and
/// refused on every sibling — through `SetTopicAttributes` and through the `CreateTopic` that
/// carries the same attribute, which leaves no topic behind.
#[tokio::test]
async fn a_role_scoped_to_one_topic_takes_no_write_on_a_sibling() {
    const ATTRIBUTE: &str = "SQSSuccessFeedbackRoleArn";

    let emulator = Emulator::new();
    let scoped_to = emulator.topic("feedback-scope-owner").await;
    let sibling = emulator.topic("feedback-scope-sibling").await;
    let account = account_of(&scoped_to);
    let role = emulator
        .role(
            "sns-logs-scoped",
            &trusts_under(Some(json!({
                "ArnEquals": {"aws:SourceArn": scoped_to},
                "StringEquals": {"aws:SourceAccount": account},
            }))),
        )
        .await;

    let case = "the topic the role's trust is scoped to";
    let answer = emulator.set_attribute(&scoped_to, ATTRIBUTE, &role).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    let held = emulator.attributes(&scoped_to).await;
    assert_eq!(
        held.get(ATTRIBUTE).map(String::as_str),
        Some(role.as_str()),
        "case {case:?}: the accepted role must read back, got {held:?}"
    );

    let case = "a sibling topic the role's trust says nothing about";
    let answer = emulator.set_attribute(&sibling, ATTRIBUTE, &role).await;
    assert_refused(case, &answer, ATTRIBUTE, &role);
    let held = emulator.attributes(&sibling).await;
    assert!(
        !held.contains_key(ATTRIBUTE),
        "case {case:?}: the refused write must leave the sibling carrying no {ATTRIBUTE}, got \
         {held:?}"
    );

    let case = "a create carrying the same value for a topic the role is not scoped to";
    let created = emulator
        .create_topic_with("feedback-scope-created", ATTRIBUTE, &role)
        .await;
    assert_refused(case, &created, ATTRIBUTE, &role);
    let read = emulator
        .sns(
            "GetTopicAttributes",
            &[(
                "TopicArn",
                &format!("arn:aws:sns:{REGION}:{account}:feedback-scope-created"),
            )],
        )
        .await;
    assert_eq!(
        read.code(),
        "NotFound",
        "case {case:?}: a refused create must leave no topic, got {} / {}",
        read.status,
        read.body
    );
}

/// RULE, on the OTHER resource whose attributes name a delivery-status feedback role: a platform
/// application's `SuccessFeedbackRoleArn` / `FailureFeedbackRoleArn` ("IAM role ARN used to give
/// Amazon SNS write access to use CloudWatch Logs on your behalf",
/// `SetPlatformApplicationAttributesInput$Attributes` doc) names a role Amazon SNS has to assume
/// FOR THAT APPLICATION, so the write is answered from the trust document this account's IAM
/// holds for the role exactly as a topic's is — accepted and read back when the document grants
/// this service the assume, refused `400 InvalidParameter` with the service's own logging-role
/// sentence otherwise, and the application reads back carrying no such attribute. The rule is the
/// same whichever call carried the write, so the registration that names the same value is
/// refused too and leaves no application behind.
///
/// The rows differ along which of the pair the write carried, what the role's trust document
/// says (this service, another service, and a role IAM holds nothing under, which is no evidence
/// about a role provisioned out of band), and which call wrote it.
#[tokio::test]
async fn a_platform_applications_feedback_role_is_held_to_the_same_trust_policy() {
    let emulator = Emulator::new();
    let trusted = emulator
        .role(
            "sns-app-logs-trusted",
            &trusts(json!({"Service": SNS_PRINCIPAL}), json!("sts:AssumeRole")),
        )
        .await;
    let account = account_of(&trusted);
    let untrusted = emulator
        .role(
            "sns-app-logs-other-service",
            &trusts(
                json!({"Service": OTHER_SERVICE_PRINCIPAL}),
                json!("sts:AssumeRole"),
            ),
        )
        .await;
    // A role this account's IAM holds nothing under: a logging role is routinely provisioned
    // out of band, so it is no evidence about the role the caller named and is not refused.
    let unheld = format!("arn:aws:iam::{account}:role/sns-app-logs-provisioned-elsewhere");

    let application = emulator
        .platform_application("feedback-role-application")
        .await;

    for attribute in APPLICATION_FEEDBACK_ROLE_ATTRIBUTES {
        let case = format!("{attribute} naming a role that trusts another service");
        let answer = emulator
            .set_application_attribute(&application, attribute, &untrusted)
            .await;
        assert_refused(&case, &answer, attribute, &untrusted);
        let held = emulator.application_attributes(&application).await;
        assert!(
            !held.contains_key(*attribute),
            "case {case:?}: a refused write must leave the application carrying no \
             {attribute}, got {held:?}"
        );

        let case = format!("{attribute} naming a role IAM holds nothing under");
        let answer = emulator
            .set_application_attribute(&application, attribute, &unheld)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );

        let case = format!("{attribute} naming a role that trusts this service");
        let answer = emulator
            .set_application_attribute(&application, attribute, &trusted)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            answer.status, answer.body
        );
        let held = emulator.application_attributes(&application).await;
        assert_eq!(
            held.get(*attribute).map(String::as_str),
            Some(trusted.as_str()),
            "case {case:?}: the accepted role must read back, got {held:?}"
        );
    }

    // The REGISTRATION is the other path the same attribute is written by, and the same rule
    // decides it: nothing is registered under a configuration this service could not honour.
    let case = "a registration naming a role that trusts another service";
    let name = "feedback-role-application-refused";
    let created = emulator
        .create_application_with(name, Some(("SuccessFeedbackRoleArn", &untrusted)))
        .await;
    assert_refused(case, &created, "SuccessFeedbackRoleArn", &untrusted);
    let read = emulator
        .sns(
            "GetPlatformApplicationAttributes",
            &[(
                "PlatformApplicationArn",
                &format!("arn:aws:sns:{REGION}:{account}:app/{PLATFORM}/{name}"),
            )],
        )
        .await;
    assert_eq!(
        read.code(),
        "NotFound",
        "case {case:?}: a refused registration must leave no application, got {} / {}",
        read.status,
        read.body
    );
}
