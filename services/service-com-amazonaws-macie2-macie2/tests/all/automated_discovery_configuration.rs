// SPDX-License-Identifier: CC0-1.0

//! Automated sensitive data discovery's configuration, end to end over the
//! wire.
//!
//! Each case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called
//! directly.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * `UpdateAutomatedDiscoveryConfiguration` PERSISTS the status it is given
//!   and the auto-enable mode it names — the documented default for an omitted
//!   one — so `GetAutomatedDiscoveryConfiguration` reports exactly that;
//! * the write materialises the classification scope and the sensitivity
//!   inspection template the analyses use, once, and both reads report their
//!   identifiers unchanged afterwards;
//! * `firstEnabledAt` is the instant of the FIRST enable and never moves,
//!   `disabledAt` is the instant of the most recent disable and is absent while
//!   automated discovery is enabled, and `lastUpdatedAt` moves only when the
//!   status or the settings actually changed;
//! * reading the configuration changes nothing a later reader sees, and nothing
//!   is materialised by a read;
//! * the configuration belongs to the region the request signed for and to that
//!   region's Macie session: both operations are refused 403 until Macie is
//!   enabled there, and `DisableMacie` deletes the configuration with the rest
//!   of the account's settings.
//!
//! The PER-ACCOUNT status of automated sensitive data discovery —
//! `BatchUpdateAutomatedDiscoveryAccounts` and
//! `ListAutomatedDiscoveryAccounts` — is the second half of this suite, from
//! `the_accounts_reported_are_the_ones_this_administrator_administers`
//! onwards. The rule those cases assert: the status is a per-account fact of
//! the accounts a Macie administrator administers in a region — its own
//! account and the accounts it has a live membership with — which a batch
//! update writes for each account it administers and explains, per account,
//! for every other, and which a list reports from the store.

use std::time::Duration;

use service_com_amazonaws_macie2_macie2 as macie2;
use smithy_server_runtime::MultiServiceRouter;

use super::macie_session_and_custom_data_identifiers::{
    call_in, emulator, emulator_with_engine, enable_in, error_code, error_message, now_seconds,
    reported_instant,
};
use super::member_relationship_status_filter::seed_association;

/// The region most cases sign for.
const REGION: &str = "us-west-2";

/// A second region, for the cases that pin the per-region scope of this
/// configuration.
const OTHER_REGION: &str = "eu-west-1";

/// The automated-discovery configuration's URI, from the operations' `@http`
/// bindings (`PUT` for the update, `GET` for the read).
const CONFIGURATION_URI: &str = "/automated-discovery/configuration";

/// Long enough that two instants stamped either side of it differ in the
/// second-and-fraction a response reports, so "this stamp moved" is observable
/// rather than a race with the clock's resolution.
const CLOCK_STEP: Duration = Duration::from_millis(50);

/// Write the configuration, requiring the modelled 200 and the empty body the
/// response shape models.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
    case: &str,
) {
    let (status, reply) = call_in(router, region, "PUT", CONFIGURATION_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateAutomatedDiscoveryConfiguration must answer 200, \
         got {status} with {reply:?}"
    );
    assert!(
        reply.is_null() || reply == serde_json::json!({}),
        "case {case:?}: the response models no members, got {reply:?}"
    );
}

