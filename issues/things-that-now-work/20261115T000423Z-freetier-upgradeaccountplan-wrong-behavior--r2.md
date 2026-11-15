Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# UpgradeAccountPlan(PAID) can never succeed over the wire: nothing in the emulator ever vets a payment instrument, so the paid half of the plan state machine is unreachable

_Filed: 2026-11-15T00:04:23Z_
_Operation: UpgradeAccountPlan | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpgradeAccountPlan` gates the move onto the PAID plan on `store.payment_instrument_vetted(account_id)`. That set (`ComAmazonawsFreetierAwsfreetierserviceState::vetted_payment_instruments`) is initialised EMPTY by `Default` and the ONLY function that ever inserts into it is `vet_payment_instrument`, which no handler, no sibling service crate and nothing else in the assembled emulator calls — a repo-wide grep for `vet_payment_instrument` finds exactly one definition in `src/state.rs` and five call sites, every one of them inside this crate's own `tests/all/*.rs`.

So for every caller of the emulator, `UpgradeAccountPlan` with `accountPlanType: "PAID"` — one of the two values the `@required` `AccountPlanType` member admits — is a guaranteed HTTP 400 `ValidationException` ("Account plan upgrade failed PI vet failure"), forever, with no request and no sequence of requests that can satisfy the precondition. The gate is not a condition a client can meet; it is a permanent refusal of half of the operation's modelled input domain.

The damage is not confined to this one operation, because `UpgradeAccountPlan` is this service's ONLY writer of plan state (`src/state.rs:10-15`). With PAID unreachable:

* `GetAccountPlanState` can never report `accountPlanType: "PAID"`, although `GetAccountPlanStateResponse$accountPlanType` is `@required` and `AccountPlanType` declares `FREE | PAID`;
* `AccountPlan::upgrade_to`'s paid branch (`if plan_type == PLAN_TYPE_PAID { self.expiration_date = None; }`, `src/state.rs:262-264`) is dead from the wire, so no response can ever omit `accountPlanExpirationDate` for a paid plan;
* `AccountPlan::free_window_open`'s "it left the free plan" clause, and therefore `UpgradeAccountPlan`'s own free-window refusal for a PAID account (`src/ops/upgrade_account_plan.rs:232-238`), are dead from the wire too — the only way to reach them is `plan_type != FREE`, which only a PAID upgrade can produce.

The crate is internally inconsistent about this. `src/state.rs:22-26` justifies the set starting empty with "It starts EMPTY, like `plans`" — but `plans` starts empty *because* the service has a writer for it ("an account has account-plan data only once the service's one plan-writing operation has established it", `src/state.rs:10-14`), and this set has none. And `tests/all/paid_plan_vetting.rs:158-161` asserts in so many words that "the refusal above is the vetting gate and not a paid plan the emulator cannot reach" — a claim it can only make because the test reaches past the API and writes the set directly from Rust (`store.vet_payment_instrument(account_id)`, `tests/all/paid_plan_vetting.rs:82`). No client can do that.

## Evidence

The gate — `services/service-com-amazonaws-freetier-awsfreetierservice/src/ops/upgrade_account_plan.rs:206-216`:
```
        // The paid plan is billed, so moving an account onto it is conditional
        // on the account holding a payment instrument that has been vetted: an
        // account with none is refused and nothing of its plan record is read
        // or written, so the refusal answers the same way however often it is
        // made. Free Tier models no operation that establishes an instrument,
        // so the vetting is state this service only reads.
        if target == crate::state::PLAN_TYPE_PAID && !store.payment_instrument_vetted(account_id) {
            return Err(stated_verbatim(
                crate::state::PAYMENT_INSTRUMENT_VET_FAILURE,
            ));
        }
```

The set it reads, initialised empty — `src/state.rs:419-428`:
```
    fn default() -> Self {
        let account_id = caller_account_id().to_string();
        let now = now();
        Self {
            plans: HashMap::new(),
            activities: HashMap::from([(account_id.clone(), seed_activities(now))]),
            usages: HashMap::from([(account_id, seed_usages())]),
            vetted_payment_instruments: HashSet::new(),
        }
    }
```

The only writer, and its own admission that nothing in the service surface reaches it — `src/state.rs:468-476`:
```
    /// Record that the account's payment instrument has been vetted, so the
    /// move onto the paid plan is open to it.
    ///
    /// This is what a vetting outside this service leaves behind; nothing in
    /// Free Tier's own surface establishes a payment instrument.
    pub fn vet_payment_instrument(&mut self, account_id: &str) {
        self.vetted_payment_instruments
            .insert(account_id.to_string());
    }
```
and `src/state.rs:397-404`:
```
    /// A payment instrument is established outside this service — Free Tier
    /// models no operation that adds one, and none of its three other
    /// operations reads this — so an account is in here only once something put
    /// it here, and the emulator's own account is not.
    pub vetted_payment_instruments: HashSet<String>,
```
The "something" never happens: a repo-wide grep for `vet_payment_instrument|vetted_payment_instruments` returns the definition and field in `src/state.rs` (lines 22, 404, 426, 465, 473-475) plus call sites ONLY in `tests/all/wire_contract.rs:138,142,401`, `tests/all/paid_plan_vetting.rs:82`, `tests/all/identity_authorization.rs:119`, `tests/all/free_tier.rs:69`, `tests/all/account_plan_window.rs:77`. No handler, no other service crate, no router wiring.

The false analogy the crate uses to justify the empty start — `src/state.rs:10-26`:
```
//! * `plans` — one [`AccountPlan`] singleton per account, reported by
//!   `GetAccountPlanState` and written by `UpgradeAccountPlan`. It starts EMPTY:
//!   an account has account-plan data only once the service's one plan-writing
//!   operation has established it, ...
//! * `vetted_payment_instruments` — the accounts whose payment instrument has
//!   been vetted, which is the precondition `UpgradeAccountPlan` checks before
//!   it moves an account onto the paid plan
//!   ([`ComAmazonawsFreetierAwsfreetierserviceState::payment_instrument_vetted`]).
//!   It starts EMPTY, like `plans`.
```
`plans` has a writer reachable from the wire; `vetted_payment_instruments` does not, so "like `plans`" does not hold.

The test that claims the opposite, and the out-of-band write it needs to make the claim — `tests/all/paid_plan_vetting.rs:158-161`:
```
/// RULE 1, its other branch: an account whose payment instrument IS vetted is
/// moved onto the paid plan, and the following read reports what was stored —
/// so the refusal above is the vetting gate and not a paid plan the emulator
/// cannot reach.
```
and `tests/all/paid_plan_vetting.rs:68-85`, where "IS vetted" is produced by writing the store from Rust, not by any request:
```
fn mounted(stored: StoredPlan, vetted: bool) -> MultiServiceRouter {
    let (router, engine) = harness();
    ...
        if vetted {
            store.vet_payment_instrument(account_id);
        }
```
`tests/all/free_tier.rs:58-71` (`vetted_router`), `tests/all/account_plan_window.rs:73-77` and `tests/all/identity_authorization.rs:119` all do the same, which is why no test over the wire alone ever observes the dead branch.

The modelled input domain the gate permanently halves — `smithy_query(query="shape", shape="UpgradeAccountPlanResponse")` / `query="operation"`:
* `UpgradeAccountPlanRequest$accountPlanType`: `{"target": "AccountPlanType", "required": true, "type": "enum"}`, doc "The target account plan type. This makes it explicit about the change and latest value of the accountPlanType."
* `AccountPlanType` → `["FREE", "PAID"]`
* `UpgradeAccountPlanResponse$accountPlanType`: `{"required": true}`, doc "The type of plan for the account."
* `GetAccountPlanStateResponse$accountPlanType`: `{"required": true}`, doc "The plan type for the account."

And the branches that go dead with it — `src/state.rs:259-266`:
```
    pub fn upgrade_to(&mut self, plan_type: &str, now: DateTime) {
        self.plan_type = plan_type.to_string();
        self.plan_status = PLAN_STATUS_ACTIVE.to_string();
        if plan_type == PLAN_TYPE_PAID {
            self.expiration_date = None;
        }
```
`src/state.rs:247-249`:
```
    pub fn free_window_open(&self) -> bool {
        self.plan_type == PLAN_TYPE_FREE && self.plan_status != PLAN_STATUS_EXPIRED
    }
```

## Steps to Reproduce

1. Boot the emulator with this service mounted exactly as a client sees it — i.e. `tests/all/free_tier.rs::harness()` WITHOUT the extra `vet_payment_instrument` write that `vetted_router()` (tests/all/free_tier.rs:64-71) performs.
2. POST / with `X-Amz-Target: AWSFreeTierService.UpgradeAccountPlan` and body `{"accountPlanType": "PAID"}`. Observe HTTP 400 `ValidationException` with message `Account plan upgrade failed PI vet failure` (src/ops/upgrade_account_plan.rs:212-216).
3. Repeat the call any number of times, in any order, interleaved with `UpgradeAccountPlan {"accountPlanType": "FREE"}`, `GetAccountPlanState`, `ListAccountActivities`, `GetAccountActivity` and `GetFreeTierUsage` — every one of the service's five operations. The refusal never changes: no request writes `vetted_payment_instruments`.
4. Confirm no code path can: grep the repository for `vet_payment_instrument` and observe the only non-test occurrences are its definition and field in `src/state.rs` (lines 404, 426, 473-475). Every caller is in `tests/all/`.
5. Call `GetAccountPlanState` after any sequence of wire requests and observe `accountPlanType` is always `"FREE"` and `accountPlanExpirationDate` is always present — the PAID branch of `AccountPlan::upgrade_to` (src/state.rs:262-264) is unreachable, although `GetAccountPlanStateResponse$accountPlanType` is @required over an enum declaring `FREE | PAID`.
6. Compare with the claim at tests/all/paid_plan_vetting.rs:158-161 ("not a paid plan the emulator cannot reach") and with src/state.rs:22-26 ("It starts EMPTY, like `plans`"): `plans` is filled by a wire-reachable operation, this set by nothing.
