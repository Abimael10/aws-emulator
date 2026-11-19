// SPDX-License-Identifier: CC0-1.0

//! How `SendEmail` reports a sending identity this account has not verified, end to
//! end over HTTP.
//!
//! An SES identity belongs to ONE Region — `CreateEmailIdentity` "starts the process
//! of verifying an email identity … in the current Amazon Web Services Region" — so
//! the refusal of a send whose `FromEmailAddress` failed the verification check names
//! the Region whose identity list it failed in, and the identities that failed it.
//!
//! The rows below span what that rule generalises over: the Region the call is signed
//! for, and the class of the sender — an address of no identity the account holds, an
//! address whose domain identity is still unverified, and two different senders in one
//! Region, which therefore produce two different messages.
//!
//! WHERE that gate sits among this operation's other refusals is the second rule here:
//! what the REQUEST carries is checked first, then the sending identity, and only then is
//! the resource the content NAMES resolved. So `Template$TemplateData` that is no set of
//! key-value pairs is the caller's mistake (`BadRequestException`) whatever the sender and
//! whatever template the content names; a sender that failed the verification check is
//! refused `MessageRejected` before the template name is looked up, which is why such a
//! sender cannot learn from the refusal whether the template exists; and a name the account
//! holds no template under is the modelled 404 only once the gate has been passed.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    ComAmazonawsSesv2SimpleemailserviceV2State, MAX_EMAIL_TEMPLATE_BYTES,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// One answered request: the status, the error shape when there was one, and the
/// parsed body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// A string at `pointer`, or `""` when the body carries nothing there.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulated service behind its router, plus the shared engine so a case can
/// settle the verification transition the clock owes an identity.
struct Ses {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// One call, signed for `region`: the credential scope is what the handler reads
    /// the Region of the call from.
    async fn call(&self, region: &str, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.{region}.amazonaws.com{path}"))
            .header("host", format!("email.{region}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIAEMULATOR0000000/20260101/{region}/ses/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }

    async fn create_identity(&self, region: &str, identity: &str) -> Reply {
        self.call(
            region,
            "POST",
            "/v2/email/identities",
            Some(json!({ "EmailIdentity": identity })),
        )
        .await
    }

    async fn send(&self, region: &str, from: &str) -> Reply {
        self.call(
            region,
            "POST",
            "/v2/email/outbound-emails",
            Some(json!({
                "FromEmailAddress": from,
                "Destination": { "ToAddresses": ["recipient@example.net"] },
                "Content": {"Simple": {"Subject": {"Data": "hi"}, "Body": {"Text": {"Data": "hello"}}}}
            })),
        )
        .await
    }

    /// `CreateEmailTemplate`: save the one template the templated cases below name.
    async fn create_template(&self, region: &str) -> Reply {
        self.call(
            region,
            "POST",
            "/v2/email/templates",
            Some(json!({
                "TemplateName": TEMPLATE,
                "TemplateContent": {"Subject": SUBJECT, "Text": TEXT, "Html": HTML},
            })),
        )
        .await
    }

    /// `TestRenderEmailTemplate`: the preview of the stored template against `data`.
    async fn render(&self, region: &str, data: &str) -> Reply {
        self.call(
            region,
            "POST",
            &format!("/v2/email/templates/{TEMPLATE}/render"),
            Some(json!({"TemplateData": data})),
        )
        .await
    }

    /// `SendEmail` with `content` as its `Content` member.
    async fn send_content(&self, region: &str, from: &str, content: Value) -> Reply {
        self.call(
            region,
            "POST",
            "/v2/email/outbound-emails",
            Some(json!({
                "FromEmailAddress": from,
                // The mailbox simulator's success address: a recipient that accepts
                // whatever is addressed to it.
                "Destination": {"ToAddresses": ["success@simulator.amazonses.com"]},
                "Content": content,
            })),
        )
        .await
    }

    /// `GetMessageInsights`: what the service reports about one accepted message.
    async fn insights(&self, region: &str, message_id: &str) -> Reply {
        self.call(
            region,
            "GET",
            &format!("/v2/email/insights/{message_id}"),
            None,
        )
        .await
    }

    /// How many messages the send log holds, and what the 24-hour counter was charged.
    fn accepted(&self) -> (usize, f64) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let store = state.lock();
        (store.sent_messages.len(), store.send_counters.sent_last_24h)
    }

    /// Bring every identity's verification transition into the past, so a case that
    /// sends from an identity the account holds does not race the span the clock owes
    /// it.
    fn settle_identities(&self) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let past = SystemTime::now()
            .checked_sub(Duration::from_millis(1))
            .expect("an instant just before now");
        for record in store.identities.values_mut() {
            record.verification_due_at = past;
        }
    }
}

/// The template every templated case below names when it names a stored one.
const TEMPLATE: &str = "receipt";

/// Its subject, with the one message variable it personalises.
const SUBJECT: &str = "Hello {{name}}";

/// Its text part.
const TEXT: &str = "Hi {{name}}, this is a probe.";

/// Its HTML part.
const HTML: &str = "<p>Hi {{name}}</p>";

/// Well-formed template data for that template.
const DATA: &str = "{\"name\":\"Ada\"}";

/// The sentence this service answers for template data that is no set of key-value pairs,
/// which `TestRenderEmailTemplate` answers the same data with.
const INVALID_DATA: &str = "The template data is not a valid JSON object";

/// The sentence this service answers for content whose `Template` identifies no template at
/// all — naming neither a stored one nor carrying content of its own.
const NO_TEMPLATE: &str = "The template does not exist";

/// The sentence every operation over an `EmailTemplateName` answers for a template the
/// account holds nothing under.
fn no_such_template(name: &str) -> String {
    format!("Template {name} does not exist.")
}

/// The sentence the service answers for a sender that failed the verification check:
/// the Region the call executes in, upper-cased, then the identities that failed.
fn refusal(region: &str, identities: &str) -> String {
    format!(
        "Email address is not verified. The following identities failed the check in region {}: \
         {identities}",
        region.to_uppercase()
    )
}

#[tokio::test]
async fn the_refusal_names_the_region_of_the_call_and_the_identity_that_failed() {
    struct Row<'a> {
        case: &'a str,
        /// The Region the call is signed for.
        region: &'a str,
        /// The Region the refusal must name, which is the Region the call is served
        /// in.
        served_in: &'a str,
        /// An identity to create first, for a row about a sender whose identity
        /// exists but is not verified yet.
        created: Option<&'a str>,
        sender: &'a str,
    }
    let rows = [
        Row {
            case: "an address of no identity the account holds",
            region: "us-west-2",
            served_in: "us-west-2",
            created: None,
            sender: "stranger@example.com",
        },
        Row {
            case: "the same address, from another Region",
            region: "eu-central-1",
            served_in: "eu-central-1",
            created: None,
            sender: "stranger@example.com",
        },
        Row {
            case: "a second sender in the same Region",
            region: "us-west-2",
            served_in: "us-west-2",
            created: None,
            sender: "someone-else@example.org",
        },
        Row {
            case: "an address identity whose verification is not through yet",
            region: "ap-southeast-2",
            served_in: "ap-southeast-2",
            created: Some("pending@example.com"),
            sender: "pending@example.com",
        },
        Row {
            case: "an address inside a domain identity that is not verified yet",
            region: "us-east-2",
            served_in: "us-east-2",
            created: Some("example.net"),
            sender: "sender@example.net",
        },
        Row {
            case: "a Region label with no digits",
            region: "sa-east-1",
            served_in: "sa-east-1",
            created: None,
            sender: "stranger@example.com",
        },
    ];

