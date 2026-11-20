// SPDX-License-Identifier: CC0-1.0

//! How the writes that set an attribute ON an email identity classify an
//! `EmailIdentity` that names none of the account's identities, end to end over HTTP.
//!
//! Every case drives the real wire path — the crate's one JSON wire harness
//! ([`crate::json_wire`]), so routing, label decoding, the handler and error
//! serialization are all exercised.
//!
//! The rule these cases pin: for the `PutEmailIdentity*Attributes` family the identity
//! is the write's INPUT — the identity SES would have to sign, forward or send for —
//! so a value that resolves to none of the account's identities is refused `400`
//! `BadRequestException`, whatever that value is spelled like and whichever attribute
//! was being set, and nothing is persisted. The DKIM write words the refusal in terms
//! of the mechanism it configures — `Domain DOMAIN is not verified for DKIM signing.`,
//! naming the domain the requested identity belongs to — and the writes that configure
//! no one mechanism of their own state it of the identity: `Identity NAME is invalid.
//! Must be a verified email address or domain.`. `NotFoundException` (404) stays the
//! answer of the operations that address the identity RECORD — `GetEmailIdentity`,
//! `GetEmailIdentityPolicies`, `DeleteEmailIdentity` — on the very same names, so the
//! two classes are pinned against each other rather than one at a time.
//!
//! The table spans what the rule generalises over: the kind and spelling of the
//! unresolvable value (an address, a domain, a subdomain, mixed case, a tagged local
//! part, a name under a reserved top-level domain, a single-label name, an ARN, a value
//! with a space in it, a name differing from a held one only in case, one that merely
//! contains a held one, and one created and then deleted), which of the four writes
//! answered it, and the repeat of the same call; and against it, the identities the
//! account does hold — a domain, an address and one whose verification has not
//! completed — which every one of the four writes accepts, with the attributes they
//! wrote read back through `GetEmailIdentity`.
//!
//! A last case walks one identity of each form through the whole lifecycle — absent,
//! held, deleted, held again — asserting at each step which class the writes answer and
//! which the record-addressing operations do: the classification is a function of what
//! the account holds when the call arrives, so a name the account held a moment ago is
//! refused as invalid input and a name it holds again is written to and read back.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    identity_key, ComAmazonawsSesv2SimpleemailserviceV2State, VERIFICATION_SETTLE,
};

use crate::json_wire::{Reply, Ses};

/// The characters this suite leaves unescaped in the one path segment the
/// `EmailIdentity` label occupies: RFC 3986's unreserved set (`-`, `.`, `_`, `~` beside
/// the alphanumerics) plus the two sub-delimiters an email identity carries, `@` and
/// `+`. Everything else — `:` and `/` of an ARN, a space — is escaped, the way a
/// client's URI builder encodes a non-greedy label, so a value that is no identity at
/// all still reaches the handler whole.
///
/// The `percent-encoding` crate does the encoding; no suite writes the escaping itself.
const IDENTITY_LABEL: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'@')
    .remove(b'+');

/// `value` encoded for the `EmailIdentity` path label.
fn label(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, IDENTITY_LABEL).to_string()
}

async fn create_identity(ses: &Ses, identity: &str) -> Reply {
    ses.call(
        "POST",
        "/v2/email/identities",
        Some(json!({ "EmailIdentity": identity })),
    )
    .await
}

async fn create_configuration_set(ses: &Ses, name: &str) -> Reply {
    ses.call(
        "POST",
        "/v2/email/configuration-sets",
        Some(json!({ "ConfigurationSetName": name })),
    )
    .await
}

async fn get_identity(ses: &Ses, identity: &str) -> Reply {
    ses.call(
        "GET",
        &format!("/v2/email/identities/{}", label(identity)),
        None,
    )
    .await
}

async fn get_identity_policies(ses: &Ses, identity: &str) -> Reply {
    ses.call(
        "GET",
        &format!("/v2/email/identities/{}/policies", label(identity)),
        None,
    )
    .await
}

async fn delete_identity(ses: &Ses, identity: &str) -> Reply {
    ses.call(
        "DELETE",
        &format!("/v2/email/identities/{}", label(identity)),
        None,
    )
    .await
}

/// Hold the clock-owed verification transition off, so an identity the account holds
/// stays in the state its create left it in: the writes have to accept it while its
/// verification is still `PENDING`, which is what makes their refusal of the names
/// below about resolving the identity and nothing else.
fn hold_verification_pending(ses: &Ses, identity: &str) {
    let state = ses
        .engine
        .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
    let mut store = state.lock();
    let record = store
        .identities
        .get_mut(&identity_key(identity))
        .unwrap_or_else(|| panic!("case {identity:?}: the identity must exist in the store"));
    record.verification_due_at = SystemTime::now() + VERIFICATION_SETTLE + Duration::from_secs(3600);
}

