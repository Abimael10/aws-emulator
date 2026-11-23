// SPDX-License-Identifier: CC0-1.0

//! A delivery-status feedback role is held to the trust document IAM holds for it on a router
//! that VERIFIES request signatures — the configuration the emulator serves traffic in.
//!
//! The RULE under test: **a write of a topic's `<Protocol>SuccessFeedbackRoleArn` /
//! `<Protocol>FailureFeedbackRoleArn` ("used to give Amazon SNS write access to use CloudWatch
//! Logs on your behalf", `SetTopicAttributesInput$AttributeName`) names a role Amazon SNS has to
//! ASSUME for that topic, so the write is ANSWERED from the trust document this account's IAM
//! holds for the role — accepted, and read back by `GetTopicAttributes`, when that document
//! grants `sts:AssumeRole` to this service's own principal for the topic, and refused
//! `400 InvalidParameter` otherwise, leaving the topic carrying no such attribute.**
//!
//! What this suite adds to the two that already pin that rule — the SNS crate's own
//! `tests/all/feedback_role_trust.rs`, which reads the whole attribute catalog and the whole
//! trust-document table, and [`crate::sns_service_trusted_feedback_role_logs_each_delivery`],
//! which reads the delivery records the accepted role authorizes — is the axis neither covers:
//! both turn request authentication OFF, so neither can see whether the trust document SNS
//! reads THROUGH THE EMULATOR'S OWN CALLBACK is reachable on a router that authenticates every
//! request, which is how the emulator is served. A hop that cannot be made on such a router
//! learns nothing about the role, and a write that learns nothing is a write that answers
//! whatever it would answer with no IAM at all.
//!
//! The rows span what the rule generalises over: what the LIVE trust document says about this
//! service's assume — this service's principal, another service's instead, this service's under
//! a `Condition` naming the topic being configured, the same `Condition` naming a sibling topic
//! — and a role ARN this account's IAM holds nothing under, which is no evidence about a role
//! provisioned out of band and so is not refused. Each row is driven over two of the attribute
//! catalog's protocol prefixes, whose attributes are written and read independently, the whole
//! table runs in more than one Region — IAM is global, so which Region a write is signed for
//! decides nothing about the role it names, while the hop that reads the document is addressed
//! in the Region the write arrived in — and every write is REPEATED, because a client whose
//! write is refused retries it across the window IAM takes to propagate a trust document and
//! every attempt has to be answered the same way.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{decoded_entries, element, form_encode, opt_element};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Regions the whole table is driven in: the Region the IAM console is served from, and
/// another one, whose calls are signed and addressed for that Region alone.
const REGIONS: [&str; 2] = ["us-east-1", "us-west-2"];

/// The awsQuery `Version` of the Amazon SNS API, as its model states it.
const SNS_VERSION: &str = "2010-03-31";

/// The awsQuery `Version` of the IAM API.
const IAM_VERSION: &str = "2010-05-08";

/// The service principal a delivery-status feedback role has to admit for Amazon SNS to assume
/// it.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// Another service's principal, which a row names instead: it is THIS service's principal, and
/// not merely some service principal, that decides the assume.
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The action a trust document grants a principal that may assume the role.
const ASSUME_ACTION: &str = "sts:AssumeRole";

/// The condition key naming the RESOURCE a cross-service assume is made for — the topic whose
/// delivery-status records the role would be assumed to write.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The protocol prefixes this suite writes the feedback-role attributes of: two of the prefixes
/// `SetTopicAttributesInput$AttributeName` documents (`HTTP`, `Firehose`, `Lambda`,
/// `Application`, `SQS`), so a row is read over more than one attribute.
const PREFIXES: [&str; 2] = ["SQS", "Lambda"];

/// How many times each row writes the SAME value: a client whose write is refused retries it
/// across the window IAM takes to propagate a trust document, and every attempt has to be
/// ANSWERED the same way — the answer is decided by the document the role carries, not by which
/// attempt it is.
const ATTEMPTS: usize = 3;

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
struct Signer {
    key_id: String,
    secret: String,
    /// The Region every call this signer signs is addressed in.
    region: &'static str,
}

