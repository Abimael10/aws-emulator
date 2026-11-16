// SPDX-License-Identifier: CC0-1.0

//! How every domain-scoped operation refuses a `DomainName` the calling account does not
//! hold, over awsJson1_1.
//!
//! One rule is pinned here: a domain-scoped operation asked about a name that is not one
//! of the account's domains answers HTTP 400 `InvalidInput` — the error the model binds
//! to every operation for this condition, "the request might specify a domain name that
//! doesn't belong to the account that submitted the request" — and carries the one
//! sentence the service words it with: "Invalid request. Problems: Domain " + the name +
//! " not found in account.". Only the name varies; the text around it is fixed, and it is
//! the same text whichever domain-scoped operation was called.
//!
//! The rule is about the ACCOUNT's store, not the registry's: a name the registry holds
//! for a third party is refused the same way, and a name this account does register stops
//! being refused at all.
//!
//! Every test drives the real wire path through the shared harness in
//! `tests/all/domain_lifecycle.rs`.

use crate::domain_lifecycle::{error_code, register, Emulator};
use serde_json::json;

/// Every domain-scoped operation this crate implements, with the member its request
/// carries the domain name in. Source: the input shapes in `OPERATIONS.md` — all of them
/// name the member `DomainName` except `GetContactReachabilityStatus`, whose model
/// member is `domainName`.
pub(crate) const DOMAIN_SCOPED_OPERATIONS: &[(&str, &str)] = &[
    ("GetDomainDetail", "DomainName"),
    ("DeleteDomain", "DomainName"),
    ("EnableDomainAutoRenew", "DomainName"),
    ("DisableDomainAutoRenew", "DomainName"),
    ("DisableDomainTransferLock", "DomainName"),
    ("ListTagsForDomain", "DomainName"),
    ("UpdateTagsForDomain", "DomainName"),
    ("GetContactReachabilityStatus", "domainName"),
];

/// The sentence the refusal must carry for `name`.
fn expected_message(name: &str) -> String {
    format!("Invalid request. Problems: Domain {name} not found in account.")
}

/// Call `operation` with `name` as its domain-name member and return the status, the
/// error code and the message.
async fn refusal(
    emulator: &Emulator,
    (operation, member): (&str, &str),
    name: &str,
) -> (u16, String, String) {
    let (status, body) = emulator.call(operation, &json!({ member: name })).await;
    let message = body["message"].as_str().unwrap_or_default().to_string();
    (status, error_code(&body), message)
}

// ---------------------------------------------------------------------------
// The rule: a name the account does not hold is refused in AWS's words
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_name_the_account_does_not_hold_is_refused_as_not_found_in_account() {
    // Rows differ along the shape of the name and along WHY the account does not hold
    // it: three TLDs including a multi-label public suffix, a label at the 63-octet DNS
    // maximum, a name given in another case (domain names are not case sensitive, so the
    // refusal names the name the registrar would hold), and a name the registry holds
    // for a third party rather than for nobody at all (`iana.org`, from the standing
    // third-party registrations `seeded_registrations` in `src/state/registry.rs`).
    let long_label = "a".repeat(63);
    let cases: Vec<(String, String)> = vec![
        (
            "sim-probe-missing.com".to_string(),
            "sim-probe-missing.com".to_string(),
        ),
        (
            "sim-probe-missing.io".to_string(),
            "sim-probe-missing.io".to_string(),
        ),
        (
            "sim-probe-missing.co.uk".to_string(),
            "sim-probe-missing.co.uk".to_string(),
        ),
        (format!("{long_label}.net"), format!("{long_label}.net")),
        (
            "SIM-Probe-Missing.COM".to_string(),
            "sim-probe-missing.com".to_string(),
        ),
        ("iana.org".to_string(), "iana.org".to_string()),
    ];
    let emulator = Emulator::new();

    for (requested, reported) in &cases {
        for operation in DOMAIN_SCOPED_OPERATIONS {
            let (status, code, message) = refusal(&emulator, *operation, requested).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidInput"),
                "case {}/{requested:?}: expected 400 InvalidInput, got {status} {code} \
                 {message:?}",
                operation.0
            );
            assert_eq!(
                message,
                expected_message(reported),
                "case {}/{requested:?}: expected {:?}, got {message:?}",
                operation.0,
                expected_message(reported)
            );
        }
    }
}

