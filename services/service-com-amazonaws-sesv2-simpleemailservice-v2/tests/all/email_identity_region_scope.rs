// SPDX-License-Identifier: CC0-1.0

//! The REGION an email identity belongs to, end to end over HTTP.
//!
//! The rule under test: **an email identity is a regional resource — `CreateEmailIdentity`
//! "starts the process of verifying an email identity … in the current Amazon Web Services
//! Region", SES publishes its identity quota per account per Region, and every identity ARN
//! carries a Region — so every operation of this surface is answered from the identities of
//! the Region the caller signed for, and a request signed for any other Region is answered
//! exactly as it would be for an identity the account never created: the reads and the
//! policy operations 404, the attribute writes refuse the input, the listing does not report
//! it, and a send from an address it would authorise is refused `MessageRejected` naming the
//! Region the send executes in.**
//!
//! The Region a call executes in is its SigV4 credential scope's, so each case signs for the
//! Region it is about rather than naming one in a body.
//!
//! The rows span what the rule generalises over: the shape of the Region label the identity
//! is created in and the one it is read from (the emulator's default Region, another
//! continent's, a numbered suffix, a label with no digits), and the class of the identity (a
//! domain, whose authority covers the addresses under it, and a single address).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    domain_not_verified_for_dkim, email_identity_not_found, email_identity_not_found_unquoted,
    identity_is_not_a_sending_identity, unverified_identities_message,
    ComAmazonawsSesv2SimpleemailserviceV2State,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
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

    /// The identity names a listing reported, in the order it reported them.
    fn listed(&self) -> Vec<String> {
        self.body
            .get("EmailIdentities")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        row.get("IdentityName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulated service behind its router, plus the shared engine — a case that sends needs
/// the identity's verification to be through, and that is evidence the account publishes
/// rather than a span to wait out.
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
        // These cases are about the REGION an identity belongs to, not about the sandbox's
        // destination gate: the account is given production access, so a send is weighed on
        // its sending identity alone (`send_sandbox_destination_gate.rs`).
        crate::send_sandbox_destination_gate::given_production_access(&engine);
        Self { engine, router }
    }

    /// One call, signed for `region`: the credential scope is what the handler reads the
    /// Region of the call from.
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

    /// Bring every identity's verification through, the way every suite of this crate that
    /// needs a verified sender does: the verification instant the record carries is moved
    /// into the past, which stands in for the evidence the account publishes (a domain's
    /// Easy DKIM records in its own DNS, a mailbox owner's confirmation).
    fn settle_verification(&self) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let past = SystemTime::now()
            .checked_sub(Duration::from_millis(1))
            .expect("an instant just before now");
        for record in state.lock().identities.values_mut() {
            record.verification_due_at = past;
        }
    }
}

/// One row: the Region the identity is created in, another Region every call is repeated
/// from, the identity, and an address that identity authorises sending from.
struct Row {
    case: &'static str,
    /// The Region the account holds the identity in.
    home: &'static str,
    /// A Region the account holds no identity in.
    elsewhere: &'static str,
    identity: &'static str,
    /// An address the identity authorises: the address itself, or one under the domain.
    sender: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a domain identity in the emulator's default Region, read from another continent",
        home: "us-east-1",
        elsewhere: "eu-west-1",
        identity: "regional.example.com",
        sender: "ada@regional.example.com",
    },
    Row {
        case: "a domain identity in a Region with a numbered suffix, read from the default one",
        home: "ap-southeast-2",
        elsewhere: "us-east-1",
        identity: "sub.regional.example.net",
        sender: "ada@sub.regional.example.net",
    },
    Row {
        case: "an address identity in a Region whose label carries no digits",
        home: "sa-east-1",
        elsewhere: "us-west-2",
        identity: "ada@solo.example.org",
        sender: "ada@solo.example.org",
    },
    Row {
        case: "an address identity, read from a Region whose label carries no digits",
        home: "eu-central-1",
        elsewhere: "sa-east-1",
        identity: "no-reply@solo.example.com",
        sender: "no-reply@solo.example.com",
    },
];

