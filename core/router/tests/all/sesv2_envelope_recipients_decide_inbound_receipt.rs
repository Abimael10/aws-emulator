// SPDX-License-Identifier: CC0-1.0

//! Which recipients of a simple SESv2 send the account's ACTIVE receipt rule set is evaluated
//! against, and what the rule that matches then saves.
//!
//! The rule these rows pin: **a receipt rule's `Recipients` — "the recipient domains and email
//! addresses that the receipt rule applies to. If this field is not specified, this rule matches
//! all recipients on all verified domains" — is compared against the message's ENVELOPE
//! recipients, which for a simple send are every address of `Destination$ToAddresses`,
//! `Destination$CcAddresses` and `Destination$BccAddresses` alike; a rule that matches carries
//! out its actions ONCE for the message, however many of its envelope recipients it matched, so
//! an `S3Action` saves exactly one object at `<ObjectKeyPrefix><MessageId>` of the bucket it
//! names, carrying the RFC 5322 document Amazon SES assembled from the parts the send sent
//! ("you specify the sender, the recipient, and the message body, and Amazon SES assembles the
//! message for you") behind the `DKIM-Signature` it signed for the sending identity — a document
//! whose address fields are the `To` and `Cc` of the send and never its blind-carbon-copy
//! recipients.**
//!
//! Three crates have to agree for that to hold — SESv2 accepts and assembles, Amazon SES holds
//! the account's receipt rule sets and receives, Amazon S3 holds the object — so the suite lives
//! here: a crate-local suite only ever sees one side of it.
//!
//! The rows differ along what the rule generalises over: which field of `Destination` names the
//! address the rule applies to (`To`, `Cc`, `Bcc`), how many of them name one (none, one, the
//! same one twice, two different ones), and whether the rule scans. A send naming its recipient
//! in `ToAddresses` is ONE of those rows.
//!
//! Every rule set here is written through Amazon SES's own `CreateReceiptRuleSet` /
//! `CreateReceiptRule` / `SetActiveReceiptRuleSet` and read back through its own
//! `DescribeActiveReceiptRuleSet`, every bucket through Amazon S3's own `CreateBucket`, and
//! every object asserted on through Amazon S3's own `ListObjectsV2` and `GetObject` — so what is
//! asserted is what those services really hold.
//!
//! A message is RECEIVED behind its acceptance, so no row waits for a span of time. Each row's
//! active set holds, after the rule under test, a CONTROL rule stating no recipient condition at
//! all: the steps of one message are carried out in order, so a row waits for the CONTROL
//! object — the last one that message writes — and only then reads the listing that says whether
//! the rule under test saved anything. That is what lets a row tell "not saved" from "not saved
//! yet".

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The API version Amazon SES's query API carries on every request.
const SES_VERSION: &str = "2010-12-01";

/// The domain identity the account holds, and therefore the domain it both sends from and
/// receives mail for.
const DOMAIN: &str = "receiving.example.com";

/// The mailbox every send below is from.
const SENDER: &str = "postmaster@receiving.example.com";

/// The two addresses the rule under test applies to.
const MATCHED: &str = "inbox@receiving.example.com";
const SECOND_MATCHED: &str = "orders@receiving.example.com";

/// An address of the SAME verified domain the rule under test does not apply to — so a row
/// addressed only to it is received by the account and matched by the control rule, and not by
/// the rule under test.
const UNMATCHED: &str = "noreply@receiving.example.com";

/// `S3Action$ObjectKeyPrefix` of the rule under test, and of the control rule.
const INBOX_PREFIX: &str = "inbox/";
const CONTROL_PREFIX: &str = "control/";

/// `ReceiptRule$Name` of the rule under test, and of the control rule that follows it.
const RULE: &str = "to-the-matched-addresses";
const CONTROL_RULE: &str = "every-recipient";

/// How long a wait for a condition goes on for, and how often it looks again.
///
/// Many times longer than the in-process work takes, and deliberately so: what a row asserts is
/// the state reached, never how quickly it was reached, so a host running the whole workspace's
/// suites at once makes these waits more patient and never less. A row that reaches the state
/// returns as soon as it sees it, so the span only ever bounds a row that is going to fail.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(50);

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body to
/// return, so S3 reports `204 No Content`.
const BUCKET_POLICY_STORED: u16 = 204;

