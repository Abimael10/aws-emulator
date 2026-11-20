// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal of a Firewall Manager call that only the
//! organization's management account can perform.
//!
//! The rule asserted here, in one sentence: an operation that administers the
//! organization's Firewall Manager administrators — which the model documents as
//! the organization's management account's own — is refused, while the
//! organization holds no such administration to read or write, with the modelled
//! `InvalidOperationException` naming the calling account and the organization's
//! management account, never with an error code outside that operation's own
//! modelled error union.
//!
//! The two operations are the ones whose model says so:
//! `ListAdminAccountsForOrganization` ("This operation can be called only from
//! the organization's management account") and `PutAdminAccount` ("Only the
//! organization's management account can create an Firewall Manager
//! administrator account"). Their modelled error unions
//! (`smithy_query(query="operation",
//! operation="ListAdminAccountsForOrganization,PutAdminAccount")`) both declare
//! `InvalidOperationException` and NEITHER declares `AccessDeniedException`,
//! which is why that code is asserted against here.
//!
//! `GetAdminScope`, the read side of what `PutAdminAccount` writes, is NOT in
//! this class and is asserted here as the contrast: it reports what the ONE
//! account its request names resolves to, so while the organization holds no
//! administration it answers its OWN modelled `ResourceNotFoundException`
//! rather than this refusal.
//!
//! The classes the rule ranges over:
//!
//! * the OPERATION — both of them — each with payload forms that differ in
//!   shape: no members at all, a page bound, a one-digit account id, a 12-digit
//!   one, a scope, and a scope that is invalid anyway (so the refusal is shown
//!   to precede payload validation);
//! * the STORE state that holds no administration: an organization that was
//!   never onboarded, and an association whose tear-down the CLOCK has already
//!   completed (a role pending deletion whose completion instant has passed,
//!   and a role already deleted);
//! * the signing REGION, which this refusal does not depend on — it names
//!   accounts, not a Region.
//!
//! The control is the same calls with an administration associated: each is then
//! answered from its own lookup instead, and what a refused call tried to write
//! is not in the store.

use std::time::{Duration, SystemTime};

use serde_json::json;
use service_com_amazonaws_fms_awsfms_20180101::state;

// The administrator record builder and the wire harness both live in one place
// for this test target, so this suite seeds a store and signs a request exactly
// as its siblings do.
use crate::administrator_gate_clock_reading::association;
use crate::administrator_gated_operations::{associate_admin, error_message, with_store};
use crate::policy_lifecycle::{call_in, emulator_with_engine, error_code, ACCOUNT, REGION};

/// A second Region, to assert the refusal does not depend on which Region the
/// request is signed for.
const OTHER_REGION: &str = "eu-central-1";

/// A 12-digit account id other than the caller's, well formed under the model's
/// `AWSAccountId` (`^[0-9]+$`, length 1..1024).
const OTHER_ACCOUNT: &str = "123456789012";

/// A one-digit account id: the SHORTEST string that satisfies `AWSAccountId`'s
/// own constraints, a form the issue did not cite.
const SHORTEST_ACCOUNT: &str = "9";

/// The refusal the live service words for a caller performing an action only the
/// organization's management account can perform. The fixed text is the
/// requirement; the two accounts in it are state — the account the request is
/// made by, and the organization's management account.
fn management_account_refusal(caller: &str, management_account: &str) -> String {
    format!(
        "{caller} is not the management account of the Organization but is attempting to \
         perform an action that only the Organization management account can perform. The \
         Organization management account is {management_account}"
    )
}

/// One call to a management-account-only operation: a name for the assertion
/// messages, the operation, and the payload a client sends.
fn management_account_only_calls() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "the enumeration with no members at all",
            "ListAdminAccountsForOrganization",
            json!({}),
        ),
        (
            "the enumeration with a page bound",
            "ListAdminAccountsForOrganization",
            json!({ "MaxResults": 1 }),
        ),
        (
            "an administrator write with no scope",
            "PutAdminAccount",
            json!({ "AdminAccount": OTHER_ACCOUNT }),
        ),
        (
            "an administrator write with a scope",
            "PutAdminAccount",
            json!({
                "AdminAccount": SHORTEST_ACCOUNT,
                "AdminScope": { "AccountScope": { "AllAccountsEnabled": true } },
            }),
        ),
        (
            "an administrator write with a scope that is invalid anyway",
            "PutAdminAccount",
            json!({
                "AdminAccount": OTHER_ACCOUNT,
                "AdminScope": {
                    "AccountScope": { "AllAccountsEnabled": true, "Accounts": [OTHER_ACCOUNT] },
                },
            }),
        ),
    ]
}