    for row in rows {
        let ses = Ses::new();
        if let Some(identity) = row.created {
            let created = ses.create_identity(row.region, identity).await;
            assert_eq!(
                created.status, 200,
                "case {:?}: the identity must be created first, got {} {:?}",
                row.case, created.status, created.body
            );
        }

        let refused = ses.send(row.region, row.sender).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "MessageRejected"),
            "case {:?}: expected a 400 MessageRejected, got {} {:?}",
            row.case,
            refused.status,
            refused.body
        );
        let expected = refusal(row.served_in, row.sender);
        assert_eq!(
            refused.message(),
            expected,
            "case {:?}: expected the message {expected:?}, got {:?}",
            row.case,
            refused.message()
        );
    }
}

#[tokio::test]
async fn a_verified_sender_is_accepted_in_whatever_region_the_call_names() {
    // The refusal above is about the verification check, not about the Region: an
    // identity this account holds, once its verification is through, sends from every
    // Region the call may name.
    for region in ["us-east-1", "us-west-2", "eu-west-3"] {
        let ses = Ses::new();
        let created = ses.create_identity(region, "verified.example").await;
        assert_eq!(
            created.status, 200,
            "case {region:?}: the identity must be created first, got {} {:?}",
            created.status, created.body
        );
        ses.settle_identities();

        let sent = ses.send(region, "sender@verified.example").await;
        assert_eq!(
            sent.status, 200,
            "case {region:?}: a verified sender is accepted, got {} {:?}",
            sent.status, sent.body
        );
        assert!(
            sent.body.get("MessageId").and_then(Value::as_str).is_some(),
            "case {region:?}: expected a MessageId, got {:?}",
            sent.body
        );
    }
}

