// SPDX-License-Identifier: CC0-1.0

//! The account state the paid upgrade is conditional on, and that BOTH of its
//! branches are reachable by whoever runs the emulator — driven end to end over
//! the wire (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSFreeTierService.<Operation>`) through the same
//! multi-service router the emulator boots.
//!
//! RULE: a payment instrument is established outside this API — Free Tier models
//! no operation that adds one — so whether the account holds a vetted instrument
//! is a property of the ACCOUNT settled when this service's store is created
//! (`state::PAYMENT_INSTRUMENT_VETTED_ENV`) and changed by no request; the
//! setting alone decides which of the two answers AWS gives the paid upgrade the
//! emulator gives: the move onto the plan, reported and read back, or the
//! modelled `ValidationException` (HTTP 400) stating the service's fixed vetting
//! sentence with the plan record left exactly as it was.
//!
//! Grounding: `UpgradeAccountPlanRequest$accountPlanType` is `@required` and
//! targets `AccountPlanType`, which declares `FREE, PAID`, and the operation
//! models `ValidationException` at HTTP 400
//! (`smithy_query(query="operation",operation="UpgradeAccountPlan")`);
//! `GetAccountPlanStateResponse$accountPlanExpirationDate` is "the timestamp for
//! when the current account plan expires", which a paid plan has none of. The
//! refusal's own wording is the recorded one and is pinned in
//! `paid_plan_vetting.rs`; what this suite is about is the account state that
//! decides between the two branches.
//!
//! The rows differ along what the rule generalises over: the spelling of the
//! setting the emulator was started with — including its absence, which is the
//! state the recorded refusal was answered for — and the branch that spelling
//! puts the wire on.