/// Read the configuration back, requiring the modelled 200.
async fn read(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> serde_json::Value {
    let (status, reply) = call_in(router, region, "GET", CONFIGURATION_URI, "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetAutomatedDiscoveryConfiguration must answer 200, \
         got {status} with {reply:?}"
    );
    reply
}

/// Whether a member is absent or null, which is how an instant that has not
/// happened and an identifier that has not been materialised are both reported.
fn is_absent(body: &serde_json::Value, member: &str) -> bool {
    body.get(member).is_none_or(serde_json::Value::is_null)
}

/// Read the configuration three times across the clock, requiring every report
/// to be identical. Hands back that one report.
///
/// This is what "a read reports" means: `GetAutomatedDiscoveryConfiguration`
/// retrieves the settings and status, so no report of it may differ from the
/// next one when nothing wrote in between — no stamp moved, no identifier
/// minted, no default settled into the store on first access.
async fn read_repeatedly(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> serde_json::Value {
    let first = read(router, region, case).await;
    for round in 1..=2 {
        tokio::time::sleep(CLOCK_STEP).await;
        let again = read(router, region, case).await;
        assert_eq!(
            again, first,
            "case {case:?}: read {round} after the first must report the same \
             configuration, because a read writes nothing a later caller sees; \
             got {again:?} after {first:?}"
        );
    }
    first
}

/// An account whose automated sensitive data discovery nothing has configured
/// reports the never-configured state: none of its activities are performed,
/// the auto-enable mode is the one its member documents as the default, and
/// there is no scope, no template and no instant to report. Reading it twice
/// reports the same thing, because a read materialises nothing.
#[tokio::test]
async fn an_unconfigured_account_reports_the_never_configured_state() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let first = read(&router, REGION, "unconfigured").await;
    assert_eq!(
        first["status"].as_str(),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"unconfigured\": nothing has asked for the activities, got {first:?}"
    );
    assert_eq!(
        first["autoEnableOrganizationMembers"].as_str(),
        Some(macie2::state::DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS),
        "case \"unconfigured\": the documented default mode must be reported, got {first:?}"
    );
    for member in [
        "classificationScopeId",
        "sensitivityInspectionTemplateId",
        "firstEnabledAt",
        "disabledAt",
        "lastUpdatedAt",
    ] {
        assert!(
            is_absent(&first, member),
            "case \"unconfigured\": {member} must be absent until a write produces it, \
             got {:?}",
            first.get(member)
        );
    }

    let again = read(&router, REGION, "unconfigured twice").await;
    assert_eq!(
        again, first,
        "case \"unconfigured twice\": a read must not change what the next reader sees, \
         got {again:?}"
    );
}

/// A write persists the status it is given and the auto-enable mode it names,
/// and the read reports exactly that. The table spans every value of
/// `AutomatedDiscoveryStatus` against every value of `AutoEnableMode` and
/// against the member being omitted — the enums as `smithy_query` reports them,
/// which this crate's `AUTOMATED_DISCOVERY_STATUS_VALUES` and
/// `AUTO_ENABLE_MODE_VALUES` carry — so the rows differ along both members and
/// along the presence of the optional one.
#[tokio::test]
async fn an_update_persists_every_status_and_auto_enable_mode_a_read_reports() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let mut modes: Vec<Option<&str>> = macie2::ops::types::AUTO_ENABLE_MODE_VALUES
        .iter()
        .map(|mode| Some(*mode))
        .collect();
    // The absent member is a row of its own: its doc names ALL as the value a
    // request that omits it takes.
    modes.push(None);

    let mut materialised: Option<(String, String)> = None;
    for status in macie2::ops::types::AUTOMATED_DISCOVERY_STATUS_VALUES {
        for mode in &modes {
            let case = format!("status {status} / mode {mode:?}");
            let mut body = serde_json::json!({ "status": status });
            if let Some(mode) = mode {
                body["autoEnableOrganizationMembers"] = serde_json::json!(mode);
            }
            update(&router, REGION, &body, &case).await;

            let reported = read(&router, REGION, &case).await;
            assert_eq!(
                reported["status"].as_str(),
                Some(*status),
                "case {case:?}: the stored status must be reported, got {reported:?}"
            );
            assert_eq!(
                reported["autoEnableOrganizationMembers"].as_str(),
                Some(mode.unwrap_or(macie2::state::DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS)),
                "case {case:?}: the mode named, or the documented default for an omitted \
                 one, must be reported, got {reported:?}"
            );

            let scope = reported["classificationScopeId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a configured account reports its classification \
                            scope, got {reported:?}")
                })
                .to_string();
            let template = reported["sensitivityInspectionTemplateId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a configured account reports its sensitivity \
                            inspection template, got {reported:?}")
                })
                .to_string();
            // `ClassificationScopeId` is modelled with `@pattern("^[0-9a-z]*$")`.
            assert!(
                scope
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "case {case:?}: a classification scope id is lowercase alphanumeric, \
                 got {scope:?}"
            );
            assert_ne!(
                scope, template,
                "case {case:?}: the scope and the template are two resources, so their \
                 identifiers differ, got {scope:?}"
            );
            match &materialised {
                // The scope and the template are materialised ONCE, with the
                // configuration; a later write reports the same two.
                Some((first_scope, first_template)) => {
                    assert_eq!(
                        (&scope, &template),
                        (first_scope, first_template),
                        "case {case:?}: a later write must not re-materialise the scope \
                         and the template, got {scope:?} and {template:?}"
                    );
                }
                None => materialised = Some((scope, template)),
            }
        }
    }
}

/// The instants the read reports follow their own documentation:
/// `firstEnabledAt` names the FIRST enable and never moves, `disabledAt` names
/// the most recent disable and is absent while automated discovery is enabled,
/// and `lastUpdatedAt` moves exactly when the status or the settings changed.
/// The rows are the steps of an enable/disable cycle, each naming what it
/// changes.
#[tokio::test]
async fn the_reported_instants_follow_the_enable_and_disable_cycle() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let opened = now_seconds() - 1.0;
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "ALL" }),
        "first enable",
    )
    .await;
    let enabled = read(&router, REGION, "first enable").await;
    let first_enabled_at = reported_instant("first enable", "firstEnabledAt", &enabled);
    assert!(
        (opened..=now_seconds() + 1.0).contains(&first_enabled_at),
        "case \"first enable\": firstEnabledAt must be the instant of that call, \
         got {first_enabled_at}"
    );
    assert!(
        is_absent(&enabled, "disabledAt"),
        "case \"first enable\": disabledAt is null while automated discovery is enabled, \
         got {:?}",
        enabled.get("disabledAt")
    );
    let first_update = reported_instant("first enable", "lastUpdatedAt", &enabled);

    // A call asking for the configuration the account already has changes
    // nothing, so no stamp moves.
    std::thread::sleep(CLOCK_STEP);
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "ALL" }),
        "re-state the same configuration",
    )
    .await;
    let unchanged = read(&router, REGION, "re-state the same configuration").await;
    assert_eq!(
        unchanged, enabled,
        "case \"re-state the same configuration\": a write that changes nothing must \
         move no stamp, got {unchanged:?}"
    );

    // Only the mode changes: `lastUpdatedAt` moves, the enable stamps do not.
    std::thread::sleep(CLOCK_STEP);
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "NEW" }),
        "settings only",
    )
    .await;
    let resettled = read(&router, REGION, "settings only").await;
    let settings_update = reported_instant("settings only", "lastUpdatedAt", &resettled);
    assert!(
        settings_update > first_update,
        "case \"settings only\": changing a setting moves lastUpdatedAt, \
         got {settings_update} after {first_update}"
    );
    assert_eq!(
        reported_instant("settings only", "firstEnabledAt", &resettled),
        first_enabled_at,
        "case \"settings only\": firstEnabledAt names the first enable and never moves"
    );
    assert!(
        is_absent(&resettled, "disabledAt"),
        "case \"settings only\": still enabled, so disabledAt stays null, got {:?}",
        resettled.get("disabledAt")
    );

    // Disabling stamps `disabledAt` and moves `lastUpdatedAt`.
    std::thread::sleep(CLOCK_STEP);
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "DISABLED", "autoEnableOrganizationMembers": "NEW" }),
        "disable",
    )
    .await;
    let disabled = read(&router, REGION, "disable").await;
    assert_eq!(
        disabled["status"].as_str(),
        Some("DISABLED"),
        "case \"disable\": the stored status must be reported, got {disabled:?}"
    );
    let disabled_at = reported_instant("disable", "disabledAt", &disabled);
    assert!(
        disabled_at > settings_update,
        "case \"disable\": disabledAt must be the instant of the disabling call, \
         got {disabled_at} after {settings_update}"
    );
    assert_eq!(
        reported_instant("disable", "lastUpdatedAt", &disabled),
        disabled_at,
        "case \"disable\": the call that disabled it is also the last change"
    );
    assert_eq!(
        reported_instant("disable", "firstEnabledAt", &disabled),
        first_enabled_at,
        "case \"disable\": firstEnabledAt still names the first enable"
    );

    // Re-enabling clears `disabledAt` and still leaves `firstEnabledAt` alone.
    std::thread::sleep(CLOCK_STEP);
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "NEW" }),
        "re-enable",
    )
    .await;
    let re_enabled = read(&router, REGION, "re-enable").await;
    assert!(
        is_absent(&re_enabled, "disabledAt"),
        "case \"re-enable\": disabledAt is null while automated discovery is enabled, \
         got {:?}",
        re_enabled.get("disabledAt")
    );
    assert_eq!(
        reported_instant("re-enable", "firstEnabledAt", &re_enabled),
        first_enabled_at,
        "case \"re-enable\": a later enable must not move firstEnabledAt"
    );
    assert!(
        reported_instant("re-enable", "lastUpdatedAt", &re_enabled) > disabled_at,
        "case \"re-enable\": the status changed, so lastUpdatedAt moves"
    );
}

