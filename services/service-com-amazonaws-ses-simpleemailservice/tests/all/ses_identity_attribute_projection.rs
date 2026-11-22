// SPDX-License-Identifier: CC0-1.0

//! What the per-identity attribute reports PROJECT: the members each entry of
//! `GetIdentityDkimAttributes` and `GetIdentityMailFromDomainAttributes` carries,
//! end to end over the wire.
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so each one is a real
//! awsQuery request: form-urlencoded body in, XML out, one shared engine behind
//! them all. Two rules are asserted, each over a SET of identities rather than
//! one recorded request:
//!
//! * a DKIM entry names WHO OWNS the signing key Amazon SES would sign that
//!   identity's mail with (`SigningKeyOrigin`), for every identity the report
//!   carries — whatever class of identity it is, whether an Easy DKIM setup
//!   covers it and whether signing is on for it — because every identity of the
//!   account has a signing key behind it. The other three members keep what they
//!   reported;
//! * a custom MAIL FROM entry names the `MailFromDomain` the identity is
//!   configured to use only while it IS configured with one: an identity with no
//!   custom MAIL FROM has no domain to name, so the member is absent rather than
//!   present and blank, which a client reads as a configured domain whose name is
//!   empty.

use crate::harness::{call, element, elements, emulator, field, map_entries, Field};
use smithy_server_runtime::MultiServiceRouter;

/// The origin an entry reports while Amazon SES owns the identity's signing key:
/// the key pair is the service's own (Easy DKIM), not one the identity's owner
/// supplied.
const AWS_MANAGED_SIGNING_KEY: &str = "AWS_SES";

/// `VerifyEmailIdentity`.
async fn verify_email(router: &MultiServiceRouter, address: &str) {
    let (status, body) = call(
        router,
        "VerifyEmailIdentity",
        &[field("EmailAddress", address)],
    )
    .await;
    assert_eq!(
        status, 200,
        "VerifyEmailIdentity({address:?}): expected 200, got {status} with {body}"
    );
}

/// `VerifyDomainIdentity`, which records a domain WITHOUT setting Easy DKIM up.
async fn verify_domain(router: &MultiServiceRouter, domain: &str) {
    let (status, body) = call(router, "VerifyDomainIdentity", &[field("Domain", domain)]).await;
    assert_eq!(
        status, 200,
        "VerifyDomainIdentity({domain:?}): expected 200, got {status} with {body}"
    );
}

/// `VerifyDomainDkim`, which records the domain AND generates its Easy DKIM
/// tokens.
async fn verify_domain_dkim(router: &MultiServiceRouter, domain: &str) {
    let (status, body) = call(router, "VerifyDomainDkim", &[field("Domain", domain)]).await;
    assert_eq!(
        status, 200,
        "VerifyDomainDkim({domain:?}): expected 200, got {status} with {body}"
    );
}

/// `SetIdentityDkimEnabled`.
async fn set_dkim_enabled(router: &MultiServiceRouter, identity: &str, enabled: bool) {
    let (status, body) = call(
        router,
        "SetIdentityDkimEnabled",
        &[
            field("Identity", identity),
            field("DkimEnabled", if enabled { "true" } else { "false" }),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetIdentityDkimEnabled({identity:?}, {enabled}): expected 200, got {status} with {body}"
    );
}

/// `SetIdentityMailFromDomain`. `domain` is `None` when the request leaves
/// `MailFromDomain` out altogether, which disables the custom MAIL FROM setting.
async fn set_mail_from_domain(
    router: &MultiServiceRouter,
    identity: &str,
    domain: Option<&str>,
) -> (u16, String) {
    let mut params = vec![field("Identity", identity)];
    if let Some(domain) = domain {
        params.push(field("MailFromDomain", domain));
    }
    call(router, "SetIdentityMailFromDomain", &params).await
}

/// The per-identity entries of a report over `identities`, as
/// `(identity, the value element's inner XML)`.
async fn reported(
    router: &MultiServiceRouter,
    action: &str,
    identities: &[&str],
) -> Vec<(String, String)> {
    let params: Vec<Field> = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| field(format!("Identities.member.{}", index + 1), *identity))
        .collect();
    let (status, body) = call(router, action, &params).await;
    assert_eq!(
        status, 200,
        "{action}({identities:?}): expected 200, got {status} with {body}"
    );
    map_entries(&body)
}

