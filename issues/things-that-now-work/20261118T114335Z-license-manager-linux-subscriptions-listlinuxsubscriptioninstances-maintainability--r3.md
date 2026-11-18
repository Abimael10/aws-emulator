Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: fdc3a9e54bef9688863d6a1e9fd32b2d4bc7efcf

# State the "absent Filter$Operator means Equal" default once instead of in both filter paths

_Filed: 2026-11-18T11:43:35Z_
_Operation: ListLinuxSubscriptionInstances | Class: maintainability_

> _Note: this issue was identified against the code base at commit `fdc3a9e54bef9688863d6a1e9fd32b2d4bc7efcf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "a `Filter` carrying no `Operator` means the `Equal` comparison" is implemented twice in this crate's own state module: once where a filter is rendered into the pagination-cursor identity (`canonical_filter`) and once where a filter is APPLIED to a resource (`filter_selects`). The two copies must agree — the cursor identity is only correct because the rendering means what the selection does — and `canonical_filter`'s own doc says so, yet nothing makes them agree structurally: each path spells its own `unwrap_or(OPERATOR_EQUAL)`, so a change to one default (or a future second defaulted member of `Filter`) can be applied to one copy and missed in the other, which would mint a cursor for a list other than the one the page was cut from. Extracting the effective operator of a `Filter` into one accessor used by both paths states the rule once; no response and no existing test changes.

## Evidence

Copy 1 — the cursor-identity rendering, services/.../src/state.rs:1199-1211:
```
fn canonical_filter(filter: &Filter) -> String {
    length_prefixed(
        [
            filter.name.clone().unwrap_or_default(),
            filter
                .operator
                .clone()
                .unwrap_or_else(|| OPERATOR_EQUAL.to_string()),
```
Copy 2 — the selection, services/.../src/state.rs:1294:
```
    match filter.operator.as_deref().unwrap_or(OPERATOR_EQUAL) {
```
The crate itself states that the two must mean the same thing, services/.../src/state.rs:1181-1184:
```
/// Each filter is rendered as the selection it MEANS rather than as the request spelled
/// it, the way [`filter_selects`] applies it: an absent `Operator` renders as the equality
/// it stands for ([`OPERATOR_EQUAL`]), and the `Values` render as the set they are
/// compared as
```
Existing coverage that pins both halves and must stay green unchanged:
- services/.../tests/all/page_cursors_are_only_what_the_listing_minted.rs:410-415 — `"an absent Operator and the Equal it means"` with `json!({ "Filters": [{ "Name": "Status", "Values": ["running"] }] })` vs `json!({ ..., "Operator": "Equal" }])` (same row repeated for `ListLinuxSubscriptions` at :455-460), which is exactly the rendering copy.
- services/.../tests/all/linux_subscriptions.rs:465 (`("Status", "NotEqual", &["running"], …)`) and the operator rows in services/.../tests/all/shared_discovery_report.rs:476-518, which cover the selection copy.

## Steps to Reproduce

1. Open services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/state.rs and read `canonical_filter` (lines 1199-1211) and `filter_selects` (lines 1288-1302).
2. Observe that each independently defaults an absent `Filter$Operator` to `OPERATOR_EQUAL`, while `canonical_filter`'s doc (lines 1181-1184) requires the rendering to mean what `filter_selects` applies.
3. Change the default in one of the two places only (e.g. make `canonical_filter` render an absent operator as the empty string) and run `cargo test -p service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions`: the drift shows up as cursor identities that no longer match, proving the two copies are the same rule maintained twice.