/// The configuration belongs to the region the request signed for: configuring
/// it in one region leaves another region's account never-configured, and both
/// operations are refused 403 `AccessDeniedException` in a region where nothing
/// has enabled Macie. The rows are the two operations, against a region with no
/// session and against a region whose session knows nothing of the other's
/// configuration.
#[tokio::test]
async fn the_configuration_is_scoped_to_the_region_and_needs_a_session() {
    let router = emulator();

    for (case, method, body) in [
        ("GetAutomatedDiscoveryConfiguration", "GET", String::new()),
        (
            "UpdateAutomatedDiscoveryConfiguration",
            "PUT",
            serde_json::json!({ "status": "ENABLED" }).to_string(),
        ),
    ] {
        let (status, reply) = call_in(&router, REGION, method, CONFIGURATION_URI, &body).await;
        assert_eq!(
            status, 403,
            "case {case:?}: Macie is not enabled here, so the call is refused, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {case:?}: the refusal must be the modelled AccessDeniedException, \
             got {reply:?}"
        );
    }

    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "NONE" }),
        "configure one region",
    )
    .await;

    let elsewhere = read(&router, OTHER_REGION, "another region").await;
    assert_eq!(
        elsewhere["status"].as_str(),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"another region\": configuring one region must not configure another, \
         got {elsewhere:?}"
    );
    assert!(
        is_absent(&elsewhere, "classificationScopeId"),
        "case \"another region\": no write has materialised a scope here, got {:?}",
        elsewhere.get("classificationScopeId")
    );
}

/// `DisableMacie` "deletes all settings and resources for a Macie account", so
/// the automated-discovery configuration goes with the session: a region
/// enabled again reports the never-configured state, and the scope the first
/// configuration named is gone.
#[tokio::test]
async fn disabling_macie_deletes_the_configuration() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    update(
        &router,
        REGION,
        &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "NEW" }),
        "configure before disabling",
    )
    .await;
    let configured = read(&router, REGION, "configure before disabling").await;

    let (status, reply) = call_in(&router, REGION, "DELETE", "/macie", "").await;
    assert_eq!(
        status, 200,
        "case \"disable Macie\": DisableMacie must answer 200, got {status} with {reply:?}"
    );

    enable_in(&router, REGION, "{}").await;
    let after = read(&router, REGION, "after re-enabling Macie").await;
    assert_eq!(
        after["status"].as_str(),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"after re-enabling Macie\": the configuration was deleted with the \
         session, got {after:?}"
    );
    assert!(
        is_absent(&after, "classificationScopeId")
            && is_absent(&after, "firstEnabledAt")
            && is_absent(&after, "lastUpdatedAt"),
        "case \"after re-enabling Macie\": nothing of the deleted configuration may \
         survive, got {after:?} after {configured:?}"
    );
}

/// A value outside the modelled enums, and a request omitting the required
/// `status`, are refused 400 `ValidationException` — the rows are the two ways
/// a request can miss the input contract.
#[tokio::test]
async fn an_update_outside_the_modelled_input_is_refused() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for (case, body) in [
        (
            "status outside AutomatedDiscoveryStatus",
            serde_json::json!({ "status": "PAUSED" }),
        ),
        (
            "mode outside AutoEnableMode",
            serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": "SOME" }),
        ),
        ("required status omitted", serde_json::json!({})),
    ] {
        let (status, reply) =
            call_in(&router, REGION, "PUT", CONFIGURATION_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a request outside the modelled input is refused 400, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, \
             got {reply:?}"
        );
    }

    // Nothing of a refused request reached the store.
    let after = read(&router, REGION, "after the refusals").await;
    assert_eq!(
        after["status"].as_str(),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"after the refusals\": a refused write persists nothing, got {after:?}"
    );
}

