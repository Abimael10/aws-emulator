Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 990310f3c08624998ca7ab018ed844542a47dd84

# ListWebFunctions walks the whole store instead of the Region view the crate's other reader uses, so it lists functions of every Region

_Filed: 2026-11-25T16:12:56Z_
_Operation: ListWebFunctions | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `990310f3c08624998ca7ab018ed844542a47dd84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The state module holds every Region's functions in one map and provides exactly one view for readers that report on a single Region (`functions_in_region`), documenting that "a reader that reports on ONE Region takes this view of it rather than the whole map" and that "the Regions' views partition the store between them". `GetWebAccountSettings` takes that view for `accountUsage.functionCount`; `ListWebFunctions` does not — it never even looks at the request's signing Region (`let _ = ctx;`) and iterates `state.functions.values()`. The two readers of the same store therefore disagree: a call signed for us-west-2 is told `functionCount: 0` while the listing signed for us-west-2 returns the functions whose own stored ARNs name us-east-1 (and whose Region the crate treats as fixed at creation, `WebFunction::home_region`).

## Evidence

The view and its stated rule — services/service-com-amazonaws-lambdaweb-lambdaweb/src/state.rs:507-526:
```
    /// The stored web functions that belong to `region` — the ones whose own ARN
    /// names it ([`WebFunction::home_region`]).
    ///
    /// The store holds every Region's functions together, so a reader that reports
    /// on ONE Region takes this view of it rather than the whole map.
...
    /// made in — and the Regions' views partition the store between them.
    pub fn functions_in_region<'view>(
```
The sibling reader that takes it — src/state/account.rs:83-87: `function_count: as_count_member(self.functions_in_region(region).count())`, reached from src/ops/get_web_account_settings.rs:150 `let region = crate::state::request_region(ctx.parts());` and :169 `account_usage: state.account_usage(&region)`.

The listing that skips it — src/ops/list_web_functions.rs:157 `let _ = ctx;` and :185-193:
```
        let matching: Vec<&crate::state::WebFunction> = state
            .functions
            .values()
            .filter(|function| {
                wanted_states
                    .iter()
                    .all(|states| states.contains(&function.state))
            })
            .collect();
```
(`functions_in_region` has exactly one caller in the crate: grep shows state/account.rs:85.)

Test coverage: tests/all/web_account_settings.rs:500-561 pins the per-Region partition for the count ("Every stored function is counted in exactly ONE Region, so the Regions' counts add up to the whole stored set"), and no test in the crate asserts anything about which Region's functions `ListWebFunctions` returns — the listing tests (tests/all/web_function_lifecycle.rs:810 `pagination_reports_every_function_once_in_name_order`, :1079 `the_state_filter_narrows_the_listing`) create every function in one Region.

## Steps to Reproduce

1. PUT /2025-03-07/web-functions signed for us-east-1 with {"functionName":"east-one"} — 202, functionArn arn:aws:lambda:us-east-1:<account>:web-function/east-one.
2. GET /2025-03-07/web-account-settings signed for us-west-2 — 200 with accountUsage.functionCount 0 (the Region view is applied).
3. POST /2025-03-07/web-functions signed for us-west-2 with {} — the listing returns east-one, whose stored ARN names us-east-1, contradicting the count the same credential scope just read.