/// The two refusals the ROUTER itself answers, which no call here may provoke.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// `Received-SPF` (RFC 7208 §9.1) and `Authentication-Results` (RFC 7601 §2), and the two
/// fields Amazon SES reports each scan's verdict in — the fields a rule whose `ScanEnabled` is
/// set has stamped on the message, and a rule that does not scan has not.
const SCAN_HEADERS: &[&str] = &[
    "received-spf",
    "authentication-results",
    "x-ses-spam-verdict",
    "x-ses-virus-verdict",
];

/// The account this emulator serves, which owns the identity, the buckets and the rule sets
/// here.
fn account() -> String {
    engine::EMULATOR_ACCOUNT_ID.to_string()
}

/// One answered request, reduced to what the next step reads off it.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text, for an XML answer.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body rendered for an assertion message, without dumping a whole message into it.
    fn summary(&self) -> String {
        let shown = String::from_utf8_lossy(&self.body[..self.body.len().min(160)]);
        format!(
            "{} byte(s) {shown:?}{}",
            self.body.len(),
            if self.body.len() > 160 { "…" } else { "" }
        )
    }
}

/// SESv2, Amazon SES and Amazon S3 mounted on ONE engine, exactly as `core/router/src/lib.rs`
/// mounts them, with request authentication off so every call carries a credential scope (which
/// is what names the Region and the service) without a signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One call to `path` on `signing_name`'s endpoint, refusing at the door the two answers the
    /// ROUTER itself produces: a request that reached nobody, or one several services claimed.
    async fn call(
        &self,
        case: &str,
        method: &str,
        signing_name: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> Answer {
        let leg = format!("{signing_name} {method} {path}");
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            );
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        let request = request
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let answered = Answer {
            status: response.status().as_u16(),
            body: response.into_body(),
        };
        let text = answered.text();
        for refusal in ROUTER_REFUSALS {
            assert!(
                !text.contains(refusal),
                "case {case:?}: {leg} must be served by exactly one service, got HTTP {}: {}",
                answered.status,
                answered.summary()
            );
        }
        answered
    }

    /// One SESv2 `restJson1` call, requiring HTTP `expected`, as its decoded body.
    async fn sesv2(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        expected: u16,
    ) -> Value {
        let payload = match &body {
            Some(value) => serde_json::to_vec(value).expect("the request body serializes"),
            None => Vec::new(),
        };
        let answered = self
            .call(case, method, "ses", path, Some("application/json"), payload)
            .await;
        assert_eq!(
            answered.status,
            expected,
            "case {case:?}: SESv2 {method} {path} expected HTTP {expected}, got {} ({})",
            answered.status,
            answered.summary()
        );
        serde_json::from_slice(&answered.body).unwrap_or(Value::Null)
    }

    /// One Amazon SES `awsQuery` call that must be served, as its XML body.
    async fn ses(&self, case: &str, action: &str, params: &[(String, String)]) -> String {
        let mut body = format!("Action={action}&Version={SES_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answered = self
            .call(
                case,
                "POST",
                "ses",
                "/",
                Some("application/x-www-form-urlencoded"),
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: Amazon SES {action} must be served, got {} ({})",
            answered.status,
            answered.summary()
        );
        answered.text()
    }

    /// One Amazon S3 call in [`REGION`], path style.
    async fn s3(&self, case: &str, method: &str, path: &str, body: Vec<u8>) -> Answer {
        self.call(case, method, "s3", path, None, body).await
    }
}

/// One form field of an Amazon SES query call.
fn field(name: impl Into<String>, value: impl Into<String>) -> (String, String) {
    (name.into(), value.into())
}