/// How a templated case identifies the template to render the message from.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// The stored template, by `TemplateName`.
    Stored,
    /// A name the account holds nothing under, by `TemplateName`.
    Absent(&'static str),
    /// The ARN addressing such a name, by `TemplateArn`.
    AbsentArn(&'static str),
    /// Content the request carries inline, naming no stored template at all.
    Inline,
    /// A `Template` identifying no template at all: no name, no ARN, no content of its own.
    Nothing,
}

/// What the send must answer.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 400 `MessageRejected`, with the not-verified sentence for the row's Region and
    /// sender.
    IdentityGate,
    /// 400 `BadRequestException`, with this sentence.
    BadRequest(&'static str),
    /// 404 `NotFoundException`, for a template of this name.
    NotFound(&'static str),
    /// 200, with a `MessageId`.
    Accepted,
}

/// One row of the ordering table: a templated send, and the one thing it must be refused
/// (or accepted) for.
struct TemplatedRow {
    case: &'static str,
    /// The Region the call is signed for.
    region: &'static str,
    /// An identity to create before the send, when the row has one.
    identity: Option<&'static str>,
    /// Whether the verification the clock owes that identity is through.
    settled: bool,
    sender: &'static str,
    names: Names,
    /// `Template$TemplateData` as the request carries it, or no member at all.
    data: Option<&'static str>,
    expect: Expect,
}

#[tokio::test]
async fn template_data_is_refused_first_then_the_sender_then_the_template_name() {
    // The rows span the Region, the class of the sender (no identity at all, an identity
    // whose verification is not through, a verified domain identity), how the content
    // identifies the template (a stored name, a name the account holds nothing under, the
    // ARN addressing such a name, content carried inline, nothing at all) and the form of
    // the template data (well-formed, truncated, a JSON value that is no object, no JSON at
    // all, and the member left out entirely).
    let rows = [
        TemplatedRow {
            case: "an unverified sender and a stored template: the identity gate",
            region: "us-west-2",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Stored,
            data: Some(DATA),
            expect: Expect::IdentityGate,
        },
        TemplatedRow {
            case: "an unverified sender and a template name that does not exist: the gate \
                   masks the name",
            region: "us-west-2",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Absent("absent"),
            data: Some(DATA),
            expect: Expect::IdentityGate,
        },
        TemplatedRow {
            case: "the same, in another Region and naming the absent template by ARN",
            region: "eu-central-1",
            identity: None,
            settled: false,
            sender: "nobody@example.net",
            names: Names::AbsentArn("absent"),
            data: Some(DATA),
            expect: Expect::IdentityGate,
        },
        TemplatedRow {
            case: "an identity whose verification is not through yet, with no TemplateData \
                   member at all",
            region: "ap-southeast-2",
            identity: Some("pending@example.com"),
            settled: false,
            sender: "pending@example.com",
            names: Names::Stored,
            data: None,
            expect: Expect::IdentityGate,
        },
        TemplatedRow {
            case: "an address inside a domain identity that is not verified yet, with the \
                   template content carried inline",
            region: "us-east-2",
            identity: Some("example.net"),
            settled: false,
            sender: "sender@example.net",
            names: Names::Inline,
            data: Some(DATA),
            expect: Expect::IdentityGate,
        },
        TemplatedRow {
            case: "truncated template data, with an unverified sender: the data is refused \
                   first",
            region: "us-west-2",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Stored,
            data: Some("{\"name\": "),
            expect: Expect::BadRequest(INVALID_DATA),
        },
        TemplatedRow {
            case: "a JSON list rather than a set of key-value pairs, with an unverified \
                   sender",
            region: "sa-east-1",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Stored,
            data: Some("[1,2]"),
            expect: Expect::BadRequest(INVALID_DATA),
        },
        TemplatedRow {
            case: "no JSON at all, with an unverified sender and a template name that does \
                   not exist",
            region: "us-west-2",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Absent("absent"),
            data: Some("name=Ada"),
            expect: Expect::BadRequest(INVALID_DATA),
        },
        TemplatedRow {
            case: "a JSON string rather than an object, with the content carried inline and \
                   an unverified sender",
            region: "eu-west-3",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Inline,
            data: Some("\"Ada\""),
            expect: Expect::BadRequest(INVALID_DATA),
        },
        TemplatedRow {
            case: "content identifying no template at all, with an unverified sender: the \
                   request states no message to render",
            region: "us-west-2",
            identity: None,
            settled: false,
            sender: "stranger@example.com",
            names: Names::Nothing,
            data: Some(DATA),
            expect: Expect::BadRequest(NO_TEMPLATE),
        },
        TemplatedRow {
            case: "the same from a VERIFIED sender",
            region: "eu-central-1",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::Nothing,
            data: None,
            expect: Expect::BadRequest(NO_TEMPLATE),
        },
        TemplatedRow {
            case: "malformed data from a VERIFIED sender: the same refusal",
            region: "us-west-2",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::Stored,
            data: Some("{\"name\": "),
            expect: Expect::BadRequest(INVALID_DATA),
        },
        TemplatedRow {
            case: "a verified sender and a template name that does not exist: the name is \
                   resolved once the gate is passed",
            region: "us-west-2",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::Absent("absent"),
            data: Some(DATA),
            expect: Expect::NotFound("absent"),
        },
        TemplatedRow {
            case: "the same by ARN, in another Region, for a one-character name",
            region: "ap-northeast-1",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::AbsentArn("x"),
            data: Some(DATA),
            expect: Expect::NotFound("x"),
        },
        TemplatedRow {
            case: "a verified sender, the stored template and well-formed data",
            region: "us-west-2",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::Stored,
            data: Some(DATA),
            expect: Expect::Accepted,
        },
        TemplatedRow {
            case: "a verified sender and the stored template, with no TemplateData member \
                   at all",
            region: "eu-central-1",
            identity: Some("verified.example"),
            settled: true,
            sender: "ada@verified.example",
            names: Names::Stored,
            data: None,
            expect: Expect::Accepted,
        },
    ];

    for row in rows {
        let ses = Ses::new();
        let created = ses.create_template(row.region).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the template must be saved first, got {} {:?}",
            row.case, created.status, created.body
        );
        if let Some(identity) = row.identity {
            let created = ses.create_identity(row.region, identity).await;
            assert_eq!(
                created.status, 200,
                "case {:?}: the identity must be created first, got {} {:?}",
                row.case, created.status, created.body
            );
        }
        if row.settled {
            ses.settle_identities();
        }

        let mut template = match row.names {
            Names::Stored => json!({"TemplateName": TEMPLATE}),
            Names::Absent(name) => json!({"TemplateName": name}),
            Names::AbsentArn(name) => json!({
                "TemplateArn": format!(
                    "arn:aws:ses:{}:{}:template/{name}",
                    row.region,
                    engine::emulator_account_id()
                ),
            }),
            Names::Inline => json!({
                "TemplateContent": {"Subject": SUBJECT, "Text": TEXT, "Html": HTML},
            }),
            Names::Nothing => json!({}),
        };
        if let Some(data) = row.data {
            template["TemplateData"] = json!(data);
        }

        let reply = ses
            .send_content(row.region, row.sender, json!({"Template": template}))
            .await;
        let (status, code, message) = match row.expect {
            Expect::IdentityGate => (400, "MessageRejected", refusal(row.region, row.sender)),
            Expect::BadRequest(sentence) => (400, "BadRequestException", sentence.to_string()),
            Expect::NotFound(name) => (404, "NotFoundException", no_such_template(name)),
            Expect::Accepted => (200, "", String::new()),
        };
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (status, code),
            "case {:?}: expected {status} {code:?}, got {} {:?}",
            row.case,
            reply.status,
            reply.body
        );
        if !message.is_empty() {
            assert_eq!(
                reply.message(),
                message,
                "case {:?}: expected the message {message:?}, got {:?}",
                row.case,
                reply.message()
            );
        }

        // A refused send mints no id, appends nothing to the log and charges no quota; an
        // accepted one does all three, for its one recipient.
        let (logged, charged) = ses.accepted();
        let expected_log = match row.expect {
            Expect::Accepted => (1, 1.0),
            _ => (0, 0.0),
        };
        assert_eq!(
            (logged, charged),
            expected_log,
            "case {:?}: expected {expected_log:?} as (messages logged, quota charged), got \
             ({logged}, {charged})",
            row.case
        );
        assert_eq!(
            !reply.text("/MessageId").is_empty(),
            matches!(row.expect, Expect::Accepted),
            "case {:?}: expected a MessageId only on an accepted send, got {:?}",
            row.case,
            reply.body
        );
    }
}