/// The identities the account holds in every case: a domain, an address under another
/// domain, and a domain whose verification is held `PENDING`.
const HELD_DOMAIN: &str = "held.example.com";
const HELD_ADDRESS: &str = "sender@mail.example.net";
const HELD_PENDING: &str = "pending.example.org";

/// The configuration set the association write names when it is expected to succeed.
const HELD_SET: &str = "attribute-write-set";

/// The identity created and then deleted, so one row of the table is a name the account
/// held and holds no longer.
const DELETED: &str = "deleted@example.com";

/// One `EmailIdentity` that resolves to none of the account's identities, with the case
/// name the assertions report.
///
/// The rows span the forms such a value takes: a well-formed address and domain nothing
/// created, a subdomain of a held domain, a domain nothing created spelled in mixed
/// case and a held address whose local part differs only in case (a domain is compared
/// the way DNS compares one, a local part as supplied), a name that merely contains a
/// held one, a tagged local part, a name under a top-level domain the DNS root does not
/// delegate (RFC 6761 reserves `example`, `invalid`, `test` and `localhost`), a
/// single-label name, an ARN, and a value with a space in it — plus the identity the
/// account held until it was deleted.
fn unresolvable_identities() -> Vec<(&'static str, String)> {
    vec![
        ("an address nothing created", "absent@example.com".into()),
        ("a domain nothing created", "absent.example.com".into()),
        ("a subdomain of a held domain", format!("sub.{HELD_DOMAIN}")),
        (
            "a mixed-case domain nothing created",
            "Absent.Example.ORG".into(),
        ),
        (
            "a held address with a differently cased local part",
            "SENDER@mail.example.net".into(),
        ),
        (
            "a domain that merely contains a held one",
            "notheld.example.com".into(),
        ),
        ("a tagged local part", "absent+tag@example.net".into()),
        (
            "a name under a reserved top-level domain",
            "fake-6fec50e7838e.invalid.example".into(),
        ),
        ("a single-label name", "localhost".into()),
        (
            "an identity ARN rather than a name",
            "arn:aws:ses:us-east-1:000000000000:identity/absent.example.com".into(),
        ),
        ("a value with a space in it", "not an identity".into()),
        ("an identity that was deleted", DELETED.into()),
    ]
}

/// The sentence the writes that configure no one mechanism of their own answer for an
/// identity they cannot resolve. Its only variable part is the `EmailIdentity` the
/// request spelled.
fn not_a_sending_identity(identity: &str) -> String {
    format!("Identity {identity} is invalid. Must be a verified email address or domain.")
}

/// The sentence the DKIM write answers for an identity it cannot resolve: DKIM signs a
/// DOMAIN's mail, so the variable part is the domain the requested identity belongs to —
/// the whole value for a domain identity, the domain part of an address.
fn not_verified_for_dkim(identity: &str) -> String {
    let domain = match identity.rsplit_once('@') {
        Some((_, domain)) => domain,
        None => identity,
    };
    format!("Domain {domain} is not verified for DKIM signing.")
}

/// Each write in the `PutEmailIdentity*Attributes` family answered for `identity`, as
/// (the operation's name, the reply, the sentence that write words the refusal in).
async fn attribute_writes(ses: &Ses, identity: &str) -> Vec<(&'static str, Reply, String)> {
    let segment = label(identity);
    vec![
        (
            "PutEmailIdentityDkimAttributes",
            ses.call(
                "PUT",
                &format!("/v2/email/identities/{segment}/dkim"),
                Some(json!({ "SigningEnabled": true })),
            )
            .await,
            not_verified_for_dkim(identity),
        ),
        (
            "PutEmailIdentityFeedbackAttributes",
            ses.call(
                "PUT",
                &format!("/v2/email/identities/{segment}/feedback"),
                Some(json!({ "EmailForwardingEnabled": true })),
            )
            .await,
            not_a_sending_identity(identity),
        ),
        (
            "PutEmailIdentityMailFromAttributes",
            ses.call(
                "PUT",
                &format!("/v2/email/identities/{segment}/mail-from"),
                Some(json!({ "MailFromDomain": "mail.example.com" })),
            )
            .await,
            not_a_sending_identity(identity),
        ),
        (
            "PutEmailIdentityConfigurationSetAttributes",
            ses.call(
                "PUT",
                &format!("/v2/email/identities/{segment}/configuration-set"),
                Some(json!({ "ConfigurationSetName": HELD_SET })),
            )
            .await,
            not_a_sending_identity(identity),
        ),
    ]
}