/// One identity's entry, panicking when the report does not carry it.
async fn entry_for(router: &MultiServiceRouter, action: &str, identity: &str) -> String {
    reported(router, action, &[identity])
        .await
        .into_iter()
        .find(|(key, _)| key == identity)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("{action}: {identity:?} must be reported"))
}

/// How an identity's DKIM state is set up before it is reported on.
#[derive(Debug, Clone, Copy)]
enum DkimSetup {
    /// `VerifyDomainDkim`: the domain carries an Easy DKIM setup.
    EasyDkimDomain,
    /// `VerifyDomainIdentity`: a domain nobody asked Easy DKIM for.
    DomainWithoutDkim,
    /// An address under a domain that carries an Easy DKIM setup.
    AddressUnderAnEasyDkimDomain,
    /// An address whose domain carries no setup.
    AddressWithoutDkim,
    /// `VerifyDomainDkim` and then signing switched off.
    EasyDkimDomainWithSigningOff,
}

#[tokio::test]
async fn every_dkim_entry_reports_who_owns_the_signing_key() {
    let router = emulator();

    // The rows differ along what the rule generalises over: the class of
    // identity, whether an Easy DKIM setup covers it (its own, its domain's, or
    // none at all) and whether signing is on for it.
    //
    // (case, the identity, how it is set up, whether the entry reports tokens,
    //  the verification status it reports, the signing flag it reports)
    let cases: Vec<(&str, &str, DkimSetup, bool, &str, &str)> = vec![
        (
            "a domain with an Easy DKIM setup",
            "dkim.example.com",
            DkimSetup::EasyDkimDomain,
            true,
            "Pending",
            "true",
        ),
        (
            "a domain nobody asked Easy DKIM for",
            "plain.example.com",
            DkimSetup::DomainWithoutDkim,
            false,
            "NotStarted",
            "false",
        ),
        (
            "an address under a domain with an Easy DKIM setup",
            "sender@dkim.example.com",
            DkimSetup::AddressUnderAnEasyDkimDomain,
            true,
            "Pending",
            "true",
        ),
        (
            "an address whose domain has no setup",
            "sender@plain.example.org",
            DkimSetup::AddressWithoutDkim,
            false,
            "NotStarted",
            "false",
        ),
        (
            "a domain with an Easy DKIM setup and signing switched off",
            "quiet.example.net",
            DkimSetup::EasyDkimDomainWithSigningOff,
            true,
            "Pending",
            "false",
        ),
    ];

    for (case, identity, setup, carries_tokens, status, signing) in &cases {
        match setup {
            DkimSetup::EasyDkimDomain => verify_domain_dkim(&router, identity).await,
            DkimSetup::DomainWithoutDkim => verify_domain(&router, identity).await,
            DkimSetup::AddressUnderAnEasyDkimDomain => {
                // The covering setup is the address's DOMAIN's, which the row
                // above already recorded.
                verify_email(&router, identity).await;
            }
            DkimSetup::AddressWithoutDkim => verify_email(&router, identity).await,
            DkimSetup::EasyDkimDomainWithSigningOff => {
                verify_domain_dkim(&router, identity).await;
                set_dkim_enabled(&router, identity, false).await;
            }
        }

        let entry = entry_for(&router, "GetIdentityDkimAttributes", identity).await;
        assert_eq!(
            element(&entry, "SigningKeyOrigin").as_deref(),
            Some(AWS_MANAGED_SIGNING_KEY),
            "case {case:?}: expected the entry to name the owner of the signing key, got {entry}"
        );
        // The member the report gained reports beside the three it already did,
        // each still answering from the identity's own record.
        assert_eq!(
            (
                element(&entry, "DkimEnabled").as_deref(),
                element(&entry, "DkimVerificationStatus").as_deref(),
                element(&entry, "DkimTokens")
                    .map(|tokens| !elements(&tokens, "member").is_empty())
            ),
            (Some(*signing), Some(*status), carries_tokens.then_some(true)),
            "case {case:?}: expected the DKIM state {signing}/{status}/tokens={carries_tokens}, \
             got {entry}"
        );
    }

    // And one call naming every identity reports the owner on EVERY entry, not
    // only on a report of one.
    let all: Vec<&str> = cases.iter().map(|(_, identity, ..)| *identity).collect();
    let entries = reported(&router, "GetIdentityDkimAttributes", &all).await;
    assert_eq!(
        entries.len(),
        all.len(),
        "case every identity in one call: expected {} entries, got {entries:?}",
        all.len()
    );
    for (identity, entry) in &entries {
        assert_eq!(
            element(entry, "SigningKeyOrigin").as_deref(),
            Some(AWS_MANAGED_SIGNING_KEY),
            "case every identity in one call: expected {identity} to name the owner of its \
             signing key, got {entry}"
        );
    }
}

