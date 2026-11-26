Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: d3a78243a1d5dfc49cb414d66afb769a981c7ac0

# A refused Filters[].Name withholds the offending value, though the model marks no filter name @sensitive

_Filed: 2026-11-26T01:40:46Z_
_Operation: ListEventTypes | Class: error-response_

> _Note: this issue was identified against the code base at commit `d3a78243a1d5dfc49cb414d66afb769a981c7ac0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service/validation.rs` decides whether to echo the offending value of a constraint violation by looking at the LAST segment of the violation path and comparing it against the literal list `SENSITIVE_MEMBERS = ["name", "targetAddress"]`. Three members of this model are called `Name` without being `@sensitive`: `ListEventTypesFilter$Name`, `ListNotificationRulesFilter$Name` and `ListTargetsFilter$Name`. A violation on one of them is reported at the path `filters.<n>.member.name`, whose last segment is `name`, so the formatter takes it for the `@sensitive` `NotificationRuleName` and strips the value out of the clause.

The result contradicts the module's own stated dialect: a caller who sends a filter name the enum does not list gets `Value at 'filters.1.member.name' failed to satisfy constraint: Member must satisfy enum value set: [...]` instead of the clause this module documents for every non-sensitive member, which quotes the submitted value back (`Value '<value>' at '<path>' ...`). The module's own doc also asserts that `NotificationRuleName` and `TargetAddress` "are the only sensitive shapes in this model", which is exactly why the filter names must not be treated as sensitive.

The path-based rule gains nothing for the genuinely sensitive members either: the framework's constraint walk already marks a `@sensitive` member's violation and drops its value (`redact_since`), and `Violation::message()` already renders such a violation without the value. So the only observable effect of this service's last-segment rule is the misfire on the three filter-name members.

## Evidence

The rule, in this service's own formatter — `src/service/validation.rs:60-68`:
```
const SENSITIVE_MEMBERS: &[&str] = &["name", "targetAddress"];
...
fn names_a_sensitive_member(path: &str) -> bool {
    SENSITIVE_MEMBERS.contains(&path.split('.').next_back().unwrap_or(path))
}
```
used at `src/service/validation.rs:110-111`:
```
let withhold = (violation.value.is_some() || violation.sensitive)
    && names_a_sensitive_member(&violation.path);
```

The dialect the same file states it implements — `src/service/validation.rs:22-24`:
```
//! * any other member's value is quoted back as the framework quotes it
//!   (`Value '<value>' at '<path>' …`), which is the only place the server reports the
//!   offending input to the caller;
```
and `src/service/validation.rs:53-56`:
```
/// `smithy_query(query="shape",shape="NotificationRuleName,TargetAddress,Target")`: the
/// `NotificationRuleName` and `TargetAddress` string shapes both carry `sensitive: true`
/// and are the only sensitive shapes in this model.
```

The model: `smithy_query(query="shape",shape="ListTargetsFilterName,ListEventTypesFilterName")` returns `{"ListTargetsFilterName": {"type": "enum", "values": ["TARGET_TYPE", "TARGET_ADDRESS", "TARGET_STATUS"]}, "ListEventTypesFilterName": {"type": "enum", "values": ["RESOURCE_TYPE", "SERVICE_NAME"]}}` — no `sensitive` trait, and the member that targets them is named `Name` (`ListTargetsFilter`: `"Name": {"target": "ListTargetsFilterName", "required": true, "type": "enum"}`).

That the violation reaches the formatter WITH a value and NOT marked sensitive — `vendor/smithy-schema-server/src/constraints.rs:535-540`:
```
        Some(Violation {
            path: path.to_string(),
            kind: ViolationKind::EnumValue(self.values.iter().map(|v| v.to_string()).collect()),
            value: Some(echoed(value).into_owned()),
            sensitive: false,
        })
```
and that the path's last segment is `name` — `vendor/smithy-schema-server/src/constraints.rs:82-89`:
```
    pub fn member(smithy_name: &str) -> String { super::to_lower_camel(smithy_name) }
    pub fn element(position: usize) -> String { format!("{position}.{COLLECTION_STEP}") }
```
(`COLLECTION_STEP = "member"`, so a filter element's member path is `filters.<n>.member.name`).

That the framework already handles the genuinely sensitive members, so nothing is protected here — `vendor/smithy-schema-server/src/constraints.rs:864-871`:
```
    /// Hides the value of every violation recorded since `from` when the member is `@sensitive`.
    fn redact_since(&mut self, from: usize, sensitive: bool) {
        if !sensitive { return; }
        ...
            violation.sensitive = true;
```

Test coverage: the existing suite asserts only the error CODE for this input, never the message, so nothing pins the current wording — `tests/all/list_targets.rs:436-447`:
```
        (
            "a filter name the enum does not list",
            json!({ "Filters": [{ "Name": "TARGET_NAME", "Value": "SNS" }] }),
            400,
            "ValidationException",
        ),
```
The unit tests in `src/service/validation.rs:250-307` cover only `name`, `targetAddress`, `target.targetAddress`, `resource` and the invented `ruleName` — no row uses a real non-sensitive member actually spelled `name`.

## Steps to Reproduce

1. POST /listTargets (or /listEventTypes, /listNotificationRules) with body {"Filters": [{"Name": "TARGET_NAME", "Value": "SNS"}]} — a filter name the enum does not list, so the framework's enum constraint refuses it before the handler runs.
2. Read the 400 ValidationException message: the clause is `Value at 'filters.1.member.name' failed to satisfy constraint: Member must satisfy enum value set: [...]` — the submitted value `TARGET_NAME` is withheld.
3. Compare with a refusal on another non-sensitive member (e.g. `Resource` on /createNotificationRule), whose clause quotes the value as `Value '<value>' at 'resource' ...`, which is the rule src/service/validation.rs:22-24 states for every member the model does not mark @sensitive.
4. Confirm the member is not sensitive: smithy_query(query="shape", shape="ListTargetsFilterName") shows a plain enum with no `sensitive` trait, and src/service/validation.rs:53-56 itself states NotificationRuleName and TargetAddress are the only sensitive shapes in the model.