/// The account's domain identity, verified for sending: created through SESv2's own
/// `CreateEmailIdentity`, and waited for until the verification the clock owes it is due.
async fn given_verified_identity(emulator: &Emulator, case: &str) {
    let created = emulator
        .sesv2(
            case,
            "POST",
            "/v2/email/identities",
            Some(json!({ "EmailIdentity": DOMAIN })),
            200,
        )
        .await;
    assert_eq!(
        created["IdentityType"],
        json!("DOMAIN"),
        "case {case:?}: {DOMAIN} is a DOMAIN identity, got {created}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = emulator
            .sesv2(case, "GET", &format!("/v2/email/identities/{DOMAIN}"), None, 200)
            .await;
        if read["VerifiedForSendingStatus"] == json!(true) {
            assert_eq!(
                read["DkimAttributes"]["Status"],
                json!("SUCCESS"),
                "case {case:?}: a verified domain identity reports its Easy DKIM set up, so the \
                 messages it sends are signed, got {read}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the identity for {DOMAIN} must reach VerifiedForSendingStatus true, \
             last read {read}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// A bucket Amazon SES may write a received message into: created through Amazon S3's own
/// `CreateBucket`, carrying the inbound-mail bucket policy the Amazon SES Developer Guide
/// writes — the service principal granted `s3:PutObject`, scoped to this account.
async fn given_bucket(emulator: &Emulator, case: &str, bucket: &str) {
    let created = emulator
        .s3(case, "PUT", &format!("/{bucket}"), Vec::new())
        .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: the CreateBucket of {bucket} must succeed, got {} ({})",
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
            case,
            "PUT",
            &format!("/{bucket}?policy"),
            serde_json::to_vec(&policy).expect("the bucket policy serializes"),
        )
        .await;
    assert_eq!(
        granted.status,
        BUCKET_POLICY_STORED,
        "case {case:?}: the PutBucketPolicy of {bucket} must be stored, got {} ({})",
        granted.status,
        granted.summary()
    );
}

/// The one form field list of the `S3Action` of rule `at`, saving into `bucket` under `prefix`.
fn s3_action(at: usize, bucket: &str, prefix: &str) -> Vec<(String, String)> {
    let action = format!("Rule.Actions.member.{at}.S3Action");
    vec![
        field(format!("{action}.BucketName"), bucket),
        field(format!("{action}.ObjectKeyPrefix"), prefix),
    ]
}

/// The rule set `rule_set` of this account, made ACTIVE, holding two rules in this order:
///
/// 1. the rule under test, applying to [`MATCHED`] and [`SECOND_MATCHED`], scanning when `scan`,
///    and saving into `bucket` under [`INBOX_PREFIX`];
/// 2. the CONTROL rule, stating no recipient condition at all — so it applies to every recipient
///    of every verified domain — and saving into the same bucket under [`CONTROL_PREFIX`].
///
/// The control rule is placed with `After` naming the rule under test, because `CreateReceiptRule`
/// otherwise "insert[s] [the rule] at the beginning of the rule list": the order is what makes
/// the control object the LAST one a received message writes.
async fn given_active_rule_set(
    emulator: &Emulator,
    case: &str,
    rule_set: &str,
    scan: bool,
    bucket: &str,
) {
    emulator
        .ses(case, "CreateReceiptRuleSet", &[field("RuleSetName", rule_set)])
        .await;

    let mut under_test = vec![
        field("RuleSetName", rule_set),
        field("Rule.Name", RULE),
        field("Rule.Enabled", "true"),
        field("Rule.ScanEnabled", if scan { "true" } else { "false" }),
        field("Rule.Recipients.member.1", MATCHED),
        field("Rule.Recipients.member.2", SECOND_MATCHED),
    ];
    under_test.extend(s3_action(1, bucket, INBOX_PREFIX));
    emulator.ses(case, "CreateReceiptRule", &under_test).await;

    let mut control = vec![
        field("RuleSetName", rule_set),
        field("After", RULE),
        field("Rule.Name", CONTROL_RULE),
        field("Rule.Enabled", "true"),
        field("Rule.ScanEnabled", "false"),
    ];
    control.extend(s3_action(1, bucket, CONTROL_PREFIX));
    emulator.ses(case, "CreateReceiptRule", &control).await;

    emulator
        .ses(
            case,
            "SetActiveReceiptRuleSet",
            &[field("RuleSetName", rule_set)],
        )
        .await;

    // The set a received message is evaluated against, read back from the service itself: a row
    // asserting on an object proves nothing about a rule the account does not hold, nor about one
    // the account holds in another order.
    let active = emulator.ses(case, "DescribeActiveReceiptRuleSet", &[]).await;
    let held = elements(&active, "Name");
    assert!(
        held.iter().any(|name| name == RULE) && held.iter().any(|name| name == CONTROL_RULE),
        "case {case:?}: the active rule set must hold {RULE} and {CONTROL_RULE}, got {held:?}"
    );
    let order: Vec<&String> = held
        .iter()
        .filter(|name| *name == RULE || *name == CONTROL_RULE)
        .collect();
    assert_eq!(
        order,
        vec![&RULE.to_string(), &CONTROL_RULE.to_string()],
        "case {case:?}: the control rule must be evaluated after the rule under test, got \
         {held:?}"
    );
}

/// One simple SESv2 send: the parts Amazon SES assembles the message from, addressed by the
/// row's three `Destination` fields, answering the `MessageId` it was accepted under.
async fn send_simple(
    emulator: &Emulator,
    case: &str,
    to: &[&str],
    cc: &[&str],
    bcc: &[&str],
    subject: &str,
) -> String {
    let mut destination = serde_json::Map::new();
    for (name, addresses) in [
        ("ToAddresses", to),
        ("CcAddresses", cc),
        ("BccAddresses", bcc),
    ] {
        if !addresses.is_empty() {
            destination.insert(name.to_string(), json!(addresses));
        }
    }
    let answered = emulator
        .sesv2(
            case,
            "POST",
            "/v2/email/outbound-emails",
            Some(json!({
                "FromEmailAddress": SENDER,
                "Destination": Value::Object(destination),
                "Content": {
                    "Simple": {
                        "Subject": { "Data": subject },
                        "Body": { "Text": { "Data": "inbound probe" } },
                    },
                },
            })),
            200,
        )
        .await;
    answered["MessageId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must answer a MessageId: {answered}"))
        .to_string()
}

/// The keys of `bucket` under `prefix`, in the order Amazon S3 lists them.
async fn keys(emulator: &Emulator, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
    let listed = emulator
        .s3(
            case,
            "GET",
            &format!("/{bucket}?list-type=2&prefix={prefix}"),
            Vec::new(),
        )
        .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: listing {bucket}/{prefix} must succeed, got {} ({})",
        listed.status,
        listed.summary()
    );
    elements(&listed.text(), "Key")
}

/// The ONE key under `prefix` of `bucket`, waited for until it is there.
///
/// The wait is for the CONDITION — an object under that prefix — looked for again until
/// [`DEADLINE`] passes, because a message is received behind its acceptance and nothing a sender
/// is told says when.
async fn settled_key(emulator: &Emulator, case: &str, bucket: &str, prefix: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = keys(emulator, case, bucket, prefix).await;
        if let Some(key) = found.first() {
            assert_eq!(
                found.len(),
                1,
                "case {case:?}: a matching rule saves one object per message, so {bucket}/{prefix} \
                 holds exactly one, got {found:?}"
            );
            return key.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no object appeared under {bucket}/{prefix}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The object `key` of `bucket`, read back through Amazon S3's own `GetObject`.
async fn stored_object(emulator: &Emulator, case: &str, bucket: &str, key: &str) -> Vec<u8> {
    let read = emulator
        .s3(case, "GET", &format!("/{bucket}/{key}"), Vec::new())
        .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: reading {bucket}/{key} must succeed, got {} ({})",
        read.status,
        read.summary()
    );
    read.body.to_vec()
}

/// The header fields of the stored message, keyed by the lowercased field name, each with every
/// occurrence of that field in the order the block holds them.
///
/// A folded continuation line joins the field it continues, which is how RFC 5322 §2.2.3 says a
/// field's value is read.
fn header_fields(case: &str, message: &[u8]) -> BTreeMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(message).into_owned();
    let block = text.split("\r\n\r\n").next().unwrap_or_default().to_string();
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut last: Option<String> = None;
    for line in block.split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with(' ') || line.starts_with('\t') {
            let name = last.clone().unwrap_or_else(|| {
                panic!("case {case:?}: a folded line opens no header field: {block:?}")
            });
            if let Some(value) = fields.entry(name).or_default().last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        let (name, value) = line.split_once(':').unwrap_or_else(|| {
            panic!(
                "case {case:?}: the stored message's header block must hold header fields, got \
                 the line {line:?}"
            )
        });
        let name = name.to_ascii_lowercase();
        fields.entry(name.clone()).or_default().push(value.trim().to_string());
        last = Some(name);
    }
    fields
}

/// The value of the one occurrence of `name` the stored message carries, and `None` when it
/// carries none; a field occurring twice is itself a difference, so it panics.
fn one_field<'a>(
    case: &str,
    fields: &'a BTreeMap<String, Vec<String>>,
    name: &str,
) -> Option<&'a str> {
    let held = fields.get(name)?;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: the stored message must carry one {name} field, got {held:?}"
    );
    held.first().map(String::as_str)
}