#[tokio::test]
async fn inline_template_content_past_the_bound_is_refused_before_the_gates() {
    // The bound on the content a request carries INLINE is measured from the request
    // alone — `crate::state::email_template_size` against `MAX_EMAIL_TEMPLATE_BYTES`, the
    // same measure `CreateEmailTemplate` bounds a stored template by — so it is refused
    // ahead of the sending-identity gate whatever state the sender is in. The rows differ
    // along how far past the bound the content is and along the sender's state.
    let region = "us-west-2";
    let rows: &[(&str, usize, bool)] = &[
        ("one byte past the bound, unverified sender", 1, false),
        ("far past the bound, unverified sender", 4096, false),
        ("one byte past the bound, verified sender", 1, true),
    ];

    for (case, over, settled) in rows {
        let ses = Ses::new();
        let sender = if *settled {
            assert_eq!(
                ses.create_identity(region, "verified.example").await.status,
                200,
                "case {case:?}: the identity must be created first"
            );
            ses.settle_identities();
            "ada@verified.example"
        } else {
            "stranger@example.com"
        };

        let text = "x".repeat(MAX_EMAIL_TEMPLATE_BYTES + over - SUBJECT.len());
        let size = SUBJECT.len() + text.len();
        let reply = ses
            .send_content(
                region,
                sender,
                json!({"Template": {
                    "TemplateContent": {"Subject": SUBJECT, "Text": text},
                    "TemplateData": DATA,
                }}),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {} {:?}",
            reply.status,
            reply.body
        );
        let expected = format!(
            "The template is {size} bytes, which is larger than the maximum template size \
             of {MAX_EMAIL_TEMPLATE_BYTES} bytes."
        );
        assert_eq!(
            reply.message(),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            reply.message()
        );
    }
}

