// SPDX-License-Identifier: CC0-1.0

//! What a REPEATED `UpdateCapacityManagerMonitoredTagKeys` leaves behind.
//!
//! The rule under test: an update that asks for a move a tag key is already making neither
//! restarts nor postpones that move, so presenting the same request again leaves the
//! account exactly where the first presentation did — in both directions. An activating key
//! still reaches `activated` one transition span after the FIRST request that asked for it,
//! and a deactivating key is monitored no more one span after the FIRST request that asked
//! for that, however many times a client retries the request in between (which is what makes
//! the modelled `ClientToken` — "unique, case-sensitive identifier that you provide to
//! ensure the idempotency of the request" — need nothing stored for it).
//!
//! Each row of the tables below differs along what the rule generalises over: which
//! direction the repeat asks for, how many tag keys the request names, whether the key had
//! settled into its steady status before the repeat, and which `ClientToken` each
//! presentation carries — the same one, a fresh one, none at all, or one only on the repeat.
//! No row is the specification.
//!
//! Driven over the wire the service speaks (`ec2Query`, through [`super::ec2_query`]), so
//! what these tests read is what a client reads.

use std::time::{Duration, Instant};

use ec2::state::{
    MONITORED_TAG_KEY_STATUS_ACTIVATED, MONITORED_TAG_KEY_STATUS_ACTIVATING,
    MONITORED_TAG_KEY_STATUS_DEACTIVATING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, indexed};
use super::wire::{Param, child_text, document, param, set_items};

/// The Region every call below is signed for: the monitored keys are a fact of the account
/// rather than of one Region, which the suite beside this one pins, so one Region is enough
/// to ask about a repeat.
const REGION: &str = "us-west-2";

/// How long a repeat loop below is given before it reports that a key never left its
/// transient status — generously past [`ec2::state::TRANSITION_DELAY`], so a slow machine
/// fails nothing, while a move the retries keep pushing away still ends the test instead of
/// hanging.
const POLL_DEADLINE: Duration = Duration::from_secs(10);

/// How long a repeat loop waits between presentations, as a client's own retry does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// `(tag key, status)` for every key a response reported, in the order it reported them.
fn reported(case: &str, body: &str) -> Vec<(String, String)> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "capacityManagerTagKeySet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "tagKey").unwrap_or_default(),
                child_text(item, "status").unwrap_or_default(),
            )
        })
        .collect()
}

/// Just the tag keys of a reported set, in the order it reported them.
fn keys_of(entries: &[(String, String)]) -> Vec<String> {
    entries.iter().map(|(tag_key, _)| tag_key.clone()).collect()
}

/// The status a reported set holds for `tag_key`, or `None` when it reported no such key.
fn status_of(entries: &[(String, String)], tag_key: &str) -> Option<String> {
    entries
        .iter()
        .find(|(held, _)| held == tag_key)
        .map(|(_, status)| status.clone())
}

/// The form fields of one update — the keys it activates, the keys it deactivates and the
/// `ClientToken` it carries, if any — as ec2Query flattens the two lists.
fn update_params(activate: &[&str], deactivate: &[&str], token: Option<&str>) -> Vec<Param> {
    let mut params = indexed("ActivateTagKey", activate);
    params.extend(indexed("DeactivateTagKey", deactivate));
    if let Some(token) = token {
        params.push(param("ClientToken", token));
    }
    params
}

/// One `UpdateCapacityManagerMonitoredTagKeys` call that has to succeed, and the keys it
/// reported as affected.
async fn update_ok(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "UpdateCapacityManagerMonitoredTagKeys",
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateCapacityManagerMonitoredTagKeys, \
         got {status} with body {body}"
    );
    reported(case, &body)
}

/// The keys the account monitors, through `GetCapacityManagerMonitoredTagKeys`.
async fn monitored(case: &str, router: &MultiServiceRouter) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "GetCapacityManagerMonitoredTagKeys",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetCapacityManagerMonitoredTagKeys, \
         got {status} with body {body}"
    );
    reported(case, &body)
}