/// `GetAutomatedDiscoveryConfiguration` RETRIEVES the settings and status, so
/// every report of one configuration is the same report: reading it repeatedly
/// across the clock, with nothing written in between, may move no stamp, mint
/// no identifier and settle no default into the store.
///
/// The rows are the states a read can find an account in — never configured,
/// then configured under each value of `AutomatedDiscoveryStatus`, the statuses
/// as `smithy_query` reports them and this crate's
/// `AUTOMATED_DISCOVERY_STATUS_VALUES` carries them — plus a second region that
/// is read throughout and must stay never-configured. The persistence of each
/// settings combination is the sibling case
/// `an_update_persists_every_status_and_auto_enable_mode_a_read_reports`; what
/// these rows add is that a READ alone changes none of it.
#[tokio::test]
async fn repeating_the_read_reports_the_same_configuration() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;

    // An account that has configured nothing: the repeated reads must leave it
    // that way, so the write below is still the FIRST to materialise a scope
    // and a template.
    let unconfigured = read_repeatedly(&router, REGION, "never configured").await;
    for member in ["classificationScopeId", "sensitivityInspectionTemplateId"] {
        assert!(
            is_absent(&unconfigured, member),
            "case \"never configured\": a read must not materialise {member}, got {:?}",
            unconfigured.get(member)
        );
    }

    let mut minted: Option<(String, String)> = None;
    for status in macie2::ops::types::AUTOMATED_DISCOVERY_STATUS_VALUES {
        let case = format!("configured {status}");
        update(
            &router,
            REGION,
            &serde_json::json!({ "status": status }),
            &case,
        )
        .await;

        let reported = read_repeatedly(&router, REGION, &case).await;
        assert_eq!(
            reported["status"].as_str(),
            Some(*status),
            "case {case:?}: the repeated reads must still report what the write \
             persisted, got {reported:?}"
        );

        let identifier = |member: &str| -> String {
            reported[member]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a configured account reports its {member}, \
                         got {reported:?}"
                    )
                })
                .to_string()
        };
        let identifiers = (
            identifier("classificationScopeId"),
            identifier("sensitivityInspectionTemplateId"),
        );
        match &minted {
            // The scope and the template were materialised by the first write,
            // not by any of the reads that have run since.
            Some(first) => assert_eq!(
                &identifiers, first,
                "case {case:?}: no read may re-materialise the scope or the template, \
                 got {identifiers:?} after {first:?}"
            ),
            None => minted = Some(identifiers),
        }
    }

    // The second region was read across all of that, and neither those reads
    // nor the other region's writes configured it.
    let elsewhere = read_repeatedly(&router, OTHER_REGION, "another region").await;
    assert_eq!(
        elsewhere["status"].as_str(),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"another region\": reading a region must not configure it, got {elsewhere:?}"
    );
    assert!(
        is_absent(&elsewhere, "classificationScopeId"),
        "case \"another region\": no read may materialise a scope here, got {:?}",
        elsewhere.get("classificationScopeId")
    );
}

// ---------------------------------------------------------------------------
// The PER-ACCOUNT status of automated sensitive data discovery
// ---------------------------------------------------------------------------

/// The automated-discovery account collection's URI, from the operations'
/// `@http` bindings (`GET` for `ListAutomatedDiscoveryAccounts`, `PATCH` for
/// `BatchUpdateAutomatedDiscoveryAccounts`).
const ACCOUNTS_URI: &str = "/automated-discovery/accounts";

/// More requests than any case's enumeration needs, so a `nextToken` that never
/// clears fails the case instead of hanging it. The widest case walks 31
/// accounts one page at a time.
const PAGE_WALK_LIMIT: usize = 64;

/// The account every emulated request is signed for, which is the administrator
/// account of every association and an account of this enumeration in its own
/// right.
fn own_account() -> String {
    ::engine::emulator_account_id().to_string()
}

/// Change the status of automated sensitive data discovery for `accounts`,
/// requiring the modelled 200, and hand back the response body.
async fn batch_update(
    router: &MultiServiceRouter,
    region: &str,
    accounts: serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let body = serde_json::json!({ "accounts": accounts });
    let (status, reply) =
        call_in(router, region, "PATCH", ACCOUNTS_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchUpdateAutomatedDiscoveryAccounts must answer 200, \
         got {status} with {reply:?}"
    );
    reply
}

/// The per-account failures a batch update reported, as (account, error code)
/// in account order. An absent or null `errors` is no failure at all, which is
/// what the member documents for a request that succeeded for every account.
fn update_errors(case: &str, reply: &serde_json::Value) -> Vec<(String, String)> {
    let Some(errors) = reply.get("errors") else {
        return Vec::new();
    };
    if errors.is_null() {
        return Vec::new();
    }
    let errors = errors.as_array().unwrap_or_else(|| {
        panic!("case {case:?}: errors must be an array when present, got {reply:?}")
    });
    let mut reported: Vec<(String, String)> = errors
        .iter()
        .map(|error| {
            let account = error["accountId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every error names its accountId, got {error:?}")
            });
            let code = error["errorCode"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every error names its errorCode, got {error:?}")
            });
            (account.to_string(), code.to_string())
        })
        .collect();
    reported.sort();
    reported
}

/// Walk the whole enumeration and hand back every (account, status) it
/// reports, in the order it reports them. `account_ids` and `max_results` are
/// sent only when named, so the empty/`None` row is the request a caller makes
/// with neither.
async fn list_accounts(
    router: &MultiServiceRouter,
    region: &str,
    account_ids: &[String],
    max_results: Option<u32>,
    case: &str,
) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=PAGE_WALK_LIMIT {
        let mut query: Vec<String> = account_ids
            .iter()
            .map(|account| format!("accountIds={account}"))
            .collect();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(token) = token.as_deref() {
            query.push(format!("nextToken={token}"));
        }
        let uri = if query.is_empty() {
            ACCOUNTS_URI.to_string()
        } else {
            format!("{ACCOUNTS_URI}?{}", query.join("&"))
        };

        let (status, page) = call_in(router, region, "GET", &uri, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListAutomatedDiscoveryAccounts must answer 200, \
             got {status} with {page:?}"
        );
        let items = page["items"].as_array().unwrap_or_else(|| {
            panic!("case {case:?}: a page must carry an items array, got {page:?}")
        });
        if let Some(size) = max_results {
            assert!(
                items.len() <= usize::try_from(size).unwrap_or_default(),
                "case {case:?}: a page must not exceed maxResults {size}, got {} items",
                items.len()
            );
        }
        for item in items {
            let account = item["accountId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every item carries its accountId, got {item:?}")
            });
            let reported = item["status"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every item carries its status, got {item:?}")
            });
            seen.push((account.to_string(), reported.to_string()));
        }

        token = page["nextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert!(
        token.is_none(),
        "case {case:?}: the enumeration must end, got a token after {seen:?}"
    );
    seen
}