/// One store state in which the organization holds no Firewall Manager
/// administration at the instant a request decides from: a name, and the
/// association to seed (`None` for an organization that was never onboarded).
fn unadministered_stores() -> Vec<(&'static str, Option<state::AdminAccountRecord>)> {
    let now = SystemTime::now();
    vec![
        ("never onboarded", None),
        (
            "a tear-down the clock has completed",
            Some(association(
                state::ROLE_STATUS_PENDING_DELETION,
                Some(now - Duration::from_millis(1)),
            )),
        ),
        (
            "a role already deleted",
            Some(association(state::ROLE_STATUS_DELETED, None)),
        ),
    ]
}

/// While the organization holds no Firewall Manager administration, every call
/// that only its management account can make is refused with the modelled
/// `InvalidOperationException`, carrying the message the live service words for
/// it — whatever the payload's form, whichever Region the request is signed for,
/// and however the organization came to hold no administration.
#[tokio::test]
async fn management_account_only_calls_are_refused_with_the_modelled_invalid_operation() {
    for (store_case, seeded) in unadministered_stores() {
        for region in [REGION, OTHER_REGION] {
            let (router, engine) = emulator_with_engine();
            with_store(&engine, |store| {
                store.admin_account = seeded.clone();
            });
            // The organization's management account: with no Organizations
            // control plane mounted on this engine there is no organization
            // record to read, so the refusal names the account this emulator
            // presents — which is the account Organizations records as the
            // management account of the organization it belongs to, pinned
            // against Organizations itself by the cross-service suite
            // `fms_management_account_only_refusal` in `core/router`.
            let expected = management_account_refusal(ACCOUNT, ACCOUNT);

            for (case, operation, request) in management_account_only_calls() {
                let (status, body) = call_in(&router, operation, &request, region).await;
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "InvalidOperationException"),
                    "case {store_case:?} / {case:?} in {region}: expected the modelled \
                     InvalidOperationException, got {status} / {code:?} with {body}"
                );
                let message = error_message(&body);
                assert_eq!(
                    message, expected,
                    "case {store_case:?} / {case:?} in {region}: expected the refusal to name \
                     the calling account and the organization's management account, got \
                     {message:?}"
                );
            }
        }
    }
}

/// The contrast: `GetAdminScope` is answered from its OWN lookup of the account
/// it names in every one of those same states, so an organization that holds no
/// Firewall Manager administration answers the modelled
/// `ResourceNotFoundException` for it and never the refusal above \u2014 whichever
/// account the request names and whichever Region it is signed for.
#[tokio::test]
async fn the_scope_read_answers_its_own_not_found_while_the_organization_holds_no_administration() {
    for (store_case, seeded) in unadministered_stores() {
        for region in [REGION, OTHER_REGION] {
            let (router, engine) = emulator_with_engine();
            with_store(&engine, |store| {
                store.admin_account = seeded.clone();
            });
            // Account-id forms spanning the modelled `AWSAccountId` (`^[0-9]+$`,
            // length 1..1024): the calling account, another 12-digit one, and
            // the shortest well-formed id.
            for account in [ACCOUNT, OTHER_ACCOUNT, SHORTEST_ACCOUNT] {
                let (status, body) = call_in(
                    &router,
                    "GetAdminScope",
                    &json!({ "AdminAccount": account }),
                    region,
                )
                .await;
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "ResourceNotFoundException"),
                    "case {store_case:?} / {account} in {region}: an account that is no \
                     administrator is this read's own not-found, got {status} / {code:?} with \
                     {body}"
                );
                assert_eq!(
                    error_message(&body),
                    state::RESOURCE_NOT_FOUND_MESSAGE,
                    "case {store_case:?} / {account} in {region}: the not-found text is the \
                     service-wide constant, got {body}"
                );
            }
        }
    }
}