#[tokio::test]
async fn every_domain_scoped_operation_serves_the_identical_refusal() {
    // The sentence is the service's, not each handler's, so the same name refused by
    // different operations must come back word for word the same. Rows differ along the
    // name, as above.
    let emulator = Emulator::new();

    for name in [
        "sim-probe-agree.com",
        "sim-probe-agree.dev",
        // Held by a third party in the registry, not by this account.
        "github.net",
    ] {
        let mut seen: Vec<(&str, String)> = Vec::new();
        for operation in DOMAIN_SCOPED_OPERATIONS {
            let (_, _, message) = refusal(&emulator, *operation, name).await;
            seen.push((operation.0, message));
        }
        let first = seen[0].1.clone();
        for (operation, message) in &seen {
            assert_eq!(
                *message, first,
                "case {operation}/{name:?}: every domain-scoped operation must serve the \
                 identical message, got {message:?} against {first:?}"
            );
        }
        assert_eq!(
            first,
            expected_message(name),
            "case {name:?}: expected {:?}, got {first:?}",
            expected_message(name)
        );
    }
}

// ---------------------------------------------------------------------------
// The rule follows the store, in both directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_refusal_follows_what_the_account_holds() {
    // Rows differ along the TLD. A name is refused before it is registered, resolves
    // while the account holds it, and is refused again once it is deleted — the refusal
    // is a statement about the store, not about the name.
    let emulator = Emulator::new();

    for name in [
        "sim-probe-roundtrip.com",
        "sim-probe-roundtrip.io",
        "sim-probe-roundtrip.co.uk",
    ] {
        let (status, code, message) =
            refusal(&emulator, ("GetDomainDetail", "DomainName"), name).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "InvalidInput", expected_message(name).as_str()),
            "case {name:?} before registration: expected the not-found refusal, got \
             {status} {code} {message:?}"
        );

        register(&emulator, name).await;

        // Read it back through the implemented reader: a name the account holds is not
        // refused at all.
        let (status, body) = emulator
            .call("GetDomainDetail", &json!({ "DomainName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?} while registered: expected GetDomainDetail to answer 200, got \
             {status} {body}"
        );
        assert_eq!(
            body["DomainName"].as_str(),
            Some(name),
            "case {name:?} while registered: expected the registered name, got {body}"
        );

        // Every other domain-scoped operation resolves it too, so none of them refuses a
        // name the account holds. `DeleteDomain` is left out of this pass: it is what
        // takes the name back out of the store below.
        for (operation, member) in DOMAIN_SCOPED_OPERATIONS {
            if *operation == "DeleteDomain" {
                continue;
            }
            let member = *member;
            let (status, body) = emulator.call(operation, &json!({ member: name })).await;
            assert_eq!(
                status, 200,
                "case {operation}/{name:?} while registered: expected 200, got {status} \
                 {body}"
            );
        }

        let (status, body) = emulator
            .call("DeleteDomain", &json!({ "DomainName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected DeleteDomain to succeed, got {status} {body}"
        );

        for operation in DOMAIN_SCOPED_OPERATIONS {
            let (status, code, message) = refusal(&emulator, *operation, name).await;
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (400, "InvalidInput", expected_message(name).as_str()),
                "case {}/{name:?} after deletion: expected the not-found refusal, got \
                 {status} {code} {message:?}",
                operation.0
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The rule stops where the name itself is the problem
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_name_that_does_not_resolve_is_not_reported_as_not_found_in_account() {
    // "not found in account" is the answer for a name the registrar COULD hold; a name
    // under a TLD it does not offer, and a blank name, are their own refusals and must
    // not be dressed up as a missing registration. Rows differ along which rule the
    // value breaks.
    let emulator = Emulator::new();
    let cases = [
        ("sim-probe.doesnotexisttld", "UnsupportedTLD"),
        ("sim-probe.invalid", "UnsupportedTLD"),
        ("", "InvalidInput"),
    ];

    for (name, expected_code) in cases {
        for operation in DOMAIN_SCOPED_OPERATIONS {
            let (status, code, message) = refusal(&emulator, *operation, name).await;
            assert_eq!(
                (status, code.as_str()),
                (400, expected_code),
                "case {}/{name:?}: expected 400 {expected_code}, got {status} {code} \
                 {message:?}",
                operation.0
            );
            assert!(
                !message.contains("not found in account"),
                "case {}/{name:?}: a name that does not resolve is not a missing \
                 registration, got {message:?}",
                operation.0
            );
        }
    }
}