/// The status the whole enumeration reports for every account, keyed by
/// account.
async fn statuses_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> std::collections::BTreeMap<String, String> {
    list_accounts(router, region, &[], None, case)
        .await
        .into_iter()
        .collect()
}

/// Associate `account` in `region`, requiring the documented 200.
async fn associate(router: &MultiServiceRouter, region: &str, account: &str, case: &str) {
    let body = serde_json::json!({
        "account": { "accountId": account, "email": format!("{account}@example.com") }
    });
    let (status, reply) = call_in(router, region, "POST", "/members", &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMember must answer 200, got {status} with {reply:?}"
    );
}

/// The twelve-digit account id of row `index`, in a range no other case of this
/// suite uses.
fn account_of(index: usize) -> String {
    format!("6666{index:08}")
}

/// A batch update PERSISTS the status it gives an account, so the list reports
/// exactly that afterwards — whether the list asks about that one account or
/// enumerates them all. The table spans every account a batch update can reach
/// (the administrator's own account, and a member account in each
/// `RelationshipStatus` of a live membership Macie runs under) against every
/// value of `AutomatedDiscoveryAccountStatus`, the enum as this crate's
/// `AUTOMATED_DISCOVERY_ACCOUNT_STATUS_VALUES` carries it.
#[tokio::test]
async fn a_batch_update_persists_every_status_a_later_list_reports() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;

    let members = [(account_of(1), "Enabled"), (account_of(2), "Invited")];
    for (account, status) in &members {
        seed_association(&engine, REGION, account, status);
    }
    let mut accounts: Vec<String> = members
        .iter()
        .map(|(account, _)| account.clone())
        .collect();
    accounts.push(own_account());
    accounts.sort();

    for account in &accounts {
        for status in macie2::ops::types::AUTOMATED_DISCOVERY_ACCOUNT_STATUS_VALUES {
            let case = format!("account {account} / status {status}");
            let reply = batch_update(
                &router,
                REGION,
                serde_json::json!([{ "accountId": account, "status": status }]),
                &case,
            )
            .await;
            assert_eq!(
                update_errors(&case, &reply),
                Vec::new(),
                "case {case:?}: this administrator administers the account, so the update \
                 reaches it, got {reply:?}"
            );

            let filtered =
                list_accounts(&router, REGION, std::slice::from_ref(account), None, &case).await;
            assert_eq!(
                filtered,
                vec![(account.clone(), (*status).to_string())],
                "case {case:?}: a read naming one account reports that account's stored \
                 status and nothing else, got {filtered:?}"
            );

            let reported = statuses_in(&router, REGION, &case).await;
            assert_eq!(
                reported.keys().cloned().collect::<Vec<String>>(),
                accounts,
                "case {case:?}: the enumeration reports every administered account, \
                 got {reported:?}"
            );
            assert_eq!(
                reported.get(account).map(String::as_str),
                Some(*status),
                "case {case:?}: the enumeration must report the stored status, \
                 got {reported:?}"
            );
        }
    }
}

/// One row of the administration catalog: the `RelationshipStatus` an
/// association carries, whether the enumeration reports the account, and the
/// per-account error code a batch update answers for it — `None` when the
/// update reaches the account.
struct AdministrationRow {
    status: &'static str,
    listed: bool,
    error: Option<&'static str>,
}