impl Signer {
    /// Sign `request` for `signing_name` in this signer's Region, in the `Authorization` header
    /// an SDK request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(self.region)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed awsQuery call, as the status and the body answered.
async fn query(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    version: &str,
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
    let host = format!("{signing_name}.{}.amazonaws.com", signer.region);
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.into_bytes())
        .expect("the test request is well formed");
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon SNS call.
async fn sns(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    query(router, signer, "sns", SNS_VERSION, action, params).await
}

/// One Amazon SNS call that must succeed.
async fn sns_ok(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = sns(router, signer, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture SNS {action} must succeed, got {status} with {body}"
    );
    body
}

/// One IAM fixture call, which must succeed.
async fn iam(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, signer, "iam", IAM_VERSION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture IAM {action} must succeed, got {status} with {body}"
    );
    body
}

/// The sentence Amazon SNS states for a value it cannot use as a delivery-status logging role,
/// stated here independently of the crates under test.
fn not_a_valid_logging_role(attribute: &str, value: &str) -> String {
    format!(
        "Invalid parameter: {attribute}: {value} is not a valid role to allow SNS to write to \
         Cloudwatch Logs"
    )
}

/// A trust document granting [`ASSUME_ACTION`] to the service principal `principal`, narrowed by
/// `source_arn` as the statement's `aws:SourceArn` clause when a row names one.
fn trust(principal: &str, source_arn: Option<&str>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": {"Service": principal},
        "Action": ASSUME_ACTION,
    });
    if let Some(source) = source_arn {
        statement
            .as_object_mut()
            .expect("a statement is a JSON object")
            .insert(
                "Condition".to_string(),
                json!({"ArnEquals": {SOURCE_ARN_KEY: source}}),
            );
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// What a row's trust document says, which is what decides the row's answer.
#[derive(Debug, Clone, Copy)]
enum Trust {
    /// IAM holds no role under the ARN the write names.
    Unheld,
    /// This service's principal, unconditionally.
    ThisService,
    /// Another service's principal instead.
    OtherService,
    /// This service's principal, narrowed to the topic being configured.
    ThisServiceForThisTopic,
    /// This service's principal, narrowed to a SIBLING topic.
    ThisServiceForAnotherTopic,
}

/// One row: the case, the trust document the role is created with, the document a client
/// afterwards leaves on it with `UpdateAssumeRolePolicy` — the LIVE document, which is then
/// what decides the write — and whether the write is accepted.
struct Row {
    case: &'static str,
    slug: &'static str,
    trust: Trust,
    repointed: Option<Trust>,
    accepted: bool,
}

/// RULE: on a router that verifies request signatures, a feedback-role write is answered from
/// the trust document IAM holds for the role — accepted and read back when that document grants
/// this service the assume FOR the topic, refused `400 InvalidParameter` with this service's own
/// sentence and left off the topic otherwise.
#[test]
fn a_feedback_role_write_is_answered_from_iam_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("sns-feedback-role-trust-authenticated".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                for region in REGIONS {
                    drive(region).await;
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, in `region`, against ONE assembled, authenticating router.
async fn drive(region: &'static str) {
    let rows = [
        Row {
            case: "a role this account's IAM holds nothing under",
            slug: "unheld",
            trust: Trust::Unheld,
            repointed: None,
            accepted: true,
        },
        Row {
            case: "a role trusting this service",
            slug: "trusted",
            trust: Trust::ThisService,
            repointed: None,
            accepted: true,
        },
        Row {
            case: "a role trusting another service instead",
            slug: "other-service",
            trust: Trust::OtherService,
            repointed: None,
            accepted: false,
        },
        Row {
            case: "a role trusting this service for this very topic",
            slug: "scoped-here",
            trust: Trust::ThisServiceForThisTopic,
            repointed: None,
            accepted: true,
        },
        Row {
            case: "a role trusting this service for a sibling topic only",
            slug: "scoped-elsewhere",
            trust: Trust::ThisServiceForAnotherTopic,
            repointed: None,
            accepted: false,
        },
        Row {
            case: "a role re-pointed from this service to another",
            slug: "repointed-away",
            trust: Trust::ThisService,
            repointed: Some(Trust::OtherService),
            accepted: false,
        },
        Row {
            case: "a role re-pointed from another service to this one",
            slug: "repointed-here",
            trust: Trust::OtherService,
            repointed: Some(Trust::ThisService),
            accepted: true,
        },
    ];

    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Every call below runs with request authentication ON, as the emulator serves traffic.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
        region,
    };

    // The sibling topic a narrowed trust document names instead of the topic under test.
    let sibling = element(
        &sns_ok(
            &router,
            &signer,
            "fixture",
            "CreateTopic",
            &[("Name", "feedback-role-sibling")],
        )
        .await,
        "TopicArn",
    );

    for row in rows {
        let case = &format!("{}, signed for {region}", row.case);
        let case = case.as_str();
        let topic = element(
            &sns_ok(
                &router,
                &signer,
                case,
                "CreateTopic",
                &[("Name", &format!("feedback-role-{}", row.slug))],
            )
            .await,
            "TopicArn",
        );

        let role_name = format!("feedback-role-{}-role", row.slug);
        let document_of = |which: Trust| match which {
            Trust::Unheld => None,
            Trust::ThisService => Some(trust(SNS_PRINCIPAL, None)),
            Trust::OtherService => Some(trust(OTHER_PRINCIPAL, None)),
            Trust::ThisServiceForThisTopic => Some(trust(SNS_PRINCIPAL, Some(&topic))),
            Trust::ThisServiceForAnotherTopic => Some(trust(SNS_PRINCIPAL, Some(&sibling))),
        };
        let document = document_of(row.trust);
        let role_arn = match &document {
            Some(document) => element(
                &iam(
                    &router,
                    &signer,
                    case,
                    "CreateRole",
                    &[
                        ("RoleName", &role_name),
                        ("AssumeRolePolicyDocument", document),
                    ],
                )
                .await,
                "Arn",
            ),
            // No role is created, so the ARN names a role IAM holds nothing under.
            None => format!(
                "arn:aws:iam::{}:role/{role_name}",
                engine::EMULATOR_ACCOUNT_ID
            ),
        };
        // A client that RE-POINTS the role's trust leaves another document on it, and the
        // write is decided by the one the role carries when it arrives.
        if let Some(document) = row.repointed.and_then(document_of) {
            iam(
                &router,
                &signer,
                case,
                "UpdateAssumeRolePolicy",
                &[("RoleName", &role_name), ("PolicyDocument", &document)],
            )
            .await;
        }

        for prefix in PREFIXES {
            let attribute = format!("{prefix}SuccessFeedbackRoleArn");
            // The SAME write, more than once: a client that retries the write across the
            // window IAM takes to propagate a trust document gets the same answer to every
            // attempt, because what decides it is the document the role carries and not
            // which attempt this is.
            for attempt in 1..=ATTEMPTS {
                let case = &format!("{case}, attempt {attempt}");
                let (status, body) = sns(
                    &router,
                    &signer,
                    "SetTopicAttributes",
                    &[
                        ("TopicArn", &topic),
                        ("AttributeName", &attribute),
                        ("AttributeValue", &role_arn),
                    ],
                )
                .await;
                if row.accepted {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: writing {attribute} must be accepted, got {status} \
                         with {body}"
                    );
                } else {
                    assert_eq!(
                        status, 400,
                        "case {case:?}: writing {attribute} must be refused, got {status} \
                         with {body}"
                    );
                    assert_eq!(
                        opt_element(&body, "Code").as_deref(),
                        Some("InvalidParameter"),
                        "case {case:?}: expected the InvalidParameter code, got {body}"
                    );
                    assert_eq!(
                        opt_element(&body, "Message").as_deref(),
                        Some(not_a_valid_logging_role(&attribute, &role_arn).as_str()),
                        "case {case:?}: expected this service's own sentence, got {body}"
                    );
                }
            }
        }

        // What the topic CARRIES is the half of the rule a client reads, so it is read back
        // rather than assumed from the writes above.
        let read = sns_ok(
            &router,
            &signer,
            case,
            "GetTopicAttributes",
            &[("TopicArn", &topic)],
        )
        .await;
        let attributes = decoded_entries(&read);
        for prefix in PREFIXES {
            let attribute = format!("{prefix}SuccessFeedbackRoleArn");
            let held = attributes.get(&attribute).cloned();
            let expected = row.accepted.then(|| role_arn.clone());
            assert_eq!(
                held, expected,
                "case {case:?}: expected the topic to carry {attribute} = {expected:?}, got \
                 {held:?}"
            );
        }
    }
}