/// An account holding the three identities above and one configuration set, with the
/// deleted row already created and removed.
async fn account() -> Ses {
    let ses = Ses::new();
    assert_eq!(
        create_configuration_set(&ses, HELD_SET).await.status,
        200,
        "the configuration set the association write names must be created"
    );
    for identity in [HELD_DOMAIN, HELD_ADDRESS, HELD_PENDING, DELETED] {
        let created = create_identity(&ses, identity).await;
        assert_eq!(
            created.status, 200,
            "case {identity:?}: creating the identity must answer 200, got {} {:?}",
            created.status, created.body
        );
    }
    hold_verification_pending(&ses, HELD_PENDING);
    let removed = delete_identity(&ses, DELETED).await;
    assert_eq!(
        removed.status, 200,
        "deleting {DELETED:?} must answer 200, got {} {:?}",
        removed.status, removed.body
    );
    ses
}

#[tokio::test]
async fn every_attribute_write_refuses_an_unresolvable_identity_as_a_bad_request() {
    let ses = account().await;

    for (case, identity) in unresolvable_identities() {
        // Twice: the refusal is terminal, so the second answer is the first.
        for attempt in [1_u8, 2] {
            for (operation, reply, expected) in attribute_writes(&ses, &identity).await {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, "BadRequestException"),
                    "case {case:?} ({operation}, {identity:?}, attempt {attempt}): expected 400 \
                     BadRequestException, got {} {:?}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    reply.message(),
                    expected,
                    "case {case:?} ({operation}, {identity:?}, attempt {attempt}): expected \
                     {expected:?}, got {:?}",
                    reply.message()
                );
            }
        }
    }

    // Nothing a refused write did created an identity: the account still holds exactly
    // the three it was given.
    let listed = ses.call("GET", "/v2/email/identities", None).await;
    let count = listed.listed("EmailIdentities", "IdentityName").len();
    assert_eq!(
        count, 3,
        "expected the account to still hold its three identities, got {count} in {:?}",
        listed.body
    );
}

#[tokio::test]
async fn the_operations_addressing_the_identity_record_still_answer_not_found() {
    let ses = account().await;

    for (case, identity) in unresolvable_identities() {
        // The reads quote the name in angle brackets and the delete does not, but all
        // three report a missing RESOURCE where the attribute writes report bad input.
        let rows = [
            (
                "GetEmailIdentity",
                get_identity(&ses, &identity).await,
                format!("Email identity <{identity}> does not exist."),
            ),
            (
                "GetEmailIdentityPolicies",
                get_identity_policies(&ses, &identity).await,
                format!("Email identity <{identity}> does not exist."),
            ),
            (
                "DeleteEmailIdentity",
                delete_identity(&ses, &identity).await,
                format!("Email identity {identity} does not exist."),
            ),
        ];
        for (operation, reply, expected) in rows {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (404, "NotFoundException"),
                "case {case:?} ({operation}, {identity:?}): expected 404 NotFoundException, got {} \
                 {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?} ({operation}, {identity:?}): expected {expected:?}, got {:?}",
                reply.message()
            );
        }
    }
}