/// Every value of the modelled `RelationshipStatus` enum —
/// `smithy_query(query="shape",shape="com.amazonaws.macie2#RelationshipStatus")`
/// lists exactly these ten — against how automated sensitive data discovery
/// treats the account.
///
/// A membership is live under `Enabled`, `Paused` and `Invited`, and those are
/// the accounts this administrator administers, so the enumeration reports
/// them. Of the three, `Paused` is "in force, with Macie suspended for the
/// account", which is the condition `ACCOUNT_PAUSED` names ("Macie isn't
/// enabled for the account in the current Amazon Web Services Region"). Under
/// every other status the administrator has a record of the account but no live
/// membership with it, so it is not its Macie administrator — the condition
/// `ACCOUNT_NOT_FOUND` names ("the account doesn't exist or you're not the
/// Amazon Macie administrator for the account").
const ADMINISTRATION_CATALOG: [AdministrationRow; 10] = [
    AdministrationRow {
        status: "Enabled",
        listed: true,
        error: None,
    },
    AdministrationRow {
        status: "Invited",
        listed: true,
        error: None,
    },
    AdministrationRow {
        status: "Paused",
        listed: true,
        error: Some("ACCOUNT_PAUSED"),
    },
    AdministrationRow {
        status: "Created",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "Removed",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "Resigned",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "EmailVerificationInProgress",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "EmailVerificationFailed",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "RegionDisabled",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
    AdministrationRow {
        status: "AccountSuspended",
        listed: false,
        error: Some("ACCOUNT_NOT_FOUND"),
    },
];

/// The accounts reported, and the accounts a batch update reaches, are the ones
/// this administrator administers in the region: its own account and every
/// account it has a live membership with. Each account it does not administer
/// is EXPLAINED, per account, rather than changed — and rather than failing the
/// whole request, so one call can both apply and explain.
///
/// The table is the whole modelled `RelationshipStatus` catalog, plus an
/// account this administrator holds no record of at all, plus the
/// administrator's own account, in ONE batch update.
#[tokio::test]
async fn the_accounts_reported_are_the_ones_this_administrator_administers() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;

    for (index, row) in ADMINISTRATION_CATALOG.iter().enumerate() {
        seed_association(&engine, REGION, &account_of(index), row.status);
    }
    // An account this administrator has no record of whatsoever: the other half
    // of what `ACCOUNT_NOT_FOUND` documents, "the account doesn't exist".
    let unknown = account_of(ADMINISTRATION_CATALOG.len());

    let before = statuses_in(&router, REGION, "before the update").await;

    let mut asked: Vec<String> = (0..ADMINISTRATION_CATALOG.len()).map(account_of).collect();
    asked.push(unknown.clone());
    asked.push(own_account());
    let updates: Vec<serde_json::Value> = asked
        .iter()
        .map(|account| serde_json::json!({ "accountId": account, "status": "ENABLED" }))
        .collect();
    let reply = batch_update(
        &router,
        REGION,
        serde_json::json!(updates),
        "the whole catalog",
    )
    .await;

    let mut expected_errors: Vec<(String, String)> = ADMINISTRATION_CATALOG
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            row.error
                .map(|code| (account_of(index), code.to_string()))
        })
        .collect();
    expected_errors.push((unknown.clone(), "ACCOUNT_NOT_FOUND".to_string()));
    expected_errors.sort();
    assert_eq!(
        update_errors("the whole catalog", &reply),
        expected_errors,
        "case \"the whole catalog\": each account the update cannot change is explained \
         by its own error code, got {reply:?}"
    );

    let after = statuses_in(&router, REGION, "after the update").await;
    let mut expected_accounts: Vec<String> = ADMINISTRATION_CATALOG
        .iter()
        .enumerate()
        .filter(|(_, row)| row.listed)
        .map(|(index, _)| account_of(index))
        .collect();
    expected_accounts.push(own_account());
    expected_accounts.sort();
    assert_eq!(
        after.keys().cloned().collect::<Vec<String>>(),
        expected_accounts,
        "case \"after the update\": only the accounts this administrator administers are \
         reported, got {after:?}"
    );

    for (index, row) in ADMINISTRATION_CATALOG.iter().enumerate() {
        let account = account_of(index);
        let case = format!("relationship status {}", row.status);
        match (row.listed, row.error) {
            // Administered and reached: the new status is what the enumeration
            // reports.
            (true, None) => assert_eq!(
                after.get(&account).map(String::as_str),
                Some("ENABLED"),
                "case {case:?}: the update reached the account, got {after:?}"
            ),
            // Administered but refused: the account keeps the status it had.
            (true, Some(_)) => assert_eq!(
                after.get(&account),
                before.get(&account),
                "case {case:?}: a refused account keeps its status, got {after:?} \
                 after {before:?}"
            ),
            // Not administered: nothing of it is reported, by the whole
            // enumeration or by a read naming it.
            _ => {
                assert!(
                    !after.contains_key(&account),
                    "case {case:?}: an account with no live membership is not reported, \
                     got {after:?}"
                );
                let named = list_accounts(
                    &router,
                    REGION,
                    std::slice::from_ref(&account),
                    None,
                    &case,
                )
                .await;
                assert_eq!(
                    named,
                    Vec::new(),
                    "case {case:?}: a read naming it reports nothing either, got {named:?}"
                );
            }
        }
    }

    // The administrator's own account was reached by the same call, so its own
    // configuration now reports what the update stored.
    assert_eq!(
        after.get(&own_account()).map(String::as_str),
        Some("ENABLED"),
        "case \"own account\": the update reached this administrator's own account, \
         got {after:?}"
    );
}

/// Macie being suspended for the administrator's OWN account is the same
/// refusal: its session is `PAUSED`, so the account is reported but a batch
/// update answers `ACCOUNT_PAUSED` for it and changes nothing. The rows are the
/// two `MacieStatus` values an enable can leave a session in.
#[tokio::test]
async fn a_suspended_session_is_explained_rather_than_changed() {
    for (case, session_status, expected_error) in [
        ("session ENABLED", "ENABLED", None),
        ("session PAUSED", "PAUSED", Some("ACCOUNT_PAUSED")),
    ] {
        let router = emulator();
        enable_in(
            &router,
            REGION,
            &serde_json::json!({ "status": session_status }).to_string(),
        )
        .await;

        let before = statuses_in(&router, REGION, case).await;
        assert_eq!(
            before.get(&own_account()).map(String::as_str),
            Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
            "case {case:?}: nothing has configured automated discovery yet, got {before:?}"
        );

        let reply = batch_update(
            &router,
            REGION,
            serde_json::json!([{ "accountId": own_account(), "status": "ENABLED" }]),
            case,
        )
        .await;
        let expected: Vec<(String, String)> = expected_error
            .map(|code| vec![(own_account(), code.to_string())])
            .unwrap_or_default();
        assert_eq!(
            update_errors(case, &reply),
            expected,
            "case {case:?}: expected {expected:?}, got {reply:?}"
        );

        let after = statuses_in(&router, REGION, case).await;
        let expected_status = if expected_error.is_some() {
            macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS
        } else {
            "ENABLED"
        };
        assert_eq!(
            after.get(&own_account()).map(String::as_str),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {after:?}"
        );
    }
}

