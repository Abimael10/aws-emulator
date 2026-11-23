// SPDX-License-Identifier: CC0-1.0

//! The verification gate on a configuration set's custom open- and click-tracking
//! domain, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — through the shared harness in the sibling
//! [`crate::configuration_sets`] suite, so routing, deserialization, the handler and
//! serialization are all exercised.
//!
//! The rule these cases pin: `TrackingOptions$CustomRedirectDomain` is "the domain to use
//! for tracking open and click events"
//! (`smithy_query(query="doc",shape="TrackingOptions",member="CustomRedirectDomain")`), so
//! it names a domain the ACCOUNT has proved it owns — a verified domain identity covering
//! that name, where a verified domain identity speaks for its own name and every name
//! under it. A create naming a domain no such identity covers is refused with the modelled
//! 400 `BadRequestException` BEFORE anything is stored, so the set is not persisted and
//! its name stays free: the read of that name still answers 404 `NotFoundException`, and
//! the same name then creates cleanly.
//!
//! The table below spans what the rule generalises over: whether the tracking name is the
//! verified identity itself, a name under it, its parent, a sibling that merely resembles
//! it or a name with no identity at all; the KIND of the identity that exists for it (a
//! domain identity against an email-address identity in the same domain); the case the
//! name is spelled in; a verification that has not completed; and the rest of the tracking
//! options the create carries beside the domain.

use serde_json::{json, Value};

use crate::configuration_sets::Ses;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state;

/// One row: the identities the account holds when the create lands, the tracking domain
/// the create names, and whether SES accepts it.
struct TrackingDomainCase {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// Identities the account has VERIFIED before the create — a domain identity, or an
    /// email address, which proves a mailbox rather than a domain.
    verified: &'static [&'static str],
    /// Identities the account holds whose verification has NOT completed: each is created
    /// and left as the create left it.
    unverified: &'static [&'static str],
    /// The `CustomRedirectDomain` the create names.
    domain: &'static str,
    /// The rest of the `TrackingOptions` object the create carries, as members merged
    /// beside the domain: `HttpsPolicy` carries one of the model's own values
    /// (`smithy_query(query="shape",shape="HttpsPolicy")`).
    https_policy: Option<&'static str>,
    /// Whether the create is accepted.
    accepted: bool,
}

/// A domain name under a top-level domain the DNS root does not delegate, so no record SES
/// looks for can ever be published for it: an identity for it stays unverified however
/// long the caller waits, which is what makes the row below deterministic rather than a
/// race against the span a verification falls due after.
const NEVER_VERIFIABLE_DOMAIN: &str = "click.probe.invalid";

/// The rows of the tracking-domain table.
fn tracking_domain_cases() -> Vec<TrackingDomainCase> {
    vec![
        TrackingDomainCase {
            case: "the tracking domain is itself a verified domain identity",
            verified: &["click.example.com"],
            unverified: &[],
            domain: "click.example.com",
            https_policy: Some("REQUIRE"),
            accepted: true,
        },
        TrackingDomainCase {
            case: "the tracking domain is a subdomain of a verified domain identity",
            verified: &["example.com"],
            unverified: &[],
            domain: "links.mail.example.com",
            https_policy: Some("OPTIONAL"),
            accepted: true,
        },
        TrackingDomainCase {
            case: "the verified identity is spelled in another case",
            verified: &["CLICK.Example.COM"],
            unverified: &[],
            domain: "click.example.com",
            https_policy: None,
            accepted: true,
        },
        TrackingDomainCase {
            case: "one of several verified identities covers the tracking domain",
            verified: &["other.example.org", "example.net", "example.com"],
            unverified: &["sender@example.com"],
            domain: "track.example.net",
            https_policy: Some("REQUIRE_OPEN_ONLY"),
            accepted: true,
        },
        TrackingDomainCase {
            case: "the account holds no identity at all",
            verified: &[],
            unverified: &[],
            domain: "click.example.com",
            https_policy: Some("REQUIRE"),
            accepted: false,
        },
        TrackingDomainCase {
            case: "the account's verified identity is a different domain",
            verified: &["example.org"],
            unverified: &[],
            domain: "click.example.com",
            https_policy: None,
            accepted: false,
        },
        TrackingDomainCase {
            case: "the tracking domain only resembles the verified one",
            verified: &["example.com"],
            unverified: &[],
            domain: "notexample.com",
            https_policy: None,
            accepted: false,
        },
        TrackingDomainCase {
            case: "the verified identity is a subdomain of the tracking domain, not the \
                   other way round",
            verified: &["click.example.com"],
            unverified: &[],
            domain: "example.com",
            https_policy: Some("OPTIONAL"),
            accepted: false,
        },
        TrackingDomainCase {
            case: "the only identity in that domain is a verified email address",
            verified: &["sender@click.example.com"],
            unverified: &[],
            domain: "click.example.com",
            https_policy: None,
            accepted: false,
        },
        TrackingDomainCase {
            case: "the identity for the tracking domain exists but is not verified",
            verified: &[],
            unverified: &[NEVER_VERIFIABLE_DOMAIN],
            domain: NEVER_VERIFIABLE_DOMAIN,
            https_policy: Some("REQUIRE"),
            accepted: false,
        },
    ]
}

