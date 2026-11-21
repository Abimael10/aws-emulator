// SPDX-License-Identifier: CC0-1.0

//! The vault-lock RETENTION-DAYS rule answers ONE fixed sentence.
//!
//! The rule under test: `PutBackupVaultLockConfiguration` treats its two retention
//! bounds as a single rule — each must fall inside the retention span the service
//! publishes and the minimum must not exceed the maximum — and refuses ANY
//! violation of that rule with HTTP 400 `InvalidParameterValueException` carrying
//! one fixed sentence that states the published span and contains no member name
//! and no value taken from the request, leaving the vault unlocked with no stored
//! bounds.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! behaviour a `backup` SDK client sees. The refusal table spans both kinds of
//! violation (a bound outside the span, on either side of either bound, and a
//! minimum above the maximum at several magnitudes) and both together, and asserts
//! every row answers the same bytes — which is what rules out a message built from
//! the request. The acceptance table is the positive control: every bound ON a
//! published limit, and equal bounds, are stored and read back through
//! `DescribeBackupVault`.
//!
//! The harness is the one `vault_read_visibility.rs` publishes to this test crate.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_read_visibility::{call, error_code, message, prepare, router, Answer, Setup};

/// The one sentence AWS Backup answers for the whole retention-days rule class.
/// FIXED text in full: it states the published span in the service's own spelling
/// (thousands separator, closing period) and names neither member nor any
/// submitted value, which is why it can be asserted verbatim.
const RETENTION_DAYS_MESSAGE: &str = "Backup vault Lock configuration retention days must be \
                                      between 1 and 36,500, and minimum must be less than or \
                                      equal to maximum.";

/// The shortest retention a vault lock accepts, as the model documents for
/// `MinRetentionDays`: "The shortest minimum retention period you can specify is 1
/// day" (`smithy_query(query="doc",shape="PutBackupVaultLockConfigurationInput",
/// member="MinRetentionDays")`).
const MIN_RETENTION_DAYS: i64 = 1;

/// The longest retention a vault lock accepts, as the model documents for
/// `MaxRetentionDays`: "The longest maximum retention period you can specify is
/// 36500 days" (`smithy_query(query="doc",
/// shape="PutBackupVaultLockConfigurationInput",member="MaxRetentionDays")`).
const MAX_RETENTION_DAYS: i64 = 36_500;

/// The smallest cooling-off span the model admits ("you must set ChangeableForDays
/// to 3 or greater"), so rows that carry one stay inside their own span and test
/// only the retention rule.
const CHANGEABLE_FOR_DAYS: i64 = 3;

async fn put_lock(router: &MultiServiceRouter, vault: &str, body: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("/backup-vaults/{vault}/vault-lock"),
        Some(body),
    )
    .await
}

async fn describe(router: &MultiServiceRouter, vault: &str) -> Answer {
    call(router, "GET", &format!("/backup-vaults/{vault}"), None).await
}