/// The enumeration is paginated over one ascending order of accounts: every
/// page is bounded by `maxResults`, the pages concatenate to the whole set in
/// account-id order however wide they are, and the enumeration resumes exactly
/// where the token says. A `nextToken` this service did not issue names no
/// page, and a request naming more accounts than the member documents is
/// refused. The rows are the page sizes a caller asks for, the published bound
/// on `accountIds` and one past it.
#[tokio::test]
async fn the_enumeration_pages_in_account_id_order() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;

    // More accounts than the widest page, so a page boundary is crossed under
    // every row below.
    let seeded = 30;
    for index in 0..seeded {
        seed_association(&engine, REGION, &account_of(index), "Enabled");
    }
    let mut expected: Vec<String> = (0..seeded).map(account_of).collect();
    expected.push(own_account());
    expected.sort();

    // `maxResults` is modelled with a range of 1 to 25, so the rows are its two
    // bounds, a size in between, and the request that names none.
    for max_results in [None, Some(1), Some(7), Some(25)] {
        let case = format!("maxResults {max_results:?}");
        let walked = list_accounts(&router, REGION, &[], max_results, &case).await;
        let accounts: Vec<String> = walked.iter().map(|(account, _)| account.clone()).collect();
        assert_eq!(
            accounts, expected,
            "case {case:?}: the pages must concatenate to every administered account in \
             account-id order, got {accounts:?}"
        );
    }

    // A read naming a subset reports exactly the administered accounts of that
    // subset, in the same order.
    let subset: Vec<String> = vec![account_of(5), account_of(0), account_of(29)];
    let mut subset_expected = subset.clone();
    subset_expected.sort();
    let named = list_accounts(&router, REGION, &subset, Some(1), "a named subset").await;
    assert_eq!(
        named
            .iter()
            .map(|(account, _)| account.clone())
            .collect::<Vec<String>>(),
        subset_expected,
        "case \"a named subset\": a read naming accounts reports those accounts, \
         got {named:?}"
    );

    // The published bound on `accountIds` is 50 accounts: 50 is served, 51 is
    // refused.
    for (case, count, expected_status) in [("50 accountIds", 50, 200), ("51 accountIds", 51, 400)] {
        let ids: Vec<String> = (0..count).map(account_of).collect();
        let query: Vec<String> = ids
            .iter()
            .map(|account| format!("accountIds={account}"))
            .collect();
        let (status, reply) = call_in(
            &router,
            REGION,
            "GET",
            &format!("{ACCOUNTS_URI}?{}", query.join("&")),
            "",
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} with {reply:?}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(&reply),
                "ValidationException",
                "case {case:?}: the refusal must be the modelled ValidationException, \
                 got {reply:?}"
            );
        }
    }

    // A token this service did not issue names no page.
    let (status, reply) = call_in(
        &router,
        REGION,
        "GET",
        &format!("{ACCOUNTS_URI}?nextToken=notatokenthisserviceissued"),
        "",
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"foreign nextToken\": a token this service did not issue is refused, \
         got {status} with {reply:?}"
    );
    assert_eq!(
        error_code(&reply),
        "ValidationException",
        "case \"foreign nextToken\": the refusal must be the modelled ValidationException, \
         got {reply:?}"
    );
    assert!(
        !error_message(&reply).is_empty(),
        "case \"foreign nextToken\": the refusal must say why, got {reply:?}"
    );
}

/// The status an account gets when nothing has set it for the account is the
/// one the organization's auto-enable mode gives it: `ALL` enables every
/// account this administrator administers and every account that joins, `NEW`
/// only the accounts that join afterwards, and `NONE` none of them. A mode
/// change rewrites no account: an account keeps the status it is currently set
/// to. The rows are every value of `AutoEnableMode`, the enum as this crate's
/// `AUTO_ENABLE_MODE_VALUES` carries it.
#[tokio::test]
async fn the_auto_enable_mode_decides_the_status_an_account_with_none_gets() {
    for mode in macie2::ops::types::AUTO_ENABLE_MODE_VALUES {
        let case = format!("mode {mode}");
        let (router, engine) = emulator_with_engine();
        enable_in(&router, REGION, "{}").await;

        // An account this administrator already administers when the
        // configuration is written, and one that joins afterwards.
        let existing = account_of(1);
        let joining = account_of(2);
        seed_association(&engine, REGION, &existing, "Enabled");

        update(
            &router,
            REGION,
            &serde_json::json!({ "status": "ENABLED", "autoEnableOrganizationMembers": mode }),
            &case,
        )
        .await;

        associate(&router, REGION, &joining, &case).await;
        // `CreateMember` leaves the association in a status that is no live
        // membership yet, so the membership is established here — which is what
        // makes the account one this administrator administers.
        seed_association(&engine, REGION, &joining, "Enabled");

        let reported = statuses_in(&router, REGION, &case).await;
        let expected_existing = if *mode == "ALL" { "ENABLED" } else { "DISABLED" };
        let expected_joining = if *mode == "NONE" { "DISABLED" } else { "ENABLED" };
        assert_eq!(
            reported.get(&existing).map(String::as_str),
            Some(expected_existing),
            "case {case:?}: an account already administered must report \
             {expected_existing}, got {reported:?}"
        );
        assert_eq!(
            reported.get(&joining).map(String::as_str),
            Some(expected_joining),
            "case {case:?}: an account that joined under this mode must report \
             {expected_joining}, got {reported:?}"
        );
        // The administrator's own account carries the configuration's status.
        assert_eq!(
            reported.get(&own_account()).map(String::as_str),
            Some("ENABLED"),
            "case {case:?}: the configuration enabled this account, got {reported:?}"
        );

        // A mode change leaves every account where it is: what each one is set
        // to is what it keeps.
        let settled = statuses_in(&router, REGION, &case).await;
        for next_mode in macie2::ops::types::AUTO_ENABLE_MODE_VALUES
            .iter()
            .filter(|next| *next != mode && **next != "ALL")
        {
            let case = format!("{case} then {next_mode}");
            update(
                &router,
                REGION,
                &serde_json::json!({
                    "status": "ENABLED",
                    "autoEnableOrganizationMembers": next_mode,
                }),
                &case,
            )
            .await;
            let after = statuses_in(&router, REGION, &case).await;
            assert_eq!(
                after, settled,
                "case {case:?}: moving to a mode that enables no existing account must \
                 leave every account as it was, got {after:?} after {settled:?}"
            );
        }
    }
}