/// The refusal names the account the request is made by and the account AWS
/// Organizations records as the organization's management account — both read
/// from state, neither a literal in the refusal's text.
#[tokio::test]
async fn the_refusal_names_the_calling_account_and_the_organizations_management_account() {
    let (router, _engine) = emulator_with_engine();
    let (status, body) = call_in(
        &router,
        "ListAdminAccountsForOrganization",
        &json!({}),
        REGION,
    )
    .await;
    let message = error_message(&body);
    assert_eq!(
        status, 400,
        "the call must be refused, got {status} with {body}"
    );
    assert!(
        message.starts_with(&format!("{ACCOUNT} is not the management account")),
        "the refusal leads with the calling account, got {message:?}"
    );
    assert!(
        message.ends_with(&format!(
            "The Organization management account is {ACCOUNT}"
        )),
        "the refusal ends with the organization's management account, got {message:?}"
    );
}

/// The control: with an administration associated the SAME calls are answered
/// from their own lookup instead — the enumeration reports the administrator,
/// the write onboards one and the read reports back what it wrote — so the
/// refusal above belongs to the organization's state and not to the operations.
#[tokio::test]
async fn with_an_administration_associated_the_same_calls_are_answered_from_their_own_lookup() {
    for region in [REGION, OTHER_REGION] {
        let (router, _engine) = emulator_with_engine();
        associate_admin(&router).await;

        let (status, body) = call_in(
            &router,
            "ListAdminAccountsForOrganization",
            &json!({}),
            region,
        )
        .await;
        assert_eq!(
            status, 200,
            "region {region}: the enumeration must be served once an administration exists, got \
             {status} with {body}"
        );

        // An account that is no administrator is answered by this operation's
        // OWN lookup: the modelled not-found, not the management-account
        // refusal.
        let (status, body) = call_in(
            &router,
            "GetAdminScope",
            &json!({ "AdminAccount": OTHER_ACCOUNT }),
            region,
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "region {region}: an account that is no administrator is this operation's own \
             not-found, got {status} with {body}"
        );

        // And the write is accepted, with the scope it wrote read back.
        let scope = json!({
            "AccountScope": { "AllAccountsEnabled": true },
            "RegionScope": { "AllRegionsEnabled": true },
            "PolicyTypeScope": { "AllPolicyTypesEnabled": true },
        });
        let (status, body) = call_in(
            &router,
            "PutAdminAccount",
            &json!({ "AdminAccount": OTHER_ACCOUNT, "AdminScope": scope }),
            region,
        )
        .await;
        assert_eq!(
            status, 200,
            "region {region}: the administrator write must be accepted, got {status} with {body}"
        );
        let (status, body) = call_in(
            &router,
            "GetAdminScope",
            &json!({ "AdminAccount": OTHER_ACCOUNT }),
            region,
        )
        .await;
        assert_eq!(
            status, 200,
            "region {region}: the onboarded administrator must be reported back, got {status} \
             with {body}"
        );
        assert_eq!(
            body["AdminScope"]["AccountScope"]["AllAccountsEnabled"].as_bool(),
            Some(true),
            "region {region}: expected the scope the write persisted, got {body}"
        );
    }
}

/// A refused call wrote nothing: once an administration IS associated, no
/// account a refused `PutAdminAccount` named is an administrator of the
/// organization.
#[tokio::test]
async fn a_refused_management_account_only_call_writes_nothing() {
    let (router, _engine) = emulator_with_engine();
    for (case, operation, request) in management_account_only_calls() {
        let (status, body) = call_in(&router, operation, &request, REGION).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidOperationException"),
            "case {case:?}: expected the modelled refusal, got {status} with {body}"
        );
    }

    associate_admin(&router).await;
    for account in [OTHER_ACCOUNT, SHORTEST_ACCOUNT] {
        let (status, body) = call_in(
            &router,
            "GetAdminScope",
            &json!({ "AdminAccount": account }),
            REGION,
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ResourceNotFoundException"),
            "account {account}: a refused write must have onboarded no administrator, but \
             GetAdminScope answers {status} with {body}"
        );
    }
    let (status, body) = call_in(
        &router,
        "ListAdminAccountsForOrganization",
        &json!({}),
        REGION,
    )
    .await;
    let reported = body["AdminAccounts"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        (status, reported.len()),
        (200, 1),
        "the organization must hold only the default administrator, got {status} with {body}"
    );
    assert_eq!(
        reported[0]["AdminAccount"].as_str(),
        Some(ACCOUNT),
        "the one administrator must be the associated default one, got {body}"
    );
}