/// One row: how the send addresses the message, whether the rule under test scans, and how many
/// objects that rule therefore saves.
struct Row {
    /// What this row is called in every assertion it makes.
    case: &'static str,
    /// The bucket and the rule set of this row — one each, so a row's listing sees its own
    /// objects and no other row's.
    bucket: &'static str,
    rule_set: &'static str,
    /// `Destination$ToAddresses`, `Destination$CcAddresses` and `Destination$BccAddresses`.
    to: &'static [&'static str],
    cc: &'static [&'static str],
    bcc: &'static [&'static str],
    /// `ReceiptRule$ScanEnabled` of the rule under test.
    scan: bool,
    /// How many objects the rule under test saves for this send.
    saved: usize,
}

/// The rows: which `Destination` field names an address the rule applies to, how many do, and
/// whether the rule scans.
const ROWS: &[Row] = &[
    Row {
        case: "the matched address in ToAddresses alone",
        bucket: "envelope-recipients-in-to",
        rule_set: "envelope-recipients-in-to",
        to: &[MATCHED],
        cc: &[],
        bcc: &[],
        scan: false,
        saved: 1,
    },
    Row {
        case: "the matched address in CcAddresses, with ToAddresses naming another mailbox",
        bucket: "envelope-recipients-in-cc",
        rule_set: "envelope-recipients-in-cc",
        to: &[UNMATCHED],
        cc: &[MATCHED],
        bcc: &[],
        scan: true,
        saved: 1,
    },
    Row {
        case: "the matched address in BccAddresses, with ToAddresses naming another mailbox",
        bucket: "envelope-recipients-in-bcc",
        rule_set: "envelope-recipients-in-bcc",
        to: &[UNMATCHED],
        cc: &[],
        bcc: &[MATCHED],
        scan: false,
        saved: 1,
    },
    Row {
        case: "the same matched address in ToAddresses and in CcAddresses",
        bucket: "envelope-recipients-twice",
        rule_set: "envelope-recipients-twice",
        to: &[MATCHED],
        cc: &[MATCHED],
        bcc: &[],
        scan: false,
        saved: 1,
    },
    Row {
        case: "two matched addresses, one in ToAddresses and one in BccAddresses",
        bucket: "envelope-recipients-two-matched",
        rule_set: "envelope-recipients-two-matched",
        to: &[MATCHED],
        cc: &[UNMATCHED],
        bcc: &[SECOND_MATCHED],
        scan: true,
        saved: 1,
    },
    Row {
        case: "no field of the destination naming an address the rule applies to",
        bucket: "envelope-recipients-none-matched",
        rule_set: "envelope-recipients-none-matched",
        to: &[UNMATCHED],
        cc: &[],
        bcc: &[],
        scan: false,
        saved: 0,
    },
];

