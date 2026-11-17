// SPDX-License-Identifier: CC0-1.0

//! What puts a message INTO WorkMail's message flow, across the one emulator: mail that
//! Amazon SES received for a mailbox of a WorkMail organization.
//!
//! RULE: a message Amazon SES accepts for a recipient this account receives mail for is
//! carried out through the actions of the active receipt rule set's matching rules, and a
//! `WorkmailAction` — "this action calls Amazon WorkMail" — hands that message to the
//! WorkMail organization its `OrganizationArn` names, so the message enters that
//! organization's email flow and is in transit in that Region's message flow, addressable
//! through `GetRawMessageContent` and `PutRawMessageContent` by the id the send was
//! accepted under, for as long as the organization really holds a mailbox for one of the
//! recipients the rule matched.
//!
//! WorkMail Message Flow models no create, so this is the ONLY producer of an in-transit
//! message a client that makes AWS calls can reach — and no single crate's test target can
//! ask for it: the organization and its mailbox are made through AMAZON WORKMAIL's own
//! operations, the mail is accepted by AMAZON SES's, the rewritten MIME is uploaded through
//! AMAZON S3's, and the message is read and updated through WORKMAIL MESSAGE FLOW's. All
//! four are mounted here on the one engine the router mounts them on, and every step goes
//! over the wire.
//!
//! The rows differ along what the rule generalises over: which mailbox of the organization
//! the mail is for, the form of the rule's recipient condition (an address, the domain, none
//! at all), the bytes and size of the message, a header a preceding action adds to it, and —
//! for the rows where nothing enters the flow — whether the ARN names an organization of
//! this account in this Region at all, whether the recipient is REGISTERED to WorkMail, and
//! whether the rule names the action in the first place.
//!
//! A message is received AFTER it is accepted, so every assertion waits for the CONDITION —
//! the message being in transit, or the object the LAST step of that same message writes —
//! rather than for a span of time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for, which is where the organization, the receipt
/// rule set and the message flow all live.
const REGION: &str = "us-east-1";

/// A second Region, for the row whose organization ARN is not the Region the mail was
/// received in.
const OTHER_REGION: &str = "eu-west-1";

/// The API version Amazon SES's query API carries on every request.
const SES_VERSION: &str = "2010-12-01";

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body to
/// return, so S3 reports `204 No Content`.
const BUCKET_POLICY_STORED: u16 = 204;

/// The mail domain the organization holds and Amazon SES has verified, so the account both
/// sends from it and receives mail for it.
const DOMAIN: &str = "flow.example.com";

/// The mailbox every message below is sent from.
const SENDER: &str = "sender@flow.example.com";

/// The mailbox the organization registers and the mail is addressed to.
const RECIPIENT: &str = "rcpt@flow.example.com";

/// A second registered mailbox of the same organization.
const OTHER_RECIPIENT: &str = "second@flow.example.com";

/// An `OrganizationId` of the modelled form that names no organization.
const ABSENT_ORGANIZATION: &str = "m-ffffffffffffffffffffffffffffffff";

/// The key prefix the control action saves a message under. Its object is written by the
/// LAST step of a message's delivery, which is what tells "did not enter the flow" from
/// "has not entered it yet".
const CONTROL_PREFIX: &str = "control/";

/// How long a row waits for a condition before giving up on it.
///
/// Generously longer than an in-process delivery takes: what a row asserts is the message,
/// never how quickly it arrived, so a loaded host makes this wait more patient and never
/// less.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long a row waits between two looks while waiting for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The account this emulator serves, which owns every organization, bucket and rule here.
fn account() -> String {
    engine::EMULATOR_ACCOUNT_ID.to_string()
}

/// One wire answer, reduced to what the next step branches on.
struct Answer {
    status: u16,
    error_type: String,
    body: Bytes,
}

