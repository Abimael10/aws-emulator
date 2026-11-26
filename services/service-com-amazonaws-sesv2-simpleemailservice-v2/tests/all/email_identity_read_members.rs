// SPDX-License-Identifier: CC0-1.0

//! What `GetEmailIdentity` reports about an identity's DKIM key length and its
//! verification state, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, label decoding, the handler and serialization are
//! all exercised.
//!
//! The rules these cases pin:
//!
//! * `DkimAttributes$NextSigningKeyLength` is "[Easy DKIM] the key length of the
//!   future DKIM key pair to be generated", so a read reports it for every identity:
//!   the length the identity's Easy-DKIM key is configured at where SES generated the
//!   pair, and the service's base length where it holds no key configuration for the
//!   identity at all (an address identity, or a domain whose key pair the caller
//!   owns). It is always one of the values `DkimSigningKeyLength` publishes.
//! * `VerificationInfo` is "an object that contains additional information about the
//!   verification status for the identity", which every identity the account holds
//!   has, so a read of an existing identity always carries the object. What varies is
//!   how much of it is known: an identity whose records no search has looked for
//!   reports the object with every member unset, rather than no object at all.
//! * `MailFromAttributes` is likewise reported for every identity, because
//!   `BehaviorOnMxFailure` states what sending does when the required MX record cannot
//!   be found whether or not a custom MAIL FROM domain was ever configured: with none
//!   configured the documented default applies and there is no domain and no MX-search
//!   state to report, so those two members carry no value.
//!
//! The rows span what the rules generalise over: the identity kind (a domain and an
//! address), how DKIM was configured for it (Easy DKIM at the default length, Easy
//! DKIM at each length the model publishes, Bring Your Own DKIM, and no DKIM at all),
//! the domain spellings (a second-level name, a subdomain, mixed case), and whether a
//! domain identity covers the address identity.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::ops::types::DKIM_SIGNING_KEY_LENGTH_VALUES;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    BEHAVIOR_ON_MX_FAILURE_DEFAULT, DKIM_BASE_SIGNING_KEY_LENGTH,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Easy-DKIM key length SES generates a pair at when the create named none.
///
/// The other value of `DkimSigningKeyLength`, which is what a create that asks for
/// nothing in particular gets, so the table can tell "the configured length" from
/// "the base length" without either being written twice.
const EASY_DKIM_DEFAULT_KEY_LENGTH: &str = "RSA_2048_BIT";