/// The addresses of an address field's value, as RFC 5322 §3.4 writes a list of them.
fn address_list(addresses: &[&str]) -> String {
    addresses.join(", ")
}

/// A simple send's envelope recipients decide which rules of the active set apply to it, and a
/// rule that applies saves ONE object carrying the document Amazon SES assembled.
#[tokio::test]
async fn the_envelope_recipients_of_a_simple_send_decide_what_the_active_rule_set_saves() {
    let emulator = Emulator::new();
    given_verified_identity(&emulator, "the receiving account").await;

    for row in ROWS {
        let case = row.case;
        given_bucket(&emulator, case, row.bucket).await;
        given_active_rule_set(&emulator, case, row.rule_set, row.scan, row.bucket).await;

        let subject = format!("probe token for {case}");
        let message_id =
            send_simple(&emulator, case, row.to, row.cc, row.bcc, &subject).await;

        // The control rule applies to every recipient of every verified domain, and it is the
        // LAST rule of the active set: its object being there is what says this message's steps
        // are finished, so the listing read next says whether the rule under test saved anything
        // rather than whether it has saved it YET.
        let control = settled_key(&emulator, case, row.bucket, CONTROL_PREFIX).await;
        assert_eq!(
            control,
            format!("{CONTROL_PREFIX}{message_id}"),
            "case {case:?}: a rule stating no recipient condition applies to every recipient of \
             a verified domain, and saves at its own prefix and the id the send was accepted \
             under, got {control:?}"
        );

        let saved = keys(&emulator, case, row.bucket, INBOX_PREFIX).await;
        assert_eq!(
            saved.len(),
            row.saved,
            "case {case:?}: the rule applying to [{MATCHED}, {SECOND_MATCHED}] must save \
             {} object(s) for a message whose envelope recipients are To {:?} Cc {:?} Bcc {:?}, \
             got {saved:?}",
            row.saved,
            row.to,
            row.cc,
            row.bcc
        );
        if row.saved == 0 {
            continue;
        }

        assert_eq!(
            saved[0],
            format!("{INBOX_PREFIX}{message_id}"),
            "case {case:?}: the saved object is keyed by the action's prefix and the id the send \
             was accepted under, got {saved:?}"
        );
        let stored = stored_object(&emulator, case, row.bucket, &saved[0]).await;
        let fields = header_fields(case, &stored);

        assert_eq!(
            one_field(case, &fields, "subject"),
            Some(subject.as_str()),
            "case {case:?}: the assembled message carries the subject the send sent, got \
             {fields:?}"
        );
        assert_eq!(
            one_field(case, &fields, "from"),
            Some(SENDER),
            "case {case:?}: the assembled message carries the sending identity's address, got \
             {fields:?}"
        );
        assert_eq!(
            one_field(case, &fields, "to"),
            Some(address_list(row.to).as_str()),
            "case {case:?}: the assembled message's To is the send's ToAddresses, got {fields:?}"
        );
        assert_eq!(
            one_field(case, &fields, "cc"),
            (!row.cc.is_empty()).then(|| address_list(row.cc)).as_deref(),
            "case {case:?}: the assembled message's Cc is the send's CcAddresses, and a send \
             naming none carries no Cc field at all, got {fields:?}"
        );
        // A blind carbon copy is blind: it decided which rules applied to the message, and it is
        // in no field of the document the applying rule saved.
        assert!(
            !fields.contains_key("bcc"),
            "case {case:?}: the assembled message carries no Bcc field, got {fields:?}"
        );
        for blind in row.bcc {
            assert!(
                !fields.values().flatten().any(|value| value.contains(blind)),
                "case {case:?}: no field of the assembled message may name the blind-carbon-copy \
                 recipient {blind}, got {fields:?}"
            );
        }

        let signature = one_field(case, &fields, "dkim-signature").unwrap_or_else(|| {
            panic!(
                "case {case:?}: a message Amazon SES sent for an Easy DKIM identity carries its \
                 DKIM-Signature, got {fields:?}"
            )
        });
        assert!(
            signature
                .split(';')
                .any(|tag| tag.trim() == format!("d={DOMAIN}")),
            "case {case:?}: the signature must be made for the sending identity's domain, got \
             {signature:?}"
        );

        for header in SCAN_HEADERS {
            assert_eq!(
                fields.contains_key(*header),
                row.scan,
                "case {case:?}: a rule whose ScanEnabled is {} has {}{header} stamped on the \
                 message it saves, got {fields:?}",
                row.scan,
                if row.scan { "" } else { "no " }
            );
        }
    }
}