impl Answer {
    /// The body rendered for an assertion message, without dumping a whole message into
    /// the log.
    fn summary(&self) -> String {
        let shown = String::from_utf8_lossy(&self.body[..self.body.len().min(120)]);
        format!(
            "{} byte(s) {shown:?}{}",
            self.body.len(),
            if self.body.len() > 120 { "…" } else { "" }
        )
    }

    /// The body as text, for an XML or JSON answer.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One request this suite makes, as the wire carries it.
struct Call<'a> {
    /// The HTTP method.
    method: &'a str,
    /// The signing name, which is what the router resolves the service from.
    signing_name: &'a str,
    /// The Region of the credential scope, which is the Region the call is made in.
    region: &'a str,
    /// The request URI.
    path: &'a str,
    /// The `Content-Type`, for a call that carries a body in a named wire form.
    content_type: Option<&'a str>,
    /// The `X-Amz-Target`, for an `awsJson` call.
    target: Option<&'a str>,
    /// The request body.
    body: Vec<u8>,
}

/// Amazon SES, AMAZON WORKMAIL, WorkMail Message Flow and Amazon S3 mounted on ONE engine,
/// exactly as the router mounts them, with request authentication off so each call carries
/// a credential scope (which is what names the Region and the service) without a computed
/// signature.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_workmail_workmailservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_workmailmessageflow_giraffemessageintransitservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    /// One call of `call.method` to `call.path` on `call.signing_name`'s endpoint.
    async fn call(&self, call: Call<'_>) -> Answer {
        let Call {
            method,
            signing_name,
            region,
            path,
            content_type,
            target,
            body,
        } = call;
        let mut request = ::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("{signing_name}.{region}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            );
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let request = request
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Answer {
            status: response.status().as_u16(),
            error_type,
            body: response.into_body(),
        }
    }

    /// One Amazon SES `awsQuery` call: the action and its form fields.
    async fn ses(&self, action: &str, params: &[(String, String)]) -> Answer {
        let mut body = format!("Action={action}&Version={SES_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.call(Call {
            method: "POST",
            signing_name: "ses",
            region: REGION,
            path: "/",
            content_type: Some("application/x-www-form-urlencoded"),
            target: None,
            body: body.into_bytes(),
        })
        .await
    }

    /// One Amazon SES call that must be served, as its XML body.
    async fn ses_ok(&self, case: &str, action: &str, params: &[(String, String)]) -> String {
        let answered = self.ses(action, params).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected Amazon SES {action} to be served, got {} ({})",
            answered.status,
            answered.summary()
        );
        answered.text()
    }

    /// One AMAZON WORKMAIL `awsJson1_1` call that must be served, as its decoded body.
    async fn workmail_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self
            .call(Call {
                method: "POST",
                signing_name: "workmail",
                region: REGION,
                path: "/",
                content_type: Some("application/x-amz-json-1.1"),
                target: Some(&format!("WorkMailService.{operation}")),
                body: serde_json::to_vec(body).expect("the request body serializes"),
            })
            .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected AMAZON WORKMAIL {operation} to be served, got {} ({})",
            answered.status,
            answered.summary()
        );
        if answered.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&answered.body).unwrap_or(Value::Null)
        }
    }

    /// WorkMail Message Flow `GetRawMessageContent`, whose success payload is the raw MIME
    /// content with no envelope.
    async fn get_raw_message_content(&self, message_id: &str) -> Answer {
        self.call(Call {
            method: "GET",
            signing_name: "workmailmessageflow",
            region: REGION,
            path: &format!("/messages/{message_id}"),
            content_type: None,
            target: None,
            body: Vec::new(),
        })
        .await
    }

    /// WorkMail Message Flow `PutRawMessageContent`, naming an uploaded object.
    async fn put_raw_message_content(&self, message_id: &str, bucket: &str, key: &str) -> Answer {
        let body = json!({ "content": { "s3Reference": { "bucket": bucket, "key": key } } });
        self.call(Call {
            method: "POST",
            signing_name: "workmailmessageflow",
            region: REGION,
            path: &format!("/messages/{message_id}"),
            content_type: Some("application/json"),
            target: None,
            body: serde_json::to_vec(&body).expect("serialize the update body"),
        })
        .await
    }

    /// One Amazon S3 call in [`REGION`], path style.
    async fn s3(&self, method: &str, path: &str, body: Vec<u8>) -> Answer {
        self.call(Call {
            method,
            signing_name: "s3",
            region: REGION,
            path,
            content_type: None,
            target: None,
            body,
        })
        .await
    }

    /// The store as it stands once the clock has carried every requested identity
    /// verification through.
    ///
    /// Asking for an identity's verification only STARTS it: the move the clock owes the
    /// record falls due a span later, so a suite that needs a SENDING identity settles the
    /// store against an instant past that span instead of sleeping for it. This is the only
    /// state any step of this suite reaches directly — every value it asserts on comes off
    /// the wire.
    fn settle_verifications(&self) {
        let state =
            service_com_amazonaws_ses_simpleemailservice::state::service_state(&*self.engine)
                .expect("Amazon SES has a store on the engine it is mounted on");
        let past_the_span = ::std::time::SystemTime::now()
            + service_com_amazonaws_ses_simpleemailservice::state::IDENTITY_VERIFICATION_SPAN;
        state.lock().settle_identities(past_the_span);
    }
}

