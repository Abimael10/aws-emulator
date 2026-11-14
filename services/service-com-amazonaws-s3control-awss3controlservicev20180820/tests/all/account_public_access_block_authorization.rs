// SPDX-License-Identifier: CC0-1.0

//! Whether the account-level `PublicAccessBlock` refusal reveals what the named
//! account holds, driven end to end over HTTP.
//!
//! `GetPublicAccessBlock`, `PutPublicAccessBlock` and `DeletePublicAccessBlock`
//! each carry their `AccountId` in the `{AccountId}.` host prefix of the endpoint
//! they are sent to and in the `x-amz-account-id` header beside it
//! (`smithy_query(query="operation",operation="GetPublicAccessBlock")`:
//! `AccountId` is `@required`, `@hostLabel`, `httpHeader: x-amz-account-id`,
//! `@length` max 64, `@pattern ^\d{12}$`, and the operation's
//! `endpoint.hostPrefix` is `{AccountId}.`). That account says WHOSE control
//! plane the call is for, so it is authorized before the configuration is read,
//! written or removed — which makes the answer for an account the caller holds no
//! credentials for the SAME refusal whether or not that account has a
//! configuration, and leaves whatever it has exactly as it was.
//!
//! The rows below span that class: a foreign account holding every setting on,
//! one holding an all-false configuration, one holding a mixed configuration, and
//! one holding none at all — the last being the baseline every other row's
//! refusal must match element for element, since a refusal that differed would be
//! an existence oracle for another account's state. The account VALUES a caller
//! may name are the sibling [`crate::public_access_block`] suite's table; the
//! harness, the readers and the shared assertions are that suite's too, so this
//! one states only the dimension it adds.
//!
//! A configuration can be put on a foreign account only in the store the
//! configurations live in (Amazon S3's own, which this service reads and writes
//! them through): the front door refuses every call addressed in one, which is
//! the very rule under test.

use crate::public_access_block::{
    assert_denied, assert_nothing_reported, stated, Emulator, Stated, OTHER_ACCOUNT, THIRD_ACCOUNT,
};

/// Two further foreign accounts, so the rows differ in more than the two the
/// sibling suite names. Both are 12 digits, as `AccountId`'s `^\d{12}$` requires.
const FOURTH_ACCOUNT: &str = "210987654323";
const FIFTH_ACCOUNT: &str = "210987654324";

#[tokio::test]
async fn the_refusal_does_not_depend_on_what_the_foreign_account_holds() {
    let emulator = Emulator::start();

    // One row per state a foreign account can be in, differing in whether it
    // holds a configuration and in which of its settings are on — the all-false
    // configuration among them, which is a configuration like any other. The
    // LAST row holds none, and is the baseline the others are compared against.
    let rows: Vec<(&str, &str, Option<[bool; 4]>)> = vec![
        (
            "a foreign account holding every setting on",
            OTHER_ACCOUNT,
            Some([true; 4]),
        ),
        (
            "a foreign account holding an all-false configuration",
            THIRD_ACCOUNT,
            Some([false; 4]),
        ),
        (
            "a foreign account holding a mixed configuration",
            FOURTH_ACCOUNT,
            Some([false, true, true, false]),
        ),
        (
            "a foreign account holding no configuration",
            FIFTH_ACCOUNT,
            None,
        ),
    ];
    for (case, account, held) in &rows {
        if let Some(settings) = held {
            emulator.seed(case, account, *settings);
        }
    }

    // The answer the account holding NOTHING gets, per operation: what every
    // other row must match, element for element, for the refusal to be no oracle
    // for whether that account has a configuration.
    let (baseline_case, baseline_account, _) = rows.last().expect("the rows end with the baseline");
    let baseline: Vec<(&'static str, Stated)> = emulator
        .every_call(baseline_account)
        .await
        .iter()
        .map(|(operation, reply)| (*operation, stated(reply)))
        .collect();

    for (case, account, held) in &rows {
        for (index, (operation, reply)) in emulator.every_call(account).await.iter().enumerate() {
            let case = format!("{operation} for {case}");
            assert_denied(&case, account, reply);
            assert_nothing_reported(&case, reply);
            let (baseline_operation, expected_statement) = &baseline[index];
            assert_eq!(
                *operation, *baseline_operation,
                "case {case:?}: the rows must compare the same operation"
            );
            assert_eq!(
                &stated(reply),
                expected_statement,
                "case {case:?}: expected the same refusal as {baseline_case:?}, got body {}",
                reply.body
            );
        }
        // Neither the refused put nor the refused delete changed what that
        // account holds: the configuration is still exactly as it was, and an
        // account that held none still holds none.
        assert_eq!(
            emulator.held(account),
            *held,
            "case {case:?}: expected the refused calls to leave {account} holding {held:?}"
        );
    }
}