/// How an identity's custom MAIL FROM is set up before it is reported on.
#[derive(Debug, Clone, Copy)]
enum MailFromSetup {
    /// Nobody ever called `SetIdentityMailFromDomain` for it.
    NeverConfigured,
    /// It carries the domain the row names.
    Configured,
    /// It carried that domain and a later call with an EMPTY `MailFromDomain`
    /// disabled the setting.
    ClearedByAnEmptyValue,
    /// It carried that domain and a later call with NO `MailFromDomain` disabled
    /// the setting.
    ClearedByAnOmittedValue,
}

#[tokio::test]
async fn a_mail_from_entry_names_the_domain_only_where_one_is_configured() {
    let router = emulator();

    // The rows differ along what the rule generalises over: the class of
    // identity, whether a custom MAIL FROM domain is configured at all, and how
    // one that was configured came to be gone again.
    //
    // (case, the identity, whether it is a domain identity, the MAIL FROM domain
    //  the row uses, the setup)
    let cases: Vec<(&str, &str, bool, &str, MailFromSetup)> = vec![
        (
            "a domain identity with no custom MAIL FROM",
            "bare.example.com",
            true,
            "mail.bare.example.com",
            MailFromSetup::NeverConfigured,
        ),
        (
            "an address identity with no custom MAIL FROM",
            "bare@plain.example.com",
            false,
            "mail.plain.example.com",
            MailFromSetup::NeverConfigured,
        ),
        (
            "a domain identity with a custom MAIL FROM",
            "set.example.com",
            true,
            "mail.set.example.com",
            MailFromSetup::Configured,
        ),
        (
            "an address identity with a custom MAIL FROM",
            "sender@set.example.org",
            false,
            "bounce.set.example.org",
            MailFromSetup::Configured,
        ),
        (
            "a custom MAIL FROM cleared by an empty value",
            "cleared.example.com",
            true,
            "mail.cleared.example.com",
            MailFromSetup::ClearedByAnEmptyValue,
        ),
        (
            "a custom MAIL FROM cleared by an omitted value",
            "dropped.example.com",
            true,
            "mail.dropped.example.com",
            MailFromSetup::ClearedByAnOmittedValue,
        ),
    ];

    for (case, identity, domain_identity, mail_from, setup) in &cases {
        if *domain_identity {
            verify_domain(&router, identity).await;
        } else {
            verify_email(&router, identity).await;
        }
        let configured = match setup {
            MailFromSetup::NeverConfigured => None,
            MailFromSetup::Configured => Some(vec![Some(*mail_from)]),
            MailFromSetup::ClearedByAnEmptyValue => Some(vec![Some(*mail_from), Some("")]),
            MailFromSetup::ClearedByAnOmittedValue => Some(vec![Some(*mail_from), None]),
        };
        for requested in configured.unwrap_or_default() {
            let (status, body) = set_mail_from_domain(&router, identity, requested).await;
            assert_eq!(
                status, 200,
                "case {case:?}: SetIdentityMailFromDomain({requested:?}): expected 200, got \
                 {status} with {body}"
            );
        }

        let entry = entry_for(&router, "GetIdentityMailFromDomainAttributes", identity).await;
        let expected = match setup {
            MailFromSetup::Configured => Some(*mail_from),
            _ => None,
        };
        assert_eq!(
            element(&entry, "MailFromDomain").as_deref(),
            expected,
            "case {case:?}: expected the reported MailFromDomain {expected:?}, got {entry}"
        );
        // The behaviour on an MX failure is reported whatever the setup, so an
        // absent domain is the one member left out and not an empty entry.
        assert_eq!(
            element(&entry, "BehaviorOnMXFailure").as_deref(),
            Some("UseDefaultValue"),
            "case {case:?}: expected the MX-failure behaviour to be reported, got {entry}"
        );
        if expected.is_some() {
            assert_eq!(
                element(&entry, "MailFromDomainStatus").as_deref(),
                Some("Pending"),
                "case {case:?}: expected a configured setup to report its status, got {entry}"
            );
        }
    }
}