#[tokio::test]
async fn an_identity_the_account_holds_is_accepted_by_every_attribute_write() {
    let ses = account().await;

    // Each identity the account holds, with the MAIL FROM domain it may take: a domain
    // strictly under the identity's own domain.
    let held = [
        (HELD_DOMAIN, format!("mail.{HELD_DOMAIN}")),
        (HELD_ADDRESS, "bounce.mail.example.net".to_string()),
        (HELD_PENDING, format!("bounce.{HELD_PENDING}")),
    ];

    for (identity, mail_from) in held {
        let segment = label(identity);
        let writes = [
            (
                "PutEmailIdentityDkimAttributes",
                format!("/v2/email/identities/{segment}/dkim"),
                json!({ "SigningEnabled": true }),
            ),
            (
                "PutEmailIdentityFeedbackAttributes",
                format!("/v2/email/identities/{segment}/feedback"),
                json!({ "EmailForwardingEnabled": true }),
            ),
            (
                "PutEmailIdentityMailFromAttributes",
                format!("/v2/email/identities/{segment}/mail-from"),
                json!({ "MailFromDomain": mail_from.clone() }),
            ),
            (
                "PutEmailIdentityConfigurationSetAttributes",
                format!("/v2/email/identities/{segment}/configuration-set"),
                json!({ "ConfigurationSetName": HELD_SET }),
            ),
        ];
        for (operation, path, body) in writes {
            let reply = ses.call("PUT", &path, Some(body)).await;
            assert_eq!(
                reply.status, 200,
                "case ({operation}, {identity:?}): expected 200, got {} {:?}",
                reply.status, reply.body
            );
        }

        // Read back what the four writes stored, so the accepted calls are backed by
        // state and not only by their own 200.
        let read = get_identity(&ses, identity).await;
        assert_eq!(
            read.status, 200,
            "case {identity:?}: the identity must be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body
                .get("DkimAttributes")
                .and_then(|dkim| dkim.get("SigningEnabled")),
            Some(&json!(true)),
            "case {identity:?}: expected DKIM signing to be on, got {:?}",
            read.body.get("DkimAttributes")
        );
        assert_eq!(
            read.body.get("FeedbackForwardingStatus"),
            Some(&json!(true)),
            "case {identity:?}: expected feedback forwarding to be on, got {:?}",
            read.body.get("FeedbackForwardingStatus")
        );
        assert_eq!(
            read.body
                .get("MailFromAttributes")
                .and_then(|attributes| attributes.get("MailFromDomain"))
                .and_then(Value::as_str),
            Some(mail_from.as_str()),
            "case {identity:?}: expected the MAIL FROM domain {mail_from:?} to be stored, got {:?}",
            read.body.get("MailFromAttributes")
        );
        assert_eq!(
            read.body.get("ConfigurationSetName"),
            Some(&json!(HELD_SET)),
            "case {identity:?}: expected the configuration set to be associated, got {:?}",
            read.body.get("ConfigurationSetName")
        );
    }
}

/// The identity spellings the lifecycle case below walks from end to end: an address, a
/// domain, a subdomain, a mixed-case address and an address with a tagged local part —
/// the forms an `EmailIdentity` takes, each one created, written to, deleted and created
/// again on an account of its own.
const LIFECYCLE_IDENTITIES: &[&str] = &[
    "lifecycle@example.com",
    "lifecycle.example.com",
    "sub.lifecycle.example.net",
    "Lifecycle.Case@Example.ORG",
    "lifecycle+tag@example.net",
];

/// `PutEmailIdentityFeedbackAttributes` with the setting supplied.
async fn put_feedback(ses: &Ses, identity: &str, enabled: bool) -> Reply {
    ses.call(
        "PUT",
        &format!("/v2/email/identities/{}/feedback", label(identity)),
        Some(json!({ "EmailForwardingEnabled": enabled })),
    )
    .await
}

/// Every write in the `PutEmailIdentity*Attributes` family refuses `identity` as invalid
/// input, each with the sentence that write words the refusal in.
async fn assert_refused_as_invalid_input(ses: &Ses, phase: &str, identity: &str) {
    for (operation, reply, expected) in attribute_writes(ses, identity).await {
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "BadRequestException"),
            "case {phase} ({operation}, {identity:?}): expected 400 BadRequestException, got {} \
             {:?}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.message(),
            expected,
            "case {phase} ({operation}, {identity:?}): expected {expected:?}, got {:?}",
            reply.message()
        );
    }
}

/// The operations that address the identity RECORD report it missing, in the wording each
/// of them uses.
async fn assert_record_reported_absent(ses: &Ses, phase: &str, identity: &str) {
    let rows = [
        (
            "GetEmailIdentity",
            get_identity(ses, identity).await,
            format!("Email identity <{identity}> does not exist."),
        ),
        (
            "GetEmailIdentityPolicies",
            get_identity_policies(ses, identity).await,
            format!("Email identity <{identity}> does not exist."),
        ),
        (
            "DeleteEmailIdentity",
            delete_identity(ses, identity).await,
            format!("Email identity {identity} does not exist."),
        ),
    ];
    for (operation, reply, expected) in rows {
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (404, "NotFoundException"),
            "case {phase} ({operation}, {identity:?}): expected 404 NotFoundException, got {} {:?}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.message(),
            expected,
            "case {phase} ({operation}, {identity:?}): expected {expected:?}, got {:?}",
            reply.message()
        );
    }
}