/// Every violation of the retention-days rule is refused with the same 400
/// `InvalidParameterValueException` and the same fixed sentence, and stores
/// nothing: the vault still reads back unlocked with neither bound set.
#[tokio::test]
async fn a_retention_days_violation_is_refused_with_one_fixed_sentence() {
    let refused: Vec<(&str, String)> = vec![
        // Each bound stepped one past a published limit, on both sides.
        (
            "a minimum one day below the shortest retention",
            format!(r#"{{"MinRetentionDays":{}}}"#, MIN_RETENTION_DAYS - 1),
        ),
        (
            "a minimum one day past the longest retention",
            format!(r#"{{"MinRetentionDays":{}}}"#, MAX_RETENTION_DAYS + 1),
        ),
        (
            "a maximum one day below the shortest retention",
            format!(r#"{{"MaxRetentionDays":{}}}"#, MIN_RETENTION_DAYS - 1),
        ),
        (
            "a maximum one day past the longest retention",
            format!(r#"{{"MaxRetentionDays":{}}}"#, MAX_RETENTION_DAYS + 1),
        ),
        (
            "a negative minimum",
            r#"{"MinRetentionDays":-365}"#.to_string(),
        ),
        (
            "a maximum far past the longest retention",
            r#"{"MaxRetentionDays":1000000}"#.to_string(),
        ),
        // The minimum above the maximum, at several magnitudes and orderings of
        // the two members in the body.
        (
            "a minimum one day above the maximum",
            r#"{"MinRetentionDays":8,"MaxRetentionDays":7}"#.to_string(),
        ),
        (
            "a month-long minimum above a week-long maximum",
            r#"{"MinRetentionDays":30,"MaxRetentionDays":7}"#.to_string(),
        ),
        (
            "the longest minimum above the shortest maximum",
            format!(
                r#"{{"MaxRetentionDays":{MIN_RETENTION_DAYS},"MinRetentionDays":{MAX_RETENTION_DAYS}}}"#
            ),
        ),
        (
            "an inverted pair alongside a cooling-off span",
            format!(
                r#"{{"MinRetentionDays":2555,"MaxRetentionDays":365,"ChangeableForDays":{CHANGEABLE_FOR_DAYS}}}"#
            ),
        ),
        // Both kinds of violation at once.
        (
            "a bound past the longest retention that is also inverted",
            format!(
                r#"{{"MinRetentionDays":{},"MaxRetentionDays":7}}"#,
                MAX_RETENTION_DAYS + 1
            ),
        ),
    ];

    let mut answers: Vec<(&str, Answer)> = Vec::new();
    for (case, body) in &refused {
        let router = router();
        prepare(&router, Setup::Created, "retention-vault").await;

        let refusal = put_lock(&router, "retention-vault", body).await;
        assert_eq!(
            (refusal.status, error_code(&refusal.body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {case:?}: expected 400 InvalidParameterValueException, got {refusal:?}"
        );
        assert_eq!(
            message(&refusal.body),
            RETENTION_DAYS_MESSAGE,
            "case {case:?}: expected the fixed retention-days sentence, got {:?}",
            message(&refusal.body)
        );
        for member in ["MinRetentionDays", "MaxRetentionDays"] {
            assert!(
                !message(&refusal.body).contains(member),
                "case {case:?}: the refusal must not name {member}, got {:?}",
                message(&refusal.body)
            );
        }

        let described = describe(&router, "retention-vault").await;
        assert_eq!(
            described.body.get("Locked").and_then(Value::as_bool),
            Some(false),
            "case {case:?}: a refused configuration must store nothing, got {:?}",
            described.body
        );
        for member in ["MinRetentionDays", "MaxRetentionDays", "LockDate"] {
            assert!(
                described.body.get(member).is_none(),
                "case {case:?}: a refused configuration must leave {member} unset, got {:?}",
                described.body
            );
        }
        answers.push((case, refusal));
    }

    // One sentence for the whole class: no row's answer may differ from another's,
    // which is what a message built from the request could not satisfy.
    let (first_case, first) = &answers[0];
    for (case, answer) in &answers[1..] {
        assert_eq!(
            answer, first,
            "case {case:?}: every retention-days refusal must answer exactly what {first_case:?} \
             answered, got {answer:?} and {first:?}"
        );
    }
}

/// One row of the acceptance table: what the put asks for, and what the vault must
/// then report.
#[derive(Debug, Clone, Copy)]
struct Accepted {
    case: &'static str,
    body: &'static str,
    min_days: Option<i64>,
    max_days: Option<i64>,
}

/// The positive control: bounds ON a published limit, equal bounds, and a single
/// bound are inside the rule, so each is stored and read back.
#[tokio::test]
async fn retention_bounds_inside_the_rule_are_stored() {
    let accepted = [
        Accepted {
            case: "a minimum at the shortest retention",
            body: r#"{"MinRetentionDays":1}"#,
            min_days: Some(1),
            max_days: None,
        },
        Accepted {
            case: "a maximum at the longest retention",
            body: r#"{"MaxRetentionDays":36500}"#,
            min_days: None,
            max_days: Some(36500),
        },
        Accepted {
            case: "both bounds spanning the whole published range",
            body: r#"{"MinRetentionDays":1,"MaxRetentionDays":36500}"#,
            min_days: Some(1),
            max_days: Some(36500),
        },
        Accepted {
            case: "equal bounds",
            body: r#"{"MinRetentionDays":7,"MaxRetentionDays":7,"ChangeableForDays":3}"#,
            min_days: Some(7),
            max_days: Some(7),
        },
        Accepted {
            case: "equal bounds at the longest retention",
            body: r#"{"MinRetentionDays":36500,"MaxRetentionDays":36500}"#,
            min_days: Some(36500),
            max_days: Some(36500),
        },
        Accepted {
            case: "a minimum below its maximum",
            body: r#"{"MinRetentionDays":7,"MaxRetentionDays":2555}"#,
            min_days: Some(7),
            max_days: Some(2555),
        },
        Accepted {
            case: "neither bound",
            body: r#"{"ChangeableForDays":3}"#,
            min_days: None,
            max_days: None,
        },
    ];

    for Accepted {
        case,
        body,
        min_days,
        max_days,
    } in accepted
    {
        let router = router();
        prepare(&router, Setup::Created, "accepted-vault").await;

        let put = put_lock(&router, "accepted-vault", body).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: the configuration must be accepted, got {put:?}"
        );
        let described = describe(&router, "accepted-vault").await;
        assert_eq!(
            described.body.get("Locked").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the vault to read back Locked, got {:?}",
            described.body
        );
        assert_eq!(
            (
                described
                    .body
                    .get("MinRetentionDays")
                    .and_then(Value::as_i64),
                described
                    .body
                    .get("MaxRetentionDays")
                    .and_then(Value::as_i64),
            ),
            (min_days, max_days),
            "case {case:?}: expected the stored bounds, got {:?}",
            described.body
        );
    }
}

/// A refused retention rule leaves an EARLIER accepted configuration standing: the
/// refusal is a parameter decision taken before anything is stored, so it neither
/// replaces nor clears the bounds the vault already carries.
#[tokio::test]
async fn a_refused_retention_rule_leaves_the_stored_configuration_standing() {
    let router = router();
    prepare(&router, Setup::Created, "relock-vault").await;

    let applied = put_lock(
        &router,
        "relock-vault",
        r#"{"MinRetentionDays":7,"MaxRetentionDays":30}"#,
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "setup: the first configuration must be accepted, got {applied:?}"
    );
    let before = describe(&router, "relock-vault").await;

    for (case, body) in [
        (
            "an inverted pair",
            r#"{"MinRetentionDays":30,"MaxRetentionDays":7}"#,
        ),
        ("a bound below the span", r#"{"MinRetentionDays":0}"#),
        ("a bound past the span", r#"{"MaxRetentionDays":36501}"#),
    ] {
        let refusal = put_lock(&router, "relock-vault", body).await;
        assert_eq!(
            (refusal.status, message(&refusal.body).as_str()),
            (400, RETENTION_DAYS_MESSAGE),
            "case {case:?}: expected the fixed retention-days refusal, got {refusal:?}"
        );
        let after = describe(&router, "relock-vault").await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused configuration must leave the stored one standing, \
             got {after:?} after {before:?}"
        );
    }
}
