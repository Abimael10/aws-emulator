Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: dd0d5f7a8a9b6c8eb2d3edf27a45a29ce399d527

# The pattern-violation clause drops the rejected value for every patterned member, on the stated premise that they are all resource ARNs — three of this service's four patterned shapes are not

_Filed: 2026-11-19T17:24:02Z_
_Operation: GetRecommendations | Class: error-response_

> _Note: this issue was identified against the code base at commit `dd0d5f7a8a9b6c8eb2d3edf27a45a29ce399d527`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service.rs` rewords every boundary `@pattern` violation into a clause that omits the rejected value, and the comment directly above the code states the reason: "every patterned member of this service is a resource ARN, which the service does not quote back". That premise is refuted by this service's own Smithy model: four shapes in the model carry `@pattern`, and only ONE of them (`Arn`) is a resource ARN. The other three are `Name` (`Promotion$name`, which `GetRecommendations` binds, and `PredictedItem$promotionName`), `FilterAttributeName` (the KEY shape of `FilterValues`, bound by all three operations and by a promotion) and `AttributeName` (the KEY shape of `Context`, bound by `GetRecommendations` and `GetPersonalizedRanking`).

So a request whose `promotions[0].name`, whose `filterValues` key, or whose `context` key violates its modelled pattern is refused with a clause that silently discards the value the caller got wrong — while every OTHER violation class on the very same non-ARN members keeps the framework's clause "value and all" (the crate's own words). A single request can therefore come back with two clauses in one envelope, one quoting the offending value and one hiding it, for two members neither of which is an ARN: the value-suppression rule is applied far wider than the only reason the crate gives for it.

The defect is the scope of the rewrite, not the ARN behaviour: the ARN members are the case the crate reasoned about and tested; the three non-ARN patterned shapes were never considered and have no test row.

## Evidence

The code and the premise it rests on — `services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/src/service.rs:85-103`:
```
/// One clause of this service's `ValidationException` message.
///
/// A violated `@pattern` is stated the way the Personalize data plane states it: the
/// member path and the expression a value has to satisfy, WITHOUT the `^`/`$` anchors
/// the model writes to make the match total, and WITHOUT echoing the rejected value —
/// every patterned member of this service is a resource ARN, which the service does not
/// quote back. Every other class keeps the framework's own clause, which already reads
/// the way AWS words it.
fn clause(violation: &Violation) -> String {
    match &violation.kind {
        ViolationKind::Pattern(pattern) => format!(
            "Value at '{}' failed to satisfy constraint: Member must satisfy regular expression \
             pattern: {}",
            violation.path,
            unanchored(pattern)
        ),
        _ => violation.message(),
    }
}
```
The same claim repeated at `src/service.rs:88-92` is contradicted by this service's model — `smithy_query(query="shape", shape="Name,FilterAttributeName,AttributeName,Arn")`:
```
"Name": {"type": "string", "length": {"min": 1, "max": 63}, "pattern": "^[a-zA-Z0-9][a-zA-Z0-9\\-_]*$"}
"FilterAttributeName": {"type": "string", "length": {"min": 0, "max": 50}, "pattern": "^[A-Za-z0-9_]+$"}
"AttributeName": {"type": "string", "length": {"min": 0, "max": 150}, "pattern": "^[A-Za-z\\d_]+$"}
"Arn": {"type": "string", "length": {"min": 0, "max": 256}, "pattern": "^arn:([a-z\\d-]+):personalize:.*:.*:.+$"}
```
Those three non-ARN patterned shapes are reachable members of this operation — `smithy_query(query="shape", shape="Promotion")`:
```
"name": {"target": "Name", "length": {"min": 1, "max": 63}, "pattern": "^[a-zA-Z0-9][a-zA-Z0-9\\-_]*$", "doc": "<p>The name of the promotion.</p>"}
"filterValues": {"target": "FilterValues", ...}
```
and `smithy_query(query="shape", shape="Context,FilterValues")`:
```
"FilterValues": {"type": "map", "key": {"target": "FilterAttributeName", "pattern": "^[A-Za-z0-9_]+$"}, ...}
"Context": {"type": "map", "key": {"target": "AttributeName", "pattern": "^[A-Za-z\\d_]+$"}, ...}
```
The inconsistency the suppression creates, stated by the crate itself — `tests/all/refusal_wording.rs:6-10`:
```
//! 1. A value outside the modelled `@pattern` is a boundary `ValidationException`
//!    (HTTP 400) whose clause names the member path and the expression the value has to
//!    satisfy ... and with no echo of the rejected value. Only that class is reworded:
//!    a violated `@range` bound keeps the framework's clause, value and all.
```
Test coverage: the suite only ever exercises ARN members, so no row covers a non-ARN pattern — `tests/all/refusal_wording.rs:24-28` (`/// The modelled `@pattern` on `Arn`, which `campaignArn`, `recommenderArn` and /// `filterArn` all target`) and the five rows at `tests/all/refusal_wording.rs:67-97` are `campaignArn`, `recommenderArn`, `campaignArn`, `campaignArn`, `filterArn`. The contrast case that pins the value-echoing clause for another class is `tests/all/refusal_wording.rs:142-148`:
```
    assert_eq!(
        refusal.message,
        "1 validation error detected: Value '-1' at 'numResults' failed to satisfy constraint: \
         Member must have value greater than or equal to 0",
```

## Steps to Reproduce

1. POST `/recommendations` with `{"campaignArn": "arn:aws:personalize:us-east-1:000000000000:campaign/any", "userId": "user-1", "numResults": 3, "promotions": [{"name": "-bad-name", "percentPromotedItems": 50, "filterArn": "arn:aws:personalize:us-east-1:000000000000:filter/any"}]}` — `-bad-name` violates `Name`'s pattern `^[a-zA-Z0-9][a-zA-Z0-9\-_]*$`.
2. Observe the 400 `ValidationException` message: `src/service.rs:93-103` renders `Value at 'promotions[0].name' failed to satisfy constraint: Member must satisfy regular expression pattern: ...` with the rejected value `-bad-name` dropped, although `promotions[0].name` is not a resource ARN — the only reason `src/service.rs:88-92` gives for dropping it.
3. Repeat with `"context": {"bad-key": "v"}` (key shape `AttributeName`, pattern `^[A-Za-z\d_]+$`) and with `"filterValues": {"bad-key": "\"Comedy\""}` (key shape `FilterAttributeName`, pattern `^[A-Za-z0-9_]+$`): the same value-free clause is produced for two more non-ARN members.
4. Send one request mixing a bad `context` key with `"numResults": -1`: the envelope carries two clauses for two non-ARN members, one quoting the value (`Value '-1' at 'numResults' ...`, pinned by `tests/all/refusal_wording.rs:142-148`) and one hiding it.
5. Confirm no test constrains the non-ARN case: `tests/all/refusal_wording.rs:24-28` scopes the pattern suite to `campaignArn`/`recommenderArn`/`filterArn`, and its rows at `:67-97` name only those members.