/// One form field of an Amazon SES query call.
fn field(name: impl Into<String>, value: impl Into<String>) -> (String, String) {
    (name.into(), value.into())
}

/// One action of a receipt rule, as the rows name it.
#[derive(Debug, Clone)]
enum Action {
    /// `WorkmailAction` handing the message to the organization this ARN names.
    HandToWorkMail(String),
    /// `AddHeaderAction` adding `(HeaderName, HeaderValue)`, which every action after it
    /// acts on.
    AddHeader(&'static str, &'static str),
    /// `S3Action` saving the message under `CONTROL_PREFIX` of this bucket — the control
    /// step, whose object says this message's delivery is finished.
    SaveControl(String),
}

/// One rule of a row's rule set.
#[derive(Debug, Clone)]
struct Rule {
    /// `ReceiptRule$Name`.
    name: String,
    /// `ReceiptRule$Recipients`, empty for a rule stating no condition — which "matches all
    /// recipients on all verified domains".
    recipients: Vec<String>,
    /// `ReceiptRule$Actions`, in the order Amazon SES takes them.
    actions: Vec<Action>,
}

/// The `Rule.*` form fields of a `CreateReceiptRule` carrying `rule`.
fn rule_fields(rule: &Rule) -> Vec<(String, String)> {
    let mut params = vec![
        field("Rule.Name", rule.name.clone()),
        field("Rule.Enabled", "true"),
        field("Rule.ScanEnabled", "false"),
    ];
    for (index, recipient) in rule.recipients.iter().enumerate() {
        params.push(field(
            format!("Rule.Recipients.member.{}", index + 1),
            recipient.clone(),
        ));
    }
    for (index, action) in rule.actions.iter().enumerate() {
        let at = format!("Rule.Actions.member.{}", index + 1);
        match action {
            Action::HandToWorkMail(organization_arn) => params.push(field(
                format!("{at}.WorkmailAction.OrganizationArn"),
                organization_arn.clone(),
            )),
            Action::AddHeader(name, value) => {
                params.push(field(format!("{at}.AddHeaderAction.HeaderName"), *name));
                params.push(field(format!("{at}.AddHeaderAction.HeaderValue"), *value));
            }
            Action::SaveControl(bucket) => {
                params.push(field(format!("{at}.S3Action.BucketName"), bucket.clone()));
                params.push(field(
                    format!("{at}.S3Action.ObjectKeyPrefix"),
                    CONTROL_PREFIX,
                ));
            }
        }
    }
    params
}

