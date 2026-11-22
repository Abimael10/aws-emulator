Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: cf76471bd03827ca67259df87b7d3e7f2799f06e

# Refusal sentences splice an unvalidated value in raw, so a submitted line break leaves the message outside the ^(.)+$ its own shape declares

_Filed: 2026-11-22T02:18:24Z_
_Operation: BatchDescribeEntities | Class: error-response_

> _Note: this issue was identified against the code base at commit `cf76471bd03827ca67259df87b7d3e7f2799f06e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every "this value is invalid" sentence this crate builds goes through `crate::state::quoting`, which splices the SUBMITTED value into the sentence verbatim and bounds only its LENGTH. The members that carry those sentences — `BatchDescribeErrorDetail$ErrorMessage` (`BatchDescribeErrorMessageContent`) and `ValidationException$Message` (`ExceptionMessageContent`) — both declare `@pattern("^(.)+$")`, which admits no line terminator. The members whose values reach `quoting` are exactly the ones taken off the boundary walk with `skip_validation`, so a value carrying a line break reaches the handler by design and is quoted back raw: `BatchDescribeEntities` answers 200 with an `Errors` entry whose `ErrorMessage` contains a literal newline (and whose map key, targeting `EntityId`, does too), and the single-read path answers a `ValidationException` whose `Message` does.

This contradicts the same crate twice over: (1) it implements that very pattern as `DetailSyntax::OneLine`, backed by `is_line_break`, and refuses a STORED value carrying a line break for it; (2) the sibling quoting helper `quoted`, documented in `quoting`'s own doc comment as following "the same rule", renders its value through `Value::to_string()` — JSON, which escapes a line break — so the `ErrorDetail$ErrorMessage` path is safe where the `quoting` path is not. The fix is confined to the one helper: `quoting` already owns the only place a quoted value is shortened, and it is where the single-line requirement of the member it is written for belongs.

## Evidence

Model (the member that carries the sentence):
- `smithy_query(query="shape", shape="BatchDescribeErrorMessageContent")` -> `{"type": "string", "length": {"min": 1, "max": 2048}, "pattern": "^(.)+$"}`
- `smithy_query(query="shape", shape="ExceptionMessageContent")` -> `{"type": "string", "length": {"min": 1, "max": 2048}, "pattern": "^(.)+$"}`

The crate knows that pattern forbids a line terminator — state.rs:2914-2918:
```
/// Whether `c` is a line terminator — a character `.` does not match, which is
/// what makes `^(.)+$` a single-line value.
fn is_line_break(c: char) -> bool {
```
and state.rs:2886-2888 enforces it:
```
Self::OneLine => !value.is_empty() && !value.chars().any(is_line_break),
```
(pinned by tests/all/summary_member_contracts.rs:433-438, case `"a product title carrying a line break"` with `json!({"ProductTitle": "two\nlines"})`).

The other side — state.rs:4020-4036, which bounds the LENGTH only and splices the value raw:
```
/// A message built around a submitted value: `prefix`, the value quoted, then
/// `suffix` — with the quote cut to the room those two leave inside
/// [`MESSAGE_BUDGET`] ...
/// The same rule [`quoted`] follows for a submitted details value ...
fn quoting(prefix: &str, value: &str, suffix: &str) -> String {
    let room = MESSAGE_BUDGET.saturating_sub(...);
    format!("{prefix}{}{suffix}", cut_to(value, room, CUT_MARK))
}
```
versus the sibling it names, state.rs:3365-3367, which escapes a line break because it renders JSON:
```
fn quoted(value: &Value) -> String {
    cut_to(&value.to_string(), DETAIL_VALUE_QUOTE_LIMIT, CUT_MARK)
}
```

The value reaches the handler unvalidated on purpose — src/ops/types/batch_describe_entities_types.rs:20-27:
```
// `skip_validation` names `Catalog` ... and `EntityId`: a batch
// read reports a malformed entry PER ENTRY in `Errors`, so an `EntityId` outside the
// published form must reach the handler rather than failing the whole call at the
// boundary.
```
and is quoted back — src/ops/batch_describe_entities.rs:193-205:
```
if !crate::state::is_entity_id(&request.entity_id) {
    errors.insert(
        request.entity_id.clone(),
        BatchDescribeErrorDetail {
            error_code: Some(crate::state::BATCH_ERROR_VALIDATION.to_string()),
            // The sentence quotes the submitted id cut to the room
            // it leaves, so an entry of any length is reported
            // inside `BatchDescribeErrorMessageContent`'s 2048.
            error_message: Some(crate::state::invalid_entity_id_message(
                &request.entity_id,
            )),
```
with state.rs:4683-4689 `invalid_entity_id_message` = `quoting("Requested entity id '", entity_id, …)`.

Test coverage: tests/all/entity_reads.rs / request_validation.rs exercise malformed ids, and summary_member_contracts.rs:433 pins the line-break rule for stored values; no test covers a submitted value that itself carries a line terminator, which is why the gap survives. The same `quoting` helper is reached from `invalid_resource_arn` (TagResource/UntagResource/Get/Put/DeleteResourcePolicy `ResourceArn`, all `skip_validation`), `invalid_change_set_name` and `invalid_entity_id` (DescribeEntity `entityId`), so one fix in the helper covers them all.

## Steps to Reproduce

1. POST /BatchDescribeEntities with body {"EntityRequestList":[{"Catalog":"AWSMarketplace","EntityId":"a\nb"}]} (a real newline, written as the JSON escape \n).
2. `EntityRequest` carries skip_validation on EntityId (src/ops/types/batch_describe_entities_types.rs:26), so the value reaches the handler; `is_entity_id("a\nb")` is false (state.rs:4650-4662 admits no line break), so the per-entry error branch runs.
3. Observe the 200 body: the `Errors` entry's `ErrorMessage` is `Requested entity id 'a<newline>b' is invalid. It should match with ^[a-zA-Z0-9][.a-zA-Z0-9/-]+[a-zA-Z0-9]$.` — a value with an embedded line terminator, which `BatchDescribeErrorMessageContent`'s `@pattern("^(.)+$")` excludes.
4. Same path on a single read: GET /DescribeEntity?catalog=AWSMarketplace&entityId=a%0Ab answers 422 ValidationException whose `Message` carries the same raw newline, against `ExceptionMessageContent`'s identical pattern.
5. Contrast the `ErrorDetail` path, which escapes it: StartChangeSet a change with DetailsDocument {"ProductTitle":"two\nlines"} (tests/all/summary_member_contracts.rs:433) and read `DescribeChangeSet` — the message quotes the value JSON-rendered via `quoted`, so the line break appears escaped, not raw.