/// The `TrackingOptions` object a row's create carries.
fn tracking_options(row: &TrackingDomainCase) -> Value {
    let mut options = json!({"CustomRedirectDomain": row.domain});
    if let Some(policy) = row.https_policy {
        options["HttpsPolicy"] = json!(policy);
    }
    options
}

#[tokio::test]
async fn a_tracking_domain_the_account_has_not_verified_is_refused() {
    for row in tracking_domain_cases() {
        let case = row.case;
        let ses = Ses::new();
        for domain in row.verified {
            ses.verify_identity(domain).await;
        }
        for identity in row.unverified {
            let created = ses
                .call(
                    "POST",
                    "/v2/email/identities",
                    Some(json!({"EmailIdentity": identity})),
                )
                .await;
            assert_eq!(
                created.status,
                200,
                "case {case:?}: expected HTTP 200 creating the identity {identity:?}, got {} {:?}",
                created.status,
                created.message()
            );
        }

        let name = "tracked_set";
        let created = ses
            .create_set(json!({
                "ConfigurationSetName": name,
                "TrackingOptions": tracking_options(&row),
            }))
            .await;

        if row.accepted {
            assert_eq!(
                created.status,
                200,
                "case {case:?}: expected HTTP 200 creating the set, got {} {:?}",
                created.status,
                created.message()
            );
            // The accepted domain is stored as the request spelled it, which the read
            // reports back.
            let read = ses.get_set(name).await;
            assert_eq!(
                read.pointer("/TrackingOptions/CustomRedirectDomain"),
                Some(json!(row.domain)),
                "case {case:?}: expected the stored tracking domain {:?}, got {:?}",
                row.domain,
                read.member("TrackingOptions")
            );
            continue;
        }

        assert_eq!(
            (created.status, created.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {} {:?} {:?}",
            created.status,
            created.error_code(),
            created.message()
        );
        assert_eq!(
            created.message(),
            state::tracking_domain_not_verified(row.domain),
            "case {case:?}: expected the refusal to name the unverified domain, got {:?}",
            created.message()
        );
        // The refused create persisted nothing, so the name it asked for is still free.
        let read = ses.get_set(name).await;
        assert_eq!(
            (read.status, read.error_code().as_str()),
            (404, "NotFoundException"),
            "case {case:?}: expected the refused create to store nothing, got {} {:?}",
            read.status,
            read.message()
        );
        assert_eq!(
            read.message(),
            state::configuration_set_not_found(name),
            "case {case:?}: expected the read to report the name as absent, got {:?}",
            read.message()
        );
    }
}

#[tokio::test]
async fn the_refused_name_creates_cleanly_afterwards() {
    // The rule generalises over the name the refused create asked for, so the names below
    // span the documented form: one character, hyphens, underscores with a digit, and the
    // 64-character bound the member's own documentation states.
    let longest = "x".repeat(64);
    for name in [
        "a",
        "promo-set",
        "promo_set_2",
        longest.as_str(),
    ] {
        let case = name;
        let ses = Ses::new();
        let domain = "click.example.com";

        let refused = ses
            .create_set(json!({
                "ConfigurationSetName": name,
                "TrackingOptions": {"CustomRedirectDomain": domain},
            }))
            .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected the unverified domain to be refused, got {} {:?}",
            refused.status,
            refused.message()
        );

        // The name is free: a create of it that configures no tracking options at all is
        // accepted, and the set it stores carries none.
        let created = ses.create_set(json!({"ConfigurationSetName": name})).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected the same name to create cleanly, got {} {:?}",
            created.status,
            created.message()
        );
        let read = ses.get_set(name).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the created set to read back, got {} {:?}",
            read.status,
            read.message()
        );
        assert_eq!(
            read.member("TrackingOptions"),
            Value::Null,
            "case {case:?}: expected the set to carry no tracking options, got {:?}",
            read.member("TrackingOptions")
        );
    }
}

#[tokio::test]
async fn verifying_the_domain_first_makes_the_same_create_succeed() {
    // One account, one domain, one set name: the only thing that changes between the two
    // creates is whether the account has verified the domain.
    let case = "the same create before and after the domain is verified";
    let ses = Ses::new();
    let domain = "click.example.com";
    let name = "tracked_set";
    let body = json!({
        "ConfigurationSetName": name,
        "TrackingOptions": {"CustomRedirectDomain": domain, "HttpsPolicy": "REQUIRE"},
    });

    let refused = ses.create_set(body.clone()).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "BadRequestException"),
        "case {case:?}: expected the create to be refused first, got {} {:?}",
        refused.status,
        refused.message()
    );

    ses.verify_identity(domain).await;
    let created = ses.create_set(body).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected the create to be accepted once the domain is verified, \
         got {} {:?}",
        created.status,
        created.message()
    );
    let read = ses.get_set(name).await;
    assert_eq!(
        read.pointer("/TrackingOptions/CustomRedirectDomain"),
        Some(json!(domain)),
        "case {case:?}: expected the stored tracking domain, got {:?}",
        read.member("TrackingOptions")
    );
    assert_eq!(
        read.pointer("/TrackingOptions/HttpsPolicy"),
        Some(json!("REQUIRE")),
        "case {case:?}: expected the stored HttpsPolicy, got {:?}",
        read.member("TrackingOptions")
    );
}