use serde_json::{json, Value};
use service_com_amazonaws_freetier_awsfreetierservice::state::{
    vetted_payment_instruments, ComAmazonawsFreetierAwsfreetierserviceState,
    PAYMENT_INSTRUMENT_VETTED_ENV, PAYMENT_INSTRUMENT_VET_FAILURE, PLAN_STATUS_ACTIVE,
    PLAN_TYPE_FREE, PLAN_TYPE_PAID,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::free_tier::{call, error_code, harness, ok, refused, text};

/// Every spelling of the setting this suite drives, with whether it says the
/// account's payment instrument has been vetted.
///
/// The catalog is the one documented on `state::PAYMENT_INSTRUMENT_VETTED_ENV`'s
/// reader (`state::vetted_payment_instruments`): `1`, `true`, `yes` or `on` in
/// any case says so, and anything else — an absent setting, an empty value, `0`,
/// `no`, or a typo — leaves the account unvetted. The rows cover each accepted
/// word, a case and padding variant of two of them, the absence of the setting,
/// and four values that are not in the catalog.
const SETTINGS: [(Option<&str>, bool); 12] = [
    (None, false),
    (Some("1"), true),
    (Some("true"), true),
    (Some("TRUE"), true),
    (Some("Yes"), true),
    (Some(" on "), true),
    (Some(""), false),
    (Some("0"), false),
    (Some("false"), false),
    (Some("no"), false),
    (Some("truthy"), false),
    (Some("2"), false),
];

/// The service mounted over a store in the state `setting` establishes at
/// start-up, whatever the environment this test process itself runs in says.
///
/// The set is computed by the service's own reader of the setting, so a row
/// cannot claim a state the setting does not produce; it is written into the
/// store the way a start-up read does, and the ambient state is cleared first so
/// the row — and not the environment of the host running the suite — decides the
/// account's instrument.
fn mounted(setting: Option<&str>) -> MultiServiceRouter {
    let (router, engine) = harness();
    let account_id = engine::emulator_account_id();
    {
        let state = engine.state::<ComAmazonawsFreetierAwsfreetierserviceState>();
        let mut store = state.lock();
        store.vetted_payment_instruments.clear();
        for account in vetted_payment_instruments(account_id, setting) {
            store.vet_payment_instrument(&account);
        }
    }
    router
}

/// RULE, its state half: the setting names which accounts start with a vetted
/// payment instrument — the account this emulator presents when it says so, and
/// no account at all otherwise.
#[test]
fn the_setting_decides_which_accounts_start_vetted() {
    let account_id = engine::emulator_account_id();
    for (setting, says_vetted) in SETTINGS {
        let vetted = vetted_payment_instruments(account_id, setting);
        assert_eq!(
            vetted.contains(account_id),
            says_vetted,
            "case {setting:?}: expected the account's instrument vetted={says_vetted}, got \
             {vetted:?}"
        );
        assert_eq!(
            vetted.len(),
            usize::from(says_vetted),
            "case {setting:?}: the setting speaks for the account this emulator presents and \
             for no other, got {vetted:?}"
        );
    }
}

/// RULE, its wiring half: a store created while the environment carries the
/// setting starts in the state that setting names, so what the emulator was
/// started with is what its first request meets.
#[test]
fn a_store_starts_in_the_state_the_environment_names() {
    let account_id = engine::emulator_account_id();
    let setting = ::std::env::var(PAYMENT_INSTRUMENT_VETTED_ENV).ok();
    let expected = vetted_payment_instruments(account_id, setting.as_deref());
    let store = ComAmazonawsFreetierAwsfreetierserviceState::default();
    assert_eq!(
        store.vetted_payment_instruments, expected,
        "case {setting:?}: a new store must hold exactly the vetted accounts the setting \
         names, got {:?}",
        store.vetted_payment_instruments
    );
    assert_eq!(
        store.payment_instrument_vetted(account_id),
        expected.contains(account_id),
        "case {setting:?}: the gate must read the state the store started with, got {:?}",
        store.vetted_payment_instruments
    );
}

/// RULE, its wire half for an account the setting leaves unvetted: the paid
/// upgrade is refused with the service's fixed vetting sentence and nothing of
/// the plan record is established, however the setting misses the catalog.
#[tokio::test]
async fn a_setting_that_does_not_say_vetted_refuses_the_paid_upgrade() {
    for (setting, says_vetted) in SETTINGS {
        if says_vetted {
            continue;
        }
        let router = mounted(setting);
        let (status, payload) = call(
            &router,
            "UpgradeAccountPlan",
            &json!({"accountPlanType": PLAN_TYPE_PAID}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {setting:?}: expected HTTP 400, got {status} with {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ValidationException",
            "case {setting:?}: expected the modelled ValidationException, got {payload}"
        );
        assert_eq!(
            text(&payload, "message"),
            PAYMENT_INSTRUMENT_VET_FAILURE,
            "case {setting:?}: expected the service's fixed vetting sentence, got {payload}"
        );

        // Nothing was established: the account still holds no plan record, which
        // this service reports as the modelled not-found error (at the status its
        // recordings carry, which `resource_not_found_status.rs` pins).
        refused(
            &router,
            "GetAccountPlanState",
            &json!({}),
            400,
            "ResourceNotFoundException",
        )
        .await;
    }
}

/// RULE, its wire half for an account the setting says is vetted: the paid plan
/// is reached over the wire with no request but the modelled one, the following
/// read reports it, a paid plan reports no expiration instant, and the free
/// plan's one-off window is then closed to it.
#[tokio::test]
async fn a_setting_that_says_vetted_reaches_the_paid_plan_over_the_wire() {
    for (setting, says_vetted) in SETTINGS {
        if !says_vetted {
            continue;
        }
        let router = mounted(setting);
        let upgraded = ok(
            &router,
            "UpgradeAccountPlan",
            &json!({"accountPlanType": PLAN_TYPE_PAID}),
        )
        .await;
        assert_eq!(
            text(&upgraded, "accountPlanType"),
            PLAN_TYPE_PAID,
            "case {setting:?}: the account must be moved onto the paid plan, got {upgraded}"
        );
        assert_eq!(
            text(&upgraded, "accountPlanStatus"),
            PLAN_STATUS_ACTIVE,
            "case {setting:?}: the plan it was moved onto is in force, got {upgraded}"
        );

        // Round trip: the write is reported by this service's own reader.
        let reported = ok(&router, "GetAccountPlanState", &json!({})).await;
        for member in ["accountId", "accountPlanType", "accountPlanStatus"] {
            assert_eq!(
                text(&reported, member),
                text(&upgraded, member),
                "case {setting:?}: the upgrade and the following read must agree on {member}, \
                 got {reported} after {upgraded}"
            );
        }
        assert_eq!(
            reported.get("accountPlanExpirationDate"),
            None::<&Value>,
            "case {setting:?}: a paid plan does not expire, so it reports no expiration \
             instant, got {reported}"
        );

        // The free plan is a one-off window, and this account has left it: the
        // refusal that branch carries is reachable over the wire too.
        refused(
            &router,
            "UpgradeAccountPlan",
            &json!({"accountPlanType": PLAN_TYPE_FREE}),
            400,
            "ValidationException",
        )
        .await;
        let still_paid = ok(&router, "GetAccountPlanState", &json!({})).await;
        assert_eq!(
            still_paid, reported,
            "case {setting:?}: a refused move back to the free plan must leave the paid plan \
             exactly as it was, got {still_paid} after {reported}"
        );
    }
}