/// One answered request: the status and the parsed body.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The `DkimAttributes` object the read reported, or `Value::Null` when absent.
    fn dkim(&self) -> Value {
        self.body
            .get("DkimAttributes")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The `VerificationInfo` object the read reported, or `Value::Null` when absent.
    fn verification_info(&self) -> Value {
        self.body
            .get("VerificationInfo")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The `MailFromAttributes` object the read reported, or `Value::Null` when absent.
    fn mail_from(&self) -> Value {
        self.body
            .get("MailFromAttributes")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// A member of `DkimAttributes`, as a string, or `None` when absent.
    fn dkim_member(&self, member: &str) -> Option<String> {
        self.dkim()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulated service behind its router.
struct Ses {
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.us-east-1.amazonaws.com{path}"))
            .header("host", "email.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/us-east-1/ses/aws4_request, SignedHeaders=host, Signature=00",
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    async fn create(&self, body: Value) -> Reply {
        self.call("POST", "/v2/email/identities", Some(body)).await
    }

    async fn get(&self, identity: &str) -> Reply {
        self.call("GET", &format!("/v2/email/identities/{identity}"), None)
            .await
    }

    /// `PutEmailIdentityMailFromAttributes`, so a case can show what a CONFIGURED
    /// identity reports beside what an unconfigured one does.
    async fn put_mail_from(&self, identity: &str, domain: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/identities/{identity}/mail-from"),
            Some(json!({"MailFromDomain": domain})),
        )
        .await
    }
}

/// One identity the rules have to hold for.
struct Case {
    /// The name the identity is created and read under.
    identity: &'static str,
    /// The `CreateEmailIdentity` body that fixes its DKIM configuration.
    create: Value,
    /// The `NextSigningKeyLength` a read of it must report.
    next_key_length: &'static str,
    /// Whether SES generated the identity's Easy-DKIM key pair, and so reports the
    /// length of the pair in use as well.
    ses_owns_the_key: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            identity: "easy.example.com",
            create: json!({"EmailIdentity": "easy.example.com"}),
            next_key_length: EASY_DKIM_DEFAULT_KEY_LENGTH,
            ses_owns_the_key: true,
        },
        Case {
            identity: "short.example.com",
            create: json!({
                "EmailIdentity": "short.example.com",
                "DkimSigningAttributes": {"NextSigningKeyLength": "RSA_1024_BIT"},
            }),
            next_key_length: "RSA_1024_BIT",
            ses_owns_the_key: true,
        },
        Case {
            identity: "mail.long.example.org",
            create: json!({
                "EmailIdentity": "mail.long.example.org",
                "DkimSigningAttributes": {"NextSigningKeyLength": "RSA_2048_BIT"},
            }),
            next_key_length: "RSA_2048_BIT",
            ses_owns_the_key: true,
        },
        Case {
            identity: "Mixed.Case.Example.NET",
            create: json!({"EmailIdentity": "Mixed.Case.Example.NET"}),
            next_key_length: EASY_DKIM_DEFAULT_KEY_LENGTH,
            ses_owns_the_key: true,
        },
        Case {
            // Bring Your Own DKIM: the caller owns the pair, so SES has generated
            // none and holds no length for the identity.
            identity: "byo.example.net",
            create: json!({
                "EmailIdentity": "byo.example.net",
                "DkimSigningAttributes": {
                    "DomainSigningSelector": "selector7",
                    "DomainSigningPrivateKey": "dGhpcyBpcyBub3QgYSByZWFsIGtleQ==",
                },
            }),
            next_key_length: DKIM_BASE_SIGNING_KEY_LENGTH,
            ses_owns_the_key: false,
        },
        Case {
            // An address identity no domain identity covers: DKIM signing applies to
            // domains, so nothing is configured for it.
            identity: "solo@uncovered.example.com",
            create: json!({"EmailIdentity": "solo@uncovered.example.com"}),
            next_key_length: DKIM_BASE_SIGNING_KEY_LENGTH,
            ses_owns_the_key: false,
        },
        Case {
            identity: "No-Reply@Mixed.Example.COM",
            create: json!({"EmailIdentity": "No-Reply@Mixed.Example.COM"}),
            next_key_length: DKIM_BASE_SIGNING_KEY_LENGTH,
            ses_owns_the_key: false,
        },
    ]
}

#[tokio::test]
async fn every_read_reports_the_key_length_of_the_next_pair() {
    for case in cases() {
        let ses = Ses::new();
        let created = ses.create(case.create.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected the identity to be created, got {} {:?}",
            case.identity, created.status, created.body
        );

        let read = ses.get(case.identity).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected the identity to be readable, got {} {:?}",
            case.identity, read.status, read.body
        );
        assert_eq!(
            read.dkim_member("NextSigningKeyLength").as_deref(),
            Some(case.next_key_length),
            "case {:?}: expected DkimAttributes.NextSigningKeyLength {:?}, got {:?}",
            case.identity,
            case.next_key_length,
            read.dkim()
        );
        // Whatever the length is, it names a key length the model publishes.
        let reported = read
            .dkim_member("NextSigningKeyLength")
            .unwrap_or_default();
        assert!(
            DKIM_SIGNING_KEY_LENGTH_VALUES.contains(&reported.as_str()),
            "case {:?}: expected one of the published key lengths {:?}, got {reported:?}",
            case.identity,
            DKIM_SIGNING_KEY_LENGTH_VALUES
        );
        // The length of the pair IN USE is reported only where SES generated one, so
        // the two members do not stand or fall together.
        assert_eq!(
            read.dkim_member("CurrentSigningKeyLength").is_some(),
            case.ses_owns_the_key,
            "case {:?}: expected CurrentSigningKeyLength to be reported only for a pair SES generated, got {:?}",
            case.identity,
            read.dkim()
        );
    }
}

#[tokio::test]
async fn every_read_reports_a_verification_info_object() {
    for case in cases() {
        let ses = Ses::new();
        ses.create(case.create.clone()).await;

        let read = ses.get(case.identity).await;
        assert!(
            read.verification_info().is_object(),
            "case {:?}: expected a VerificationInfo object on the read, got {:?}",
            case.identity,
            read.body
        );
    }
}

#[tokio::test]
async fn every_read_reports_the_mail_from_behaviour() {
    for case in cases() {
        let ses = Ses::new();
        ses.create(case.create.clone()).await;

        let read = ses.get(case.identity).await;
        assert_eq!(
            read.mail_from(),
            json!({"BehaviorOnMxFailure": BEHAVIOR_ON_MX_FAILURE_DEFAULT}),
            "case {:?}: expected the behaviour that applies with no custom MAIL FROM domain and no domain or status beside it, got {:?}",
            case.identity,
            read.mail_from()
        );
    }
}

#[tokio::test]
async fn a_configured_mail_from_domain_is_reported_with_its_state() {
    // The other side of the rule: once a custom MAIL FROM domain IS configured, the
    // same object carries the domain and where the search for its MX record stands.
    for (identity, domain) in [
        ("example.com", "mail.example.com"),
        ("sender@example.net", "bounce.example.net"),
    ] {
        let ses = Ses::new();
        ses.create(json!({"EmailIdentity": identity})).await;
        let put = ses.put_mail_from(identity, domain).await;
        assert_eq!(
            put.status, 200,
            "case {identity:?}: expected {domain:?} to be configurable, got {} {:?}",
            put.status, put.body
        );

        let read = ses.get(identity).await;
        assert_eq!(
            read.mail_from().get("MailFromDomain").and_then(Value::as_str),
            Some(domain),
            "case {identity:?}: expected the configured domain to be reported, got {:?}",
            read.mail_from()
        );
        assert!(
            read.mail_from()
                .get("MailFromDomainStatus")
                .and_then(Value::as_str)
                .is_some(),
            "case {identity:?}: expected the MX search's state to be reported, got {:?}",
            read.mail_from()
        );
    }
}

#[tokio::test]
async fn an_identity_no_search_has_run_for_reports_an_empty_object() {
    // An email-address identity that no domain identity covers is verified by the
    // recipient following the link in the verification message: no DNS zone has been
    // searched for it, nothing has succeeded and nothing has failed — so the object
    // is there with every member unset.
    for identity in ["solo@uncovered.example.com", "No-Reply@Mixed.Example.COM"] {
        let ses = Ses::new();
        ses.create(json!({"EmailIdentity": identity})).await;

        let read = ses.get(identity).await;
        assert_eq!(
            read.verification_info(),
            json!({}),
            "case {identity:?}: expected a VerificationInfo with no member set, got {:?}",
            read.verification_info()
        );
    }
}

#[tokio::test]
async fn an_address_identity_reports_the_covering_domain_s_key_length() {
    // The DKIM records an address identity's mail is signed with are published in the
    // covering domain's zone, so it is that domain's key configuration a read of the
    // address reports — including the length of the next pair.
    let ses = Ses::new();
    ses.create(json!({
        "EmailIdentity": "covering.example.com",
        "DkimSigningAttributes": {"NextSigningKeyLength": "RSA_1024_BIT"},
    }))
    .await;
    ses.create(json!({"EmailIdentity": "user@covering.example.com"}))
        .await;

    let domain = ses.get("covering.example.com").await;
    let address = ses.get("user@covering.example.com").await;
    assert_eq!(
        address.dkim_member("NextSigningKeyLength"),
        domain.dkim_member("NextSigningKeyLength"),
        "case covered: expected the covering domain's next key length, got {:?} against {:?}",
        address.dkim(),
        domain.dkim()
    );
    assert_eq!(
        address.dkim_member("NextSigningKeyLength").as_deref(),
        Some("RSA_1024_BIT"),
        "case covered: expected the length the domain was configured with, got {:?}",
        address.dkim()
    );
    assert!(
        address.verification_info().is_object(),
        "case covered: expected a VerificationInfo object on the read, got {:?}",
        address.body
    );
}
