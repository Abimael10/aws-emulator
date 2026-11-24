Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 455b877557af92974b1e1b0549e81b06a05b022f

# State the instance list's accepted Filter$Name set once instead of twice in the same file

_Filed: 2026-11-24T22:21:53Z_
_Operation: ListLinuxSubscriptionInstances | Class: maintainability_

> _Note: this issue was identified against the code base at commit `455b877557af92974b1e1b0549e81b06a05b022f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The set of `Filter$Name` values `ListLinuxSubscriptionInstances` accepts is written down TWICE in one file, and the two copies are what the operation's two halves read: `FILTER_NAMES` is the list the request is VALIDATED against (`crate::state::validate_filters`), and the `match name` arms of `filtered_member` are the list the selection is APPLIED from. Nothing ties them together, so they can drift: a name added to `FILTER_NAMES` without a matching arm passes validation and then resolves through the catch-all `_ => Vec::new()`, which `crate::state::filter_selects` treats as "the record holds no value for this member" and therefore selects nothing — exactly the silently-empty page the state layer documents `validate_filters` as existing to prevent. Conversely an arm added without the name in `FILTER_NAMES` is dead code, because the filter is refused before it is reached.

The rule "which `Filter$Name` values this list addresses, and which member each addresses" should be stated once — e.g. by having `filtered_member` answer `Option<Vec<String>>` (`None` meaning "no such name") and deriving the accepted-name set from that one table — so a name cannot be admitted by validation without a projection existing for it. Every response and every existing test stays byte-for-byte the same: the nine names and the nine projections are unchanged, only their single source is.

## Evidence

Copy 1 — the validated set, `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/ops/list_linux_subscription_instances.rs:224`:
```
const FILTER_NAMES: &[&str] = &[
    "AccountID",
    "AmiID",
    "DualSubscription",
    "InstanceID",
    "InstanceType",
    "ProductCode",
    "Region",
    "Status",
    "UsageOperation",
];
```
used at `list_linux_subscription_instances.rs:173`: `crate::state::validate_filters(&filters, FILTER_NAMES).map_err(into_error)?;`

Copy 2 — the applied set, same file `list_linux_subscription_instances.rs:242`:
```
fn filtered_member(record: &crate::state::DiscoveredInstanceRecord, name: &str) -> Vec<String> {
    match name {
        "AccountID" => vec![record.account_id.clone()],
        "AmiID" => vec![record.ami_id.clone()],
        ...
        "UsageOperation" => vec![record.usage_operation.clone()],
        _ => Vec::new(),
    }
}
```
whose own doc at `list_linux_subscription_instances.rs:241` admits the coupling it cannot enforce: "Only a name in [`FILTER_NAMES`] reaches here, so an unknown one holds nothing."

The rule the drift would break, `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/state.rs:1292`: "`valid_names` is the set of `Filter$Name` values that list documents: a filter naming anything else addresses no member, so it can select nothing and is a `ValidationException` rather than a silently empty page." An unmapped-but-validated name is precisely a silently empty page, because `filter_selects` answers `false` for an empty `member_values` (`state.rs:1336`: `if member_values.is_empty() { return false; }`).

Test coverage that must stay unchanged: `tests/all/linux_subscriptions.rs:463` `an_instance_filter_selects_on_the_member_it_names` drives all nine names and the member each addresses (its own table `FILTERED_MEMBERS` at `tests/all/linux_subscriptions.rs:538`), and `tests/all/linux_subscriptions.rs:828` `a_filter_a_list_cannot_apply_is_refused` pins that a name this list does not document is refused with HTTP 400 `ValidationException`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/ops/list_linux_subscription_instances.rs` and read `FILTER_NAMES` (line 224) and `filtered_member` (line 242): the same nine `Filter$Name` values are enumerated in both.
2. Add a tenth name to `FILTER_NAMES` only (simulating the drift) and send `POST /subscription/ListLinuxSubscriptionInstances` with `{"Filters":[{"Name":"<new name>","Values":["x"]}]}`.
3. Observe the request is accepted by `validate_filters` and then selects nothing via the `_ => Vec::new()` arm plus `filter_selects`' empty-values short-circuit — a 200 with an empty `Instances` list, the silently empty page `state.rs:1292` says must be a `ValidationException` instead.
4. Confirm the reverse drift too: add a `match` arm without the name in `FILTER_NAMES` and the arm is unreachable, since `validate_filters` refuses the name first.