/// A batch update is what changes ONE account, and what it set is what the
/// account keeps across the mode changes the auto-enable doc describes: "If you
/// specify NEW or NONE, automated sensitive data discovery continues to be
/// enabled for any existing accounts that it's currently enabled for." The rows
/// are the statuses a batch update sets, each carried through both of those
/// modes.
#[tokio::test]
async fn what_a_batch_update_set_survives_a_mode_change() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;
    let member = account_of(3);
    seed_association(&engine, REGION, &member, "Enabled");

    for status in macie2::ops::types::AUTOMATED_DISCOVERY_ACCOUNT_STATUS_VALUES {
        let case = format!("status {status}");
        batch_update(
            &router,
            REGION,
            serde_json::json!([{ "accountId": &member, "status": status }]),
            &case,
        )
        .await;

        for mode in ["NEW", "NONE"] {
            let case = format!("{case} under {mode}");
            update(
                &router,
                REGION,
                &serde_json::json!({
                    "status": "ENABLED",
                    "autoEnableOrganizationMembers": mode,
                }),
                &case,
            )
            .await;
            let reported = statuses_in(&router, REGION, &case).await;
            assert_eq!(
                reported.get(&member).map(String::as_str),
                Some(*status),
                "case {case:?}: the account keeps what the batch update set it to, \
                 got {reported:?}"
            );
        }
    }
}

/// These accounts belong to the region the request signed for and to that
/// region's Macie session: both operations are refused 403
/// `AccessDeniedException` where nothing has enabled Macie, a write in one
/// region leaves another region's accounts alone, reading them changes nothing
/// a later reader sees, and `DisableMacie` deletes them with the rest of the
/// account's settings.
#[tokio::test]
async fn the_accounts_are_scoped_to_the_region_and_need_a_session() {
    let (router, engine) = emulator_with_engine();

    for (case, method, body) in [
        ("ListAutomatedDiscoveryAccounts", "GET", String::new()),
        (
            "BatchUpdateAutomatedDiscoveryAccounts",
            "PATCH",
            serde_json::json!({
                "accounts": [{ "accountId": own_account(), "status": "ENABLED" }]
            })
            .to_string(),
        ),
    ] {
        let (status, reply) = call_in(&router, REGION, method, ACCOUNTS_URI, &body).await;
        assert_eq!(
            status, 403,
            "case {case:?}: Macie is not enabled here, so the call is refused, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {case:?}: the refusal must be the modelled AccessDeniedException, \
             got {reply:?}"
        );
    }

    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;
    let member = account_of(4);
    seed_association(&engine, REGION, &member, "Enabled");
    batch_update(
        &router,
        REGION,
        serde_json::json!([
            { "accountId": &member, "status": "ENABLED" },
            { "accountId": own_account(), "status": "ENABLED" },
        ]),
        "one region",
    )
    .await;

    let elsewhere = statuses_in(&router, OTHER_REGION, "another region").await;
    assert_eq!(
        elsewhere.keys().cloned().collect::<Vec<String>>(),
        vec![own_account()],
        "case \"another region\": an association of one region is not administered in \
         another, got {elsewhere:?}"
    );
    assert_eq!(
        elsewhere.get(&own_account()).map(String::as_str),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"another region\": a write in one region must not configure another, \
         got {elsewhere:?}"
    );

    // A read reports the same thing however often it runs: nothing is written,
    // no default settles into the store.
    let first = statuses_in(&router, REGION, "repeated read").await;
    for round in 1..=2 {
        let again = statuses_in(&router, REGION, "repeated read").await;
        assert_eq!(
            again, first,
            "case \"repeated read\": read {round} after the first must report the same \
             accounts, because a read writes nothing a later caller sees; got {again:?} \
             after {first:?}"
        );
    }

    // `DisableMacie` "deletes all settings and resources for a Macie account",
    // so the per-account statuses go with the session: the region enabled again
    // administers only this account, in the never-configured status.
    let (status, reply) = call_in(&router, REGION, "DELETE", "/macie", "").await;
    assert_eq!(
        status, 200,
        "case \"disable Macie\": DisableMacie must answer 200, got {status} with {reply:?}"
    );
    enable_in(&router, REGION, "{}").await;
    let after = statuses_in(&router, REGION, "after re-enabling Macie").await;
    assert_eq!(
        after.keys().cloned().collect::<Vec<String>>(),
        vec![own_account()],
        "case \"after re-enabling Macie\": the associations went with the session, \
         got {after:?}"
    );
    assert_eq!(
        after.get(&own_account()).map(String::as_str),
        Some(macie2::state::DEFAULT_AUTOMATED_DISCOVERY_STATUS),
        "case \"after re-enabling Macie\": nothing of the deleted configuration may \
         survive, got {after:?}"
    );
}

/// An account update that names no account, or no status to give it, states no
/// change: it is refused 400 `ValidationException` and the whole request
/// persists nothing. The rows are the two members of
/// `AutomatedDiscoveryAccountUpdate` and a value outside the status enum.
#[tokio::test]
async fn an_update_outside_the_modelled_input_is_refused_and_persists_nothing() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;
    let member = account_of(5);
    seed_association(&engine, REGION, &member, "Enabled");
    let before = statuses_in(&router, REGION, "before the refusals").await;

    for (case, accounts) in [
        (
            "no accountId",
            serde_json::json!([{ "status": "ENABLED" }]),
        ),
        (
            "no status",
            serde_json::json!([{ "accountId": &member }]),
        ),
        (
            "status outside AutomatedDiscoveryAccountStatus",
            serde_json::json!([{ "accountId": &member, "status": "PAUSED" }]),
        ),
        (
            "one good account beside a bad one",
            serde_json::json!([
                { "accountId": &member, "status": "ENABLED" },
                { "status": "ENABLED" },
            ]),
        ),
    ] {
        let body = serde_json::json!({ "accounts": accounts });
        let (status, reply) =
            call_in(&router, REGION, "PATCH", ACCOUNTS_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a request outside the modelled input is refused 400, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: the refusal must be the modelled ValidationException, \
             got {reply:?}"
        );

        let after = statuses_in(&router, REGION, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused request persists nothing, got {after:?} \
             after {before:?}"
        );
    }
}