#[tokio::test]
async fn the_refusal_class_follows_what_the_account_holds_at_the_time() {
    for identity in LIFECYCLE_IDENTITIES {
        let ses = Ses::new();
        let set = create_configuration_set(&ses, HELD_SET).await;
        assert_eq!(
            set.status, 200,
            "case {identity:?}: the configuration set the association write names must be \
             created, got {} {:?}",
            set.status, set.body
        );

        // Nothing created yet: the attribute writes refuse the value as invalid input
        // while the operations addressing the record report it missing — the two classes
        // answered for one and the same name.
        assert_refused_as_invalid_input(&ses, "before the create", identity).await;
        assert_record_reported_absent(&ses, "before the create", identity).await;

        // The account holds it: the write is accepted and every later read reports what
        // it stored, both settings and a value written twice.
        let created = create_identity(&ses, identity).await;
        assert_eq!(
            created.status, 200,
            "case {identity:?}: creating the identity must answer 200, got {} {:?}",
            created.status, created.body
        );
        for enabled in [true, false, true, true] {
            let put = put_feedback(&ses, identity, enabled).await;
            assert_eq!(
                put.status, 200,
                "case ({identity:?}, {enabled:?}): expected the write to be accepted with 200, \
                 got {} {:?}",
                put.status, put.body
            );
            let read = get_identity(&ses, identity).await;
            assert_eq!(
                read.flag("FeedbackForwardingStatus"),
                enabled,
                "case ({identity:?}, {enabled:?}): expected FeedbackForwardingStatus {enabled:?} \
                 to be read back, got {:?}",
                read.body
            );
            if enabled {
                // `FeedbackForwardingStatus` is modelled with a default of `false`, which
                // the wire protocol may leave out; `true` is never left out, so the
                // member itself is pinned on those steps.
                assert_eq!(
                    read.body.get("FeedbackForwardingStatus"),
                    Some(&json!(true)),
                    "case ({identity:?}, {enabled:?}): expected the answer to carry \
                     FeedbackForwardingStatus true, got {:?}",
                    read.body
                );
            }
        }

        // Deleted: the record-addressing operations answer 404 again and the attribute
        // writes 400 again, on the name the account held a moment ago.
        let removed = delete_identity(&ses, identity).await;
        assert_eq!(
            removed.status, 200,
            "case {identity:?}: deleting the identity must answer 200, got {} {:?}",
            removed.status, removed.body
        );
        assert_record_reported_absent(&ses, "after the delete", identity).await;
        assert_refused_as_invalid_input(&ses, "after the delete", identity).await;

        // Created again: the very same write is accepted again, so each refusal above was
        // about what the account held when the call arrived and not about the name having
        // once been deleted.
        let recreated = create_identity(&ses, identity).await;
        assert_eq!(
            recreated.status, 200,
            "case {identity:?}: creating the identity again must answer 200, got {} {:?}",
            recreated.status, recreated.body
        );
        let put = put_feedback(&ses, identity, true).await;
        assert_eq!(
            put.status, 200,
            "case {identity:?}: expected the write to be accepted after the re-create, got {} \
             {:?}",
            put.status, put.body
        );
        let read = get_identity(&ses, identity).await;
        assert!(
            read.flag("FeedbackForwardingStatus"),
            "case {identity:?}: expected FeedbackForwardingStatus true after the re-create, got \
             {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn a_refused_attribute_write_persists_nothing() {
    let ses = account().await;

    // What the held identities report before any refused call.
    let before = [
        get_identity(&ses, HELD_DOMAIN).await,
        get_identity(&ses, HELD_ADDRESS).await,
        get_identity(&ses, HELD_PENDING).await,
    ];

    for (case, identity) in unresolvable_identities() {
        for (operation, reply, _expected) in attribute_writes(&ses, &identity).await {
            assert_eq!(
                reply.status, 400,
                "case {case:?} ({operation}, {identity:?}): expected 400, got {} {:?}",
                reply.status, reply.body
            );
        }
    }

    for (index, identity) in [HELD_DOMAIN, HELD_ADDRESS, HELD_PENDING]
        .into_iter()
        .enumerate()
    {
        let after = get_identity(&ses, identity).await;
        for member in [
            "DkimAttributes",
            "FeedbackForwardingStatus",
            "MailFromAttributes",
            "ConfigurationSetName",
            "IdentityType",
            "VerifiedForSendingStatus",
        ] {
            assert_eq!(
                after.body.get(member),
                before[index].body.get(member),
                "case {identity:?}: expected {member} to be unchanged by refused writes, got {:?} \
                 where it was {:?}",
                after.body.get(member),
                before[index].body.get(member)
            );
        }
    }
}