/// A WorkMail organization holding `DOMAIN`, as `CreateOrganization` makes one, with the ARN
/// WorkMail itself reports for it read back through `DescribeOrganization` — never a value
/// this suite composes.
async fn given_organization(emulator: &Emulator, case: &str, alias: &str) -> (String, String) {
    let created = emulator
        .workmail_ok(
            case,
            "CreateOrganization",
            &json!({
                "Alias": alias,
                "Domains": [{ "DomainName": DOMAIN }],
            }),
        )
        .await;
    let organization_id = created["OrganizationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateOrganization returned no id: {created}"))
        .to_string();
    let described = emulator
        .workmail_ok(
            case,
            "DescribeOrganization",
            &json!({ "OrganizationId": organization_id }),
        )
        .await;
    let arn = described["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeOrganization returned no ARN: {described}"))
        .to_string();
    (organization_id, arn)
}

/// A user of `organization_id`, registered to WorkMail at `email` when `registered` — which
/// is what gives it a mailbox ("enables the user … to use WorkMail", `RegisterToWorkMail`).
async fn given_user(
    emulator: &Emulator,
    case: &str,
    organization_id: &str,
    email: &str,
    registered: bool,
) {
    let name = email.split('@').next().unwrap_or(email);
    let created = emulator
        .workmail_ok(
            case,
            "CreateUser",
            &json!({
                "OrganizationId": organization_id,
                "Name": name,
                "DisplayName": name,
                "Password": "Not-a-real-password-1",
            }),
        )
        .await;
    let user_id = created["UserId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateUser returned no id: {created}"))
        .to_string();
    if registered {
        emulator
            .workmail_ok(
                case,
                "RegisterToWorkMail",
                &json!({
                    "OrganizationId": organization_id,
                    "EntityId": user_id,
                    "Email": email,
                }),
            )
            .await;
    }
}

/// `VerifyDomainIdentity` for the domain this account sends from and receives mail for, and
/// then the store settled past the span that request leaves the record pending for.
async fn given_verified_domain(emulator: &Emulator, case: &str) {
    emulator
        .ses_ok(case, "VerifyDomainIdentity", &[field("Domain", DOMAIN)])
        .await;
    emulator.settle_verifications();
}

/// A bucket Amazon SES may write a received message into: created through Amazon S3's own
/// `CreateBucket`, carrying the inbound-mail bucket policy — the service principal granted
/// `s3:PutObject`, scoped to this account.
async fn given_control_bucket(emulator: &Emulator, case: &str, bucket: &str) {
    let created = emulator.s3("PUT", &format!("/{bucket}"), Vec::new()).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected the S3 CreateBucket of {bucket} to succeed, got {} ({})",
        created.status,
        created.summary()
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSESPuts",
            "Effect": "Allow",
            "Principal": { "Service": "ses.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": { "StringEquals": { "AWS:SourceAccount": account() } },
        }],
    });
    let granted = emulator
        .s3(
            "PUT",
            &format!("/{bucket}?policy"),
            serde_json::to_vec(&policy).expect("serialize the bucket policy"),
        )
        .await;
    assert_eq!(
        granted.status,
        BUCKET_POLICY_STORED,
        "case {case:?}: expected the S3 PutBucketPolicy of {bucket} to be stored, got {} ({})",
        granted.status,
        granted.summary()
    );
}

/// A bucket WorkMail may read the updated message content out of: the grant WorkMail's own
/// instructions name — `s3:GetObject` to WorkMail's service principal.
async fn given_rewrite_bucket(emulator: &Emulator, case: &str, bucket: &str) {
    let created = emulator.s3("PUT", &format!("/{bucket}"), Vec::new()).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected the S3 CreateBucket of {bucket} to succeed, got {} ({})",
        created.status,
        created.summary()
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowWorkMailToReadMessageContent",
            "Effect": "Allow",
            "Principal": { "Service": "workmail.amazonaws.com" },
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }],
    });
    let granted = emulator
        .s3(
            "PUT",
            &format!("/{bucket}?policy"),
            serde_json::to_vec(&policy).expect("serialize the bucket policy"),
        )
        .await;
    assert_eq!(
        granted.status,
        BUCKET_POLICY_STORED,
        "case {case:?}: expected the S3 PutBucketPolicy of {bucket} to be stored, got {} ({})",
        granted.status,
        granted.summary()
    );
}