#[tokio::test]
async fn an_accepted_templated_send_carries_the_rendered_message() {
    // Once the gates are passed, the message the send carries is the stored template with
    // the request's values substituted into it — the same substitution
    // `TestRenderEmailTemplate` previews, which is the positive control here. The rows
    // differ along the value substituted, including a variable the data states nothing for;
    // the last flag says whether the PREVIEW renders that data too, since
    // `TestRenderEmailTemplate` requires a value for every variable the template
    // references while a send renders one the data does not state as nothing.
    let rows: &[(&str, Option<&str>, &str, bool)] = &[
        (
            "a value for the variable",
            Some("{\"name\":\"Ada\"}"),
            "Ada",
            true,
        ),
        (
            "another value, beside a key the template does not use",
            Some("{\"name\":\"Grace\",\"unused\":\"x\"}"),
            "Grace",
            true,
        ),
        (
            "data stating no value for the variable",
            Some("{\"other\":\"x\"}"),
            "",
            false,
        ),
        ("no TemplateData member at all", None, "", false),
    ];
    let region = "us-west-2";

    for (case, data, substituted, previews) in rows {
        let ses = Ses::new();
        assert_eq!(
            ses.create_template(region).await.status,
            200,
            "case {case:?}: the template must be saved first"
        );
        assert_eq!(
            ses.create_identity(region, "verified.example").await.status,
            200,
            "case {case:?}: the identity must be created first"
        );
        ses.settle_identities();

        let mut template = json!({"TemplateName": TEMPLATE});
        if let Some(data) = data {
            template["TemplateData"] = json!(data);
        }
        let sent = ses
            .send_content(region, "ada@verified.example", json!({"Template": template}))
            .await;
        assert_eq!(
            sent.status, 200,
            "case {case:?}: expected the send to be accepted, got {} {:?}",
            sent.status, sent.body
        );
        let message_id = sent.text("/MessageId");
        assert!(
            !message_id.is_empty(),
            "case {case:?}: expected a MessageId, got {:?}",
            sent.body
        );

        // Read the accepted message back through an implemented reader: the subject it
        // carries is the rendered one, with no reference left unresolved.
        let expected_subject = format!("Hello {substituted}");
        let read = ses.insights(region, &message_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the accepted message to be readable back, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.text("/Subject"),
            expected_subject,
            "case {case:?}: expected the rendered subject {expected_subject:?}, got {:?}",
            read.body
        );

        // The positive control: the same template and data through
        // `TestRenderEmailTemplate` render the same subject line into the preview.
        if *previews {
            let data = data.expect("a row whose preview renders states its data");
            let preview = ses.render(region, data).await;
            assert_eq!(
                preview.status, 200,
                "case {case:?}: expected the preview to render, got {} {:?}",
                preview.status, preview.body
            );
            assert!(
                preview.text("/RenderedTemplate").contains(&expected_subject),
                "case {case:?}: expected the preview to carry {expected_subject:?}, got {:?}",
                preview.body
            );
        }
    }
}
