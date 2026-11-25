Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: ebbcebe5e22b62a6a0ce7f26d78d7af6e6d536ee

# The deserialization refusal names the alphabetically-first unreadable member, not "the first value of the body" its own doc promises

_Filed: 2026-11-25T17:56:49Z_
_Operation: BatchPutMetrics | Class: error-response_

> _Note: this issue was identified against the code base at commit `ebbcebe5e22b62a6a0ce7f26d78d7af6e6d536ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The envelope's deserialization refusal is worded from the FIRST member of the body whose JSON type its shape cannot be read from. Both helpers that implement that selection state, in their own doc comments, that the walk follows the order the request carries the members in — `unreadable_member` is documented as "The first value of the body the member it is bound to cannot be read from" and `declared_members` as yielding the body's values "in the order the request carries them".

The code under those sentences cannot honour them: `declared_members` iterates `serde_json::Map::iter`, and this crate asks for no `serde_json` feature at all (`serde_json = { workspace = true }`, the workspace entry being plain `serde_json = "1"`). Without the `preserve_order` feature a `serde_json::Map` is re-sorted by key rather than keeping the document's order — the fact a sibling crate of this same workspace states when it opts into that feature. So for a request carrying TWO values their members cannot be read from, the refusal names whichever member sorts first alphabetically, not the one the request carried first:

* `PUT /BatchPutMetrics` with `{"TrialComponentName": 7, "MetricData": true}` is refused `"BOOLEAN_VALUE is not an expected Json type"` (key `MetricData` < `TrialComponentName`), where the documented order yields `"NUMBER_VALUE is not an expected Json type"`;
* the same inside a batch item: `{"MetricData": [{"Value": "x", "Timestamp": true}]}` reports the `Timestamp` class, not the `Value` class the item carried first.

Worse, which of the two sentences a caller gets is not a property of this crate at all: `serde_json`'s features unify across one build, and another crate of this workspace (cloudwatchlogs) requests `preserve_order`. Built alone (the way this crate's own tests run) the walk is sorted; built together with that crate (the way the router binary is built) it is insertion-ordered and the doc becomes true. One request, two possible refusal sentences, decided by the build graph.

The defect is in the selection of WHICH offending value is named; the status (400), the error code and the capital-`Message` member are correct and must not change. It is distinct from the already-fixed BatchPutMetrics error-response gap about a non-epoch `Timestamp` string (that one was about whether a value reads at all, not about which of several non-reading values is reported).

## Evidence

The order the code promises — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/service/envelope.rs:468-469`:
```
/// The first value of the body the member it is bound to cannot be read from, as the refusal
/// it earns.
```
and `src/service/envelope.rs:494-495`:
```
/// The values of `submitted` that `shape` declares a member for, each with that member's
/// own schema, in the order the request carries them.
```
The code under them — `src/service/envelope.rs:496-506`:
```
fn declared_members<'a>(
    shape: &'a Schema,
    submitted: &'a serde_json::Map<String, Value>,
) -> impl Iterator<Item = (&'a Schema, &'a Value)> {
    submitted.iter().filter_map(move |(key, held)| {
```
and the two `find_map`s that take the FIRST element of that iterator — `src/service/envelope.rs:474-491`:
```
fn unreadable_member(input: &Schema, members: &serde_json::Map<String, Value>) -> Option<Refusal> {
    declared_members(input, members).find_map(|(member, held)| {
        if !reads_into(member, held) {
            return Some(Refusal::Unreadable(unexpected_json_type(held)));
        }
...
            Value::Object(fields) => declared_members(item, fields).find_map(|(field, held)| {
                (!reads_into(field, held)).then(|| Refusal::Unreadable(unexpected_json_type(held)))
            }),
```
What this crate asks of `serde_json` — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/Cargo.toml:27`:
```
serde_json = { workspace = true }
```
and the workspace entry it resolves to — `Cargo.toml:936`:
```
serde_json = "1"
```
(no `preserve_order`). What that feature means, stated in this workspace — `services/service-com-amazonaws-cloudwatchlogs-logs-20140328/Cargo.toml:25-28`:
```
# `preserve_order`: a record an HTTP ingestion endpoint takes in is stored as the document
# it arrived as (`src/state/http_ingestion.rs`), so the member order a client wrote it in is
# part of what a reader gets back rather than re-sorted by the reader of the body.
serde_json = { workspace = true, features = ["preserve_order"] }
```
— i.e. without it the reader of the body re-sorts the members, which is exactly the reader `declared_members` is.

Test coverage leaves this unpinned in both directions: every row of `src/service/envelope.rs:710-798` (`the_refusal_is_about_a_member_the_input_declares`) and every case of `tests/all/envelope_and_per_item_reporting.rs:303-415` (`a_member_of_the_wrong_json_type_is_refused_as_a_deserialization_failure`) carries exactly ONE unreadable value, e.g. `src/service/envelope.rs:729`:
```
                &object(json!({"MetricData": [{"MetricName": "loss", "Value": "not-a-number"}]}))
```
so no test can tell the two orders apart, and a fix changes no existing assertion.

## Steps to Reproduce

1. Read src/service/envelope.rs:494-495: `declared_members` is documented as yielding the body's values "in the order the request carries them", and src/service/envelope.rs:468-469 words the refusal from "The first value of the body".
2. Read src/service/envelope.rs:500: the iteration is `submitted.iter()` over a `serde_json::Map`, and Cargo.toml:27 + root Cargo.toml:936 show this crate requests no `preserve_order` feature for serde_json (the feature a sibling crate documents at services/service-com-amazonaws-cloudwatchlogs-logs-20140328/Cargo.toml:25-28 as what stops the body's members being "re-sorted by the reader of the body").
3. Send PUT /BatchPutMetrics with the raw body text {"TrialComponentName": 7, "MetricData": true} (two members whose shapes cannot be read from those JSON types).
4. Observed in a build without preserve_order: 400 with Message = "BOOLEAN_VALUE is not an expected Json type" — the alphabetically first key, `MetricData`. Expected from the quoted docs: the first value the request carried, i.e. "NUMBER_VALUE is not an expected Json type".
5. Repeat nested in an item: PUT /BatchPutMetrics with {"TrialComponentName": "tc-1", "MetricData": [{"Value": "x", "Timestamp": true}]} — the sentence names the `Timestamp` class, not the `Value` class carried first.
6. Note the answer is build-dependent: serde_json's features unify across one build, and services/service-com-amazonaws-cloudwatchlogs-logs-20140328/Cargo.toml:28 enables preserve_order, so the same request answers the other sentence when the two crates are built together (the router binary) versus this crate alone (its own test run).