/// `CreateReceiptRuleSet` for `rule_set`, one `CreateReceiptRule` per rule in order, and
/// `SetActiveReceiptRuleSet` — the one rule set the account evaluates.
async fn given_active_rule_set(emulator: &Emulator, case: &str, rule_set: &str, rules: &[Rule]) {
    emulator
        .ses_ok(
            case,
            "CreateReceiptRuleSet",
            &[field("RuleSetName", rule_set)],
        )
        .await;
    for (index, rule) in rules.iter().enumerate() {
        let mut params = vec![field("RuleSetName", rule_set)];
        // "The name of an existing rule after which the new rule is placed. If this
        // parameter is null, the new rule is inserted at the beginning of the rule list"
        // (`CreateReceiptRuleRequest$After`), so each rule after the first names the one
        // before it and the set evaluates its rules in the order these rows list.
        if let Some(previous) = index.checked_sub(1).map(|before| rules[before].name.clone()) {
            params.push(field("After", previous));
        }
        params.extend(rule_fields(rule));
        emulator.ses_ok(case, "CreateReceiptRule", &params).await;
    }
    emulator
        .ses_ok(
            case,
            "SetActiveReceiptRuleSet",
            &[field("RuleSetName", rule_set)],
        )
        .await;
}

/// `SendRawEmail` carrying `raw` for `recipients`, answering the `MessageId` it was accepted
/// under — the id an in-transit message is addressed by.
///
/// `RawMessage$Data` is a blob, so the awsQuery wire carries it base64-encoded.
async fn send_raw(emulator: &Emulator, case: &str, recipients: &[&str], raw: &[u8]) -> String {
    let mut params = vec![
        field("Source", SENDER),
        field("RawMessage.Data", ::aws_smithy_types::base64::encode(raw)),
    ];
    for (index, recipient) in recipients.iter().enumerate() {
        params.push(field(
            format!("Destinations.member.{}", index + 1),
            *recipient,
        ));
    }
    let body = emulator.ses_ok(case, "SendRawEmail", &params).await;
    element(&body, "MessageId")
}

