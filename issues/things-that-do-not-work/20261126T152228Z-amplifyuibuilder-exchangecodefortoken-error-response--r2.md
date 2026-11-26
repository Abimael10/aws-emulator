Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 4c0072743c4b802ee07d254916613d27e39cf370

# The declared-statuses reader's own doc names two of its three reader suites, omitting the ExchangeCodeForToken one

_Filed: 2026-11-26T15:22:28Z_
_Operation: ExchangeCodeForToken | Class: error-response_

> _Note: this issue was identified against the code base at commit `4c0072743c4b802ee07d254916613d27e39cf370`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`tests/all/modelled_refusal_statuses.rs` is the crate's single shared reader of the "a refusal carries a status the refusing operation's own model declares" rule: `declared_statuses(operation)` reads the shipped Smithy model so no suite can type a status in by hand.

Its module doc enumerates the suites that check that rule and the suites that take the declared set from it, and it names exactly two: `refresh_token_declared_statuses` and `emulator_store_bound_refusals`. The crate actually holds THREE callers of `declared_statuses`: those two plus `exchange_code_for_token_declared_statuses`, a byte-for-byte parallel of the refresh suite that states the same rule in its own doc and resolves its expectation through the same reader.

So the reader's own doc is false about its own crate in two ways: "Two suites check that rule" (three do) and "both take the declared set from here" (three do). The claim is the one place an editor of `declared_statuses` looks to see who depends on it — the enumeration exists precisely so a change to the reader can be weighed against its readers — and it under-reports them, so a change made on the strength of it would silently affect the ExchangeCodeForToken suite the doc says does not exist. This is the same class of self-contradiction already fixed in this crate for the page-cursor counter, whose doc named only two of the four record kinds that draw from it.

The code is the correct side on both counts: the exchange suite exists, drives every refusal `ExchangeCodeForToken` can answer, and correctly resolves its statuses through the shared reader. The defect is the stale enumeration in the reader's doc. Fixing it changes no response and no assertion.

This is distinct from the FIXED issue `20261104T045802Z-amplifyuibuilder-exchangecodefortoken-error-response` (that one was about the handler answering an unmodelled 402 for a store bound; the handler answers the modelled 400 today and that is not in question here).

## Evidence

The claim, in the shared reader that owns the rule:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/modelled_refusal_statuses.rs:6-12
    //! A refusal of a request carries an error shape the refusing operation's own model declares
    //! for it, so the status a client classifies the answer by is one of that operation's
    //! declared statuses. Two suites check that rule — [`super::refresh_token_declared_statuses`]
    //! on `RefreshToken`, and [`super::emulator_store_bound_refusals`] on every store this
    //! emulator bounds on its own account — and both take the declared set from here rather than
    //! from a list typed into a test, so neither can expect a status the shipped model does not
    //! give the operation.

The third suite the enumeration omits — it states the same rule and reads the same function:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/exchange_code_for_token_declared_statuses.rs:6-8
    //! The RULE: a refusal of a request carries an error shape the refusing operation's own
    //! model declares for it, so the status is one of that operation's declared statuses —
    //! including a refusal of a bound this EMULATOR holds one of its own stores under, ...
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/exchange_code_for_token_declared_statuses.rs:16-18
    //! ... The expectation every row
    //! is checked against is read off the shipped model by
    //! [`super::modelled_refusal_statuses::declared_statuses`], never typed in here.
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/exchange_code_for_token_declared_statuses.rs:41
    use crate::modelled_refusal_statuses::{declared_statuses, modelled_status};
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/exchange_code_for_token_declared_statuses.rs:177-178
    fn declared_for_an_exchange() -> BTreeSet<u16> {
        let declared = declared_statuses("ExchangeCodeForToken");

The two readers the doc does name, for comparison — the same call shape:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/refresh_token_declared_statuses.rs:183-184
    fn declared_for_a_refresh() -> BTreeSet<u16> {
        let declared = declared_statuses("RefreshToken");
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/emulator_store_bound_refusals.rs:415
            let declared = declared_statuses(site.operation);

The exchange suite is not a stray file: the exchange handler really does answer the refusals it drives, through the shared bound helper the suite's rows are about —

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/exchange_code_for_token.rs:252-260
                return Err(
                    ExchangeCodeForTokenErrorModeled::InvalidParameterException(
                        crate::state::store_bound_reached(
                            "live token sessions",
                            crate::state::PROVIDER_TOKEN_SESSION_QUOTA,
                        ),
                    )
                    .into(),
                );

and the exchange suite itself already knows the reader has more than two readers, naming both of the others:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/exchange_code_for_token_declared_statuses.rs:24-26
    //! [`super::refresh_token_declared_statuses`] reads the same model for the same rule on the
    //! refresh side of this store, and [`super::emulator_store_bound_refusals`] on every other
    //! bounded store of this crate.

Test coverage: all three suites pass today; nothing asserts over the reader's enumeration, which is why the omission is invisible to the suite run.

## Steps to Reproduce

1. Read tests/all/modelled_refusal_statuses.rs lines 6-12: "Two suites check that rule — refresh_token_declared_statuses ... and emulator_store_bound_refusals ... — and both take the declared set from here".
2. grep the test tree for `declared_statuses`: it is called from tests/all/refresh_token_declared_statuses.rs:184, tests/all/emulator_store_bound_refusals.rs:415 AND tests/all/exchange_code_for_token_declared_statuses.rs:178 — three suites, not two.
3. Read tests/all/exchange_code_for_token_declared_statuses.rs lines 6-18: it states the identical rule and says its expectation is "read off the shipped model by super::modelled_refusal_statuses::declared_statuses, never typed in here", so it is a reader of the same rule through the same function.
4. Read tests/all/exchange_code_for_token_declared_statuses.rs lines 24-26: that suite's own doc names the other two suites, so the crate records the three-way relationship everywhere except in the reader the three share.