/// The body of a send from `sender`.
fn send_body(sender: &str) -> Value {
    json!({
        "FromEmailAddress": sender,
        "Destination": {"ToAddresses": ["rcpt@example.net"]},
        "Content": {"Simple": {
            "Subject": {"Data": "hello"},
            "Body": {"Text": {"Data": "a message"}},
        }},
    })
}

/// RULE: every operation that resolves an `EmailIdentity` answers from the identities of the
/// Region the caller signed for. The identity is created in one Region; each operation is
/// then made in that Region and in another, and the two answers are what the rule says.
#[tokio::test]
async fn an_identity_is_reachable_only_from_the_region_it_was_created_in() {
    for row in ROWS {
        let case = row.case;
        let ses = Ses::new();
        let identity = row.identity;
        let created = ses
            .call(
                row.home,
                "POST",
                "/v2/email/identities",
                Some(json!({"EmailIdentity": identity})),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create in {home:?} must succeed, got {} {:?}",
            created.status,
            created.body,
            home = row.home
        );

        // The reads: the identity's own, and its policies'.
        let path = format!("/v2/email/identities/{identity}");
        let read_home = ses.call(row.home, "GET", &path, None).await;
        assert_eq!(
            read_home.status, 200,
            "case {case:?}: the Region that holds the identity answers for it, got {} {:?}",
            read_home.status, read_home.body
        );
        let read_away = ses.call(row.elsewhere, "GET", &path, None).await;
        assert_eq!(
            (read_away.status, read_away.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: another Region holds no such identity, got {} {:?}",
            read_away.status,
            read_away.body
        );
        assert_eq!(
            read_away.message(),
            email_identity_not_found(identity),
            "case {case:?}: the refusal is the one every identity read answers with"
        );

        let policies_path = format!("/v2/email/identities/{identity}/policies");
        let policies_home = ses.call(row.home, "GET", &policies_path, None).await;
        assert_eq!(
            policies_home.status, 200,
            "case {case:?}: the identity's policies are readable where it is held, got {} {:?}",
            policies_home.status, policies_home.body
        );
        let policies_away = ses.call(row.elsewhere, "GET", &policies_path, None).await;
        assert_eq!(
            (policies_away.status, policies_away.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: the policies of an identity another Region holds are not there, got \
             {} {:?}",
            policies_away.status,
            policies_away.body
        );

        // A policy write names the identity as the resource it hangs off, so it is the
        // modelled 404 from another Region.
        let policy_path = format!("/v2/email/identities/{identity}/policies/Shared");
        // A well-formed sending-authorization policy, so what the write is refused for is
        // the identity it names and not the document it carries.
        let policy_document = json!({
            "Policy": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                       \"Action\":\"ses:SendEmail\",\"Principal\":{\"AWS\":\"*\"}}]}",
        });
        let policy_away = ses
            .call(row.elsewhere, "POST", &policy_path, Some(policy_document))
            .await;
        assert_eq!(
            (policy_away.status, policy_away.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: a policy cannot be attached from another Region, got {} {:?}",
            policy_away.status,
            policy_away.body
        );
        assert_eq!(
            policy_away.message(),
            email_identity_not_found_unquoted(identity),
            "case {case:?}: the refusal is the one every identity write answers with"
        );

        // The attribute writes take the identity as their INPUT, so from another Region the
        // value names none of the account's identities and each refuses the input in its own
        // words.
        let dkim_away = ses
            .call(
                row.elsewhere,
                "PUT",
                &format!("/v2/email/identities/{identity}/dkim"),
                Some(json!({"SigningEnabled": true})),
            )
            .await;
        assert_eq!(
            (dkim_away.status, dkim_away.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: a DKIM write from another Region names no identity, got {} {:?}",
            dkim_away.status,
            dkim_away.body
        );
        assert_eq!(
            dkim_away.message(),
            domain_not_verified_for_dkim(identity),
            "case {case:?}: the DKIM write's own wording for an identity it does not hold"
        );

        for (member, path, body) in [
            (
                "feedback",
                format!("/v2/email/identities/{identity}/feedback"),
                json!({"EmailForwardingEnabled": true}),
            ),
            (
                "mail-from",
                format!("/v2/email/identities/{identity}/mail-from"),
                json!({"MailFromDomain": format!("mail.{}", identity.rsplit('@').next().unwrap_or(identity))}),
            ),
            (
                "configuration-set",
                format!("/v2/email/identities/{identity}/configuration-set"),
                json!({}),
            ),
        ] {
            let away = ses.call(row.elsewhere, "PUT", &path, Some(body)).await;
            assert_eq!(
                (away.status, away.error_code().as_str()),
                (400, "BadRequestException"),
                "case {case:?}: the {member} write from another Region names no identity, got {} \
                 {:?}",
                away.status,
                away.body
            );
            assert_eq!(
                away.message(),
                identity_is_not_a_sending_identity(identity),
                "case {case:?}: the {member} write's wording for an identity it does not hold"
            );
        }

        // The listing reports the identities of the Region it is made in.
        let listed_home = ses
            .call(row.home, "POST", "/v2/email/list-identities", Some(json!({})))
            .await;
        assert_eq!(
            listed_home.listed(),
            vec![identity.to_string()],
            "case {case:?}: the listing in {home:?} reports the identity it holds, got {:?}",
            listed_home.body,
            home = row.home
        );
        let listed_away = ses
            .call(
                row.elsewhere,
                "POST",
                "/v2/email/list-identities",
                Some(json!({})),
            )
            .await;
        assert!(
            listed_away.listed().is_empty(),
            "case {case:?}: the listing in {elsewhere:?} reports no identity, got {:?}",
            listed_away.body,
            elsewhere = row.elsewhere
        );

        // And the send gate: the identity authorises its sender in the Region that holds it,
        // and in no other — where the refusal names the Region the send executes in.
        ses.settle_verification();
        let sent_home = ses
            .call(
                row.home,
                "POST",
                "/v2/email/outbound-emails",
                Some(send_body(row.sender)),
            )
            .await;
        assert_eq!(
            sent_home.status, 200,
            "case {case:?}: the verified identity authorises its sender in {home:?}, got {} {:?}",
            sent_home.status,
            sent_home.body,
            home = row.home
        );
        let sent_away = ses
            .call(
                row.elsewhere,
                "POST",
                "/v2/email/outbound-emails",
                Some(send_body(row.sender)),
            )
            .await;
        assert_eq!(
            (sent_away.status, sent_away.error_code().as_str()),
            (400, "MessageRejected"),
            "case {case:?}: a send from {elsewhere:?} has no verified identity, got {} {:?}",
            sent_away.status,
            sent_away.body,
            elsewhere = row.elsewhere
        );
        assert_eq!(
            sent_away.message(),
            // This account has production access (`Ses::new`), so the only address the
            // check weighs is the sending identity — which is what this row is about.
            unverified_identities_message(row.elsewhere, &[row.sender]),
            "case {case:?}: the refusal names the Region the send executes in and the \
             sending identity that failed the check"
        );

        // The delete is region-scoped too, and a refused one leaves the identity exactly
        // where it was.
        let deleted_away = ses.call(row.elsewhere, "DELETE", &path, None).await;
        assert_eq!(
            (deleted_away.status, deleted_away.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: a delete from another Region holds no such identity, got {} {:?}",
            deleted_away.status,
            deleted_away.body
        );
        let still_there = ses.call(row.home, "GET", &path, None).await;
        assert_eq!(
            still_there.status, 200,
            "case {case:?}: the refused delete left the identity where it was, got {} {:?}",
            still_there.status, still_there.body
        );
        let deleted_home = ses.call(row.home, "DELETE", &path, None).await;
        assert_eq!(
            deleted_home.status, 200,
            "case {case:?}: the Region that holds the identity deletes it, got {} {:?}",
            deleted_home.status, deleted_home.body
        );
    }
}