/// Present `params` again and again, as a client retrying an idempotent request does,
/// reading the account's monitored keys after each presentation, until `tag_key` holds
/// `want` or a generous deadline passes. Answers the status the key then held and how many
/// repeats were presented.
///
/// This is what asks the rule: a move the retries restarted or postponed would never reach
/// `want`, whatever the host's speed, while one the first request fixed the instant of
/// arrives under every retry rate.
async fn repeat_until(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
    tag_key: &str,
    want: Option<&str>,
) -> (Option<String>, usize) {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut repeats = 0usize;
    loop {
        let held = status_of(&monitored(case, router).await, tag_key);
        if held.as_deref() == want || Instant::now() >= deadline {
            return (held, repeats);
        }
        update_ok(case, router, params).await;
        repeats += 1;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `(case, the keys the request names, the token the first presentation carries, the token
/// every repeat carries)`.
type Row = (
    &'static str,
    &'static [&'static str],
    Option<&'static str>,
    Option<&'static str>,
);

/// The token dimension, spanning what a retrying client sends: the same identifier again, a
/// fresh one, none at all, and one only on the repeat.
const ROWS: [Row; 4] = [
    (
        "the same client token presented again",
        &["Project"],
        Some("retry-token"),
        Some("retry-token"),
    ),
    (
        "a fresh client token on every repeat",
        &["Alpha", "Beta"],
        Some("first-token"),
        Some("second-token"),
    ),
    (
        "no client token at all",
        &["cost-centre.01_x/y:z", "Owner", "Zeta"],
        None,
        None,
    ),
    (
        "a client token only on the repeats",
        &["Late"],
        None,
        Some("late-token"),
    ),
];

/// A repeated ACTIVATION leaves the account where the first one did: it affects exactly the
/// same keys, and the keys still reach `activated` one transition span after the first
/// request rather than one span after the last retry.
#[tokio::test]
async fn a_repeated_activation_does_not_restart_the_activation() {
    for (case, keys, first_token, repeat_token) in ROWS {
        let router = emulator();
        let first = update_ok(case, &router, &update_params(keys, &[], first_token)).await;
        let mut expected = keys.to_vec();
        expected.sort_unstable();
        assert_eq!(
            keys_of(&first),
            expected
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the activation to affect {expected:?}, got {first:?}"
        );
        for (tag_key, status) in &first {
            assert_eq!(
                status, MONITORED_TAG_KEY_STATUS_ACTIVATING,
                "case {case:?}: expected {tag_key:?} to start \
                 {MONITORED_TAG_KEY_STATUS_ACTIVATING}, got {status:?}"
            );
        }

        // The repeat affects the same keys — the account asked for those keys to be
        // monitored, which they already are.
        let repeat_params = update_params(keys, &[], repeat_token);
        let repeat = update_ok(case, &router, &repeat_params).await;
        assert_eq!(
            keys_of(&repeat),
            keys_of(&first),
            "case {case:?}: expected the repeat to affect the same keys as the first \
             request ({first:?}), got {repeat:?}"
        );

        // … and retrying it does not hold any of them in the transient status.
        for tag_key in keys {
            let (held, repeats) = repeat_until(
                case,
                &router,
                &repeat_params,
                tag_key,
                Some(MONITORED_TAG_KEY_STATUS_ACTIVATED),
            )
            .await;
            assert_eq!(
                held.as_deref(),
                Some(MONITORED_TAG_KEY_STATUS_ACTIVATED),
                "case {case:?}: expected {tag_key:?} to reach \
                 {MONITORED_TAG_KEY_STATUS_ACTIVATED} under {repeats} repeat(s) of the \
                 activation, got {held:?}"
            );
        }

        let held = monitored(case, &router).await;
        assert_eq!(
            keys_of(&held),
            keys_of(&first),
            "case {case:?}: expected the account to monitor exactly {first:?}, got {held:?}"
        );
    }
}

/// A repeated DEACTIVATION does not postpone the end of monitoring: the key is monitored no
/// more one transition span after the first request that asked for it, however many times
/// the same request is presented in between, and the account's other keys are untouched.
#[tokio::test]
async fn a_repeated_deactivation_does_not_postpone_the_end_of_monitoring() {
    for (case, keys, first_token, repeat_token) in ROWS {
        // The first key named is the one deactivated; everything else the account monitors
        // has to be left exactly where it was, so a row naming several keys asks that too.
        let going = keys[0];
        let staying: Vec<&str> = keys[1..].to_vec();
        for settle_first in [false, true] {
            let case = format!(
                "{case} ({})",
                if settle_first {
                    "deactivated once it had activated"
                } else {
                    "deactivated while it was still activating"
                }
            );
            let case = case.as_str();
            let router = emulator();
            update_ok(case, &router, &update_params(keys, &[], first_token)).await;
            if settle_first {
                let (held, _) = repeat_until(
                    case,
                    &router,
                    &update_params(keys, &[], first_token),
                    going,
                    Some(MONITORED_TAG_KEY_STATUS_ACTIVATED),
                )
                .await;
                assert_eq!(
                    held.as_deref(),
                    Some(MONITORED_TAG_KEY_STATUS_ACTIVATED),
                    "case {case:?}: expected {going:?} activated before the deactivation, \
                     got {held:?}"
                );
            }

            let deactivation = update_params(&[], &[going], first_token);
            let first = update_ok(case, &router, &deactivation).await;
            assert_eq!(
                status_of(&first, going).as_deref(),
                Some(MONITORED_TAG_KEY_STATUS_DEACTIVATING),
                "case {case:?}: expected {going:?} to start \
                 {MONITORED_TAG_KEY_STATUS_DEACTIVATING}, got {first:?}"
            );

            // The retries: each one asks for the move already under way, so the key leaves
            // the monitored set at the instant the FIRST request set, not one span after
            // the last retry.
            let repeated = update_params(&[], &[going], repeat_token);
            let (held, repeats) = repeat_until(case, &router, &repeated, going, None).await;
            assert_eq!(
                held, None,
                "case {case:?}: expected {going:?} to be monitored no more under {repeats} \
                 repeat(s) of the deactivation, got {held:?}"
            );

            let remaining = monitored(case, &router).await;
            assert_eq!(
                keys_of(&remaining),
                {
                    let mut expected = staying.clone();
                    expected.sort_unstable();
                    expected
                        .iter()
                        .map(|key| (*key).to_string())
                        .collect::<Vec<String>>()
                },
                "case {case:?}: expected the account's other keys untouched, got {remaining:?}"
            );

            // A further deactivation of a key the account no longer monitors affects
            // nothing at all, which is the same request answered as the idempotent
            // no-change it now is.
            let after = update_ok(case, &router, &repeated).await;
            assert!(
                after.is_empty(),
                "case {case:?}: expected a deactivation of a key the account no longer \
                 monitors to affect nothing, got {after:?}"
            );
        }
    }
}