/// The content WorkMail Message Flow reports for `message_id`, waited for until the message
/// is in transit.
///
/// The wait is for the CONDITION — the message being in the flow — looked for again every
/// [`POLL_INTERVAL`] until [`DELIVERY_DEADLINE`] passes, because a message is received after
/// it is accepted and nothing a sender is told says when.
async fn settled_in_transit(emulator: &Emulator, case: &str, message_id: &str) -> Bytes {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let read = emulator.get_raw_message_content(message_id).await;
        if read.status == 200 {
            return read.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: message {message_id} was not in transit within {deadline_secs}s: \
             GetRawMessageContent answered {} {} ({})",
            read.status,
            read.error_type,
            read.summary(),
            deadline_secs = DELIVERY_DEADLINE.as_secs(),
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The barrier for a row that expects NO in-transit message: the object the control step of
/// that same message writes, waited for until Amazon S3 holds it.
///
/// The steps of ONE message are carried out in order, and the control rule is the LAST rule
/// of the row's active set, so its object being there is what makes "the message did not
/// enter the flow" an assertion rather than a race.
async fn settled_control_object(emulator: &Emulator, case: &str, bucket: &str, message_id: &str) {
    let key = format!("/{bucket}/{CONTROL_PREFIX}{message_id}");
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let read = emulator.s3("GET", &key, Vec::new()).await;
        if read.status == 200 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the control object {key} was not written within {deadline_secs}s: \
             S3 answered {} ({})",
            read.status,
            read.summary(),
            deadline_secs = DELIVERY_DEADLINE.as_secs(),
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A well-formed RFC 5322 message: the `Date` and `From` fields §3.6 requires every message
/// to carry, addressed `to`, with `subject` and `body`.
fn mime(to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "Date: Sat, 24 Oct 2026 10:13:52 +0000\r\nFrom: {SENDER}\r\nTo: {to}\r\n\
         Subject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// A message with `paragraphs` body lines, so one row's message is far larger than the
/// others' without any line approaching the RFC 5321 line limit.
fn long_mime(to: &str, paragraphs: usize) -> Vec<u8> {
    let mut content = mime(to, "a longer message", "the first paragraph");
    for index in 0..paragraphs {
        content.extend(format!("paragraph {index}\r\n").as_bytes());
    }
    content
}

/// One row of the delivery table.
struct Delivered {
    /// What this row is called in every assertion it makes.
    case: &'static str,
    /// `ReceiptRule$Recipients` of the rule that hands the message over.
    recipients: Vec<String>,
    /// The envelope recipients the message is sent to.
    destinations: Vec<&'static str>,
    /// The header an `AddHeaderAction` adds before the hand-over, when the row has one.
    added_header: Option<(&'static str, &'static str)>,
    /// The message's own bytes, as sent.
    raw: Vec<u8>,
}

/// The rule: mail Amazon SES receives for a mailbox of the organization a `WorkmailAction`
/// names enters that organization's email flow, so the message is in transit under the id
/// the send was accepted under and `GetRawMessageContent` reports the bytes it arrived with.
#[tokio::test]
async fn mail_received_for_a_mailbox_enters_the_organizations_email_flow() {
    let rows = vec![
        Delivered {
            case: "the rule names the recipient's own address",
            recipients: vec![RECIPIENT.to_string()],
            destinations: vec![RECIPIENT],
            added_header: None,
            raw: mime(RECIPIENT, "into the flow", "the body the message arrived with"),
        },
        Delivered {
            case: "the rule names the recipient's domain",
            recipients: vec![DOMAIN.to_string()],
            destinations: vec![RECIPIENT],
            added_header: None,
            raw: mime(RECIPIENT, "matched on the domain", "another body"),
        },
        Delivered {
            case: "the rule states no recipient condition at all",
            recipients: Vec::new(),
            destinations: vec![RECIPIENT],
            added_header: None,
            raw: mime(RECIPIENT, "matched on nothing", "a third body"),
        },
        Delivered {
            case: "a second mailbox of the same organization",
            recipients: vec![OTHER_RECIPIENT.to_string()],
            destinations: vec![OTHER_RECIPIENT],
            added_header: None,
            raw: mime(OTHER_RECIPIENT, "the other mailbox", "its body"),
        },
        Delivered {
            case: "a message far larger than the others",
            recipients: vec![RECIPIENT.to_string()],
            destinations: vec![RECIPIENT],
            added_header: None,
            raw: long_mime(RECIPIENT, 512),
        },
        Delivered {
            case: "a header an earlier action adds is in the content that entered the flow",
            recipients: vec![RECIPIENT.to_string()],
            destinations: vec![RECIPIENT],
            added_header: Some(("X-Flow-Stamp", "added before the hand-over")),
            raw: mime(RECIPIENT, "stamped", "the body"),
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let (organization_id, organization_arn) =
            given_organization(&emulator, case, "flowOrg").await;
        given_user(&emulator, case, &organization_id, RECIPIENT, true).await;
        given_user(&emulator, case, &organization_id, OTHER_RECIPIENT, true).await;
        given_verified_domain(&emulator, case).await;

        let mut actions = Vec::new();
        if let Some((name, value)) = row.added_header {
            actions.push(Action::AddHeader(name, value));
        }
        actions.push(Action::HandToWorkMail(organization_arn));
        given_active_rule_set(
            &emulator,
            case,
            "flow-rules",
            &[Rule {
                name: "hand-to-workmail".to_string(),
                recipients: row.recipients.clone(),
                actions,
            }],
        )
        .await;

        let message_id = send_raw(&emulator, case, &row.destinations, &row.raw).await;
        assert!(
            !message_id.is_empty(),
            "case {case:?}: expected SendRawEmail to answer a MessageId"
        );

        // The content as the steps of this message left it: the bytes that were sent, with
        // the header a preceding `AddHeaderAction` added at the top of the block.
        let expected: Vec<u8> = match row.added_header {
            None => row.raw.clone(),
            Some((name, value)) => {
                let mut stamped = format!("{name}: {value}\r\n").into_bytes();
                stamped.extend_from_slice(&row.raw);
                stamped
            }
        };
        let in_transit = settled_in_transit(&emulator, case, &message_id).await;
        assert_eq!(
            in_transit.as_ref(),
            expected.as_slice(),
            "case {case:?}: expected the {} octet(s) the message entered the flow with, got {} \
             octet(s)",
            expected.len(),
            in_transit.len()
        );

        // A read does not consume the message: the next caller sees the same bytes.
        let again = emulator.get_raw_message_content(&message_id).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected the message to still be in transit after a read, got {} ({})",
            again.status,
            again.summary()
        );
        assert_eq!(
            again.body.as_ref(),
            expected.as_slice(),
            "case {case:?}: a second read reported different content: {}",
            again.summary()
        );
    }
}

/// What the row sets up differently, for the rows where nothing enters the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoFlow {
    /// The action names an ARN of this account and Region whose organization does not
    /// exist.
    UnknownOrganization,
    /// The action names the organization's id in ANOTHER Region, which is not this
    /// organization's ARN.
    OrganizationInAnotherRegion,
    /// The recipient's user exists but was never registered to WorkMail, so the
    /// organization holds no mailbox for it.
    NoMailbox,
    /// The matching rule names no `WorkmailAction` at all — the baseline, which shows the
    /// control barrier itself does not deliver anything.
    NoWorkmailAction,
}

/// The rule's other half: mail the organization holds no mailbox for, and mail no
/// `WorkmailAction` hands over, enters no flow — so the message is not in transit and both
/// modelled operations answer `ResourceNotFoundException` for its id.
#[tokio::test]
async fn mail_no_mailbox_of_an_organization_is_for_enters_no_flow() {
    let bucket = "wmf-email-flow-control";
    let rows: [(&str, NoFlow); 4] = [
        ("an ARN naming no organization", NoFlow::UnknownOrganization),
        (
            "the organization's id in another Region",
            NoFlow::OrganizationInAnotherRegion,
        ),
        ("a user that was never registered", NoFlow::NoMailbox),
        ("a rule naming no WorkmailAction", NoFlow::NoWorkmailAction),
    ];

    for (case, setup) in rows {
        let emulator = Emulator::new();
        let (organization_id, organization_arn) =
            given_organization(&emulator, case, "flowOrg").await;
        given_user(
            &emulator,
            case,
            &organization_id,
            RECIPIENT,
            setup != NoFlow::NoMailbox,
        )
        .await;
        given_verified_domain(&emulator, case).await;
        given_control_bucket(&emulator, case, bucket).await;

        let named_arn = match setup {
            NoFlow::UnknownOrganization => Some(format!(
                "arn:aws:workmail:{REGION}:{}:organization/{ABSENT_ORGANIZATION}",
                account()
            )),
            NoFlow::OrganizationInAnotherRegion => Some(format!(
                "arn:aws:workmail:{OTHER_REGION}:{}:organization/{organization_id}",
                account()
            )),
            NoFlow::NoMailbox => Some(organization_arn.clone()),
            NoFlow::NoWorkmailAction => None,
        };
        let mut rules = Vec::new();
        if let Some(organization_arn) = named_arn {
            rules.push(Rule {
                name: "hand-to-workmail".to_string(),
                recipients: vec![RECIPIENT.to_string()],
                actions: vec![Action::HandToWorkMail(organization_arn)],
            });
        }
        // The control rule is LAST, so its object is written by the last step of this
        // message's delivery.
        rules.push(Rule {
            name: "control".to_string(),
            recipients: Vec::new(),
            actions: vec![Action::SaveControl(bucket.to_string())],
        });
        given_active_rule_set(&emulator, case, "flow-rules", &rules).await;

        let raw = mime(RECIPIENT, "no flow to enter", "the body");
        let message_id = send_raw(&emulator, case, &[RECIPIENT], &raw).await;
        settled_control_object(&emulator, case, bucket, &message_id).await;

        let read = emulator.get_raw_message_content(&message_id).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected HTTP 404 from GetRawMessageContent, got {} ({})",
            read.status,
            read.summary()
        );
        assert_eq!(
            read.error_type, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?} ({})",
            read.error_type,
            read.summary()
        );
        let update = emulator
            .put_raw_message_content(&message_id, bucket, "whatever")
            .await;
        assert_eq!(
            update.status, 404,
            "case {case:?}: expected HTTP 404 from PutRawMessageContent, got {} ({})",
            update.status,
            update.summary()
        );
    }
}

/// The workflow the message flow exists for, over the wire alone: the message arrives in
/// transit, a client reads its raw content, uploads rewritten MIME to Amazon S3, installs it
/// with `PutRawMessageContent`, and the next read reports the rewritten bytes.
#[tokio::test]
async fn a_message_that_entered_the_flow_takes_an_update_from_s3() {
    let case = "the message-flow workflow end to end";
    let bucket = "wmf-email-flow-rewrites";
    let key = "updated/message-mime";
    let emulator = Emulator::new();

    let (organization_id, organization_arn) = given_organization(&emulator, case, "flowOrg").await;
    given_user(&emulator, case, &organization_id, RECIPIENT, true).await;
    given_verified_domain(&emulator, case).await;
    given_rewrite_bucket(&emulator, case, bucket).await;
    given_active_rule_set(
        &emulator,
        case,
        "flow-rules",
        &[Rule {
            name: "hand-to-workmail".to_string(),
            recipients: vec![RECIPIENT.to_string()],
            actions: vec![Action::HandToWorkMail(organization_arn)],
        }],
    )
    .await;

    let arrived = mime(RECIPIENT, "as it arrived", "the body the message arrived with");
    let message_id = send_raw(&emulator, case, &[RECIPIENT], &arrived).await;
    let in_transit = settled_in_transit(&emulator, case, &message_id).await;
    assert_eq!(
        in_transit.as_ref(),
        arrived.as_slice(),
        "case {case:?}: expected the bytes the message arrived with, got {} octet(s)",
        in_transit.len()
    );

    // What a Run Lambda rule does with the content it read: rewrite it and upload it.
    let rewritten = mime(RECIPIENT, "as it was rewritten", "the rewritten body");
    let uploaded = emulator
        .s3("PUT", &format!("/{bucket}/{key}"), rewritten.clone())
        .await;
    assert_eq!(
        uploaded.status,
        200,
        "case {case:?}: expected the S3 PutObject to succeed, got {} ({})",
        uploaded.status,
        uploaded.summary()
    );
    let update = emulator
        .put_raw_message_content(&message_id, bucket, key)
        .await;
    assert_eq!(
        update.status,
        200,
        "case {case:?}: expected HTTP 200 from PutRawMessageContent, got {} ({})",
        update.status,
        update.summary()
    );

    let after = emulator.get_raw_message_content(&message_id).await;
    assert_eq!(
        after.status,
        200,
        "case {case:?}: expected HTTP 200 from GetRawMessageContent, got {} ({})",
        after.status,
        after.summary()
    );
    assert_eq!(
        after.body.as_ref(),
        rewritten.as_slice(),
        "case {case:?}: expected the rewritten {} octet(s), got {}",
        rewritten.len(),
        after.summary()
    );
}
