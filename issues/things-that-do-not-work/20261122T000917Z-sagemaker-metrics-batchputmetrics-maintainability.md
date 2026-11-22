Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 161f6d1fce0a488dc28b92e49964072868b13e1b

# Collapse the duplicated VALIDATION_ERROR paragraph in the BatchPutMetrics handler docs, which states a storage rule the handler breaks

_Filed: 2026-11-22T00:09:17Z_
_Operation: BatchPutMetrics | Class: maintainability_

> _Note: this issue was identified against the code base at commit `161f6d1fce0a488dc28b92e49964072868b13e1b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `BatchPutMetrics` handler doc states the same rule twice, in two copies that have already drifted from each other and from the code beneath them.

Lines 41-46 and 48-52 of `src/ops/batch_put_metrics.rs` are two versions of one paragraph (both open with the identical sentence "`VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service validates nothing about a point it can store."). The second copy asserts that a point is stored "under the name as sent — blank, long or absent — and read back under that same name", and the first copy asserts "however long, whatever it holds".

Both assertions are false for this handler:

1. a BLANK/absent `MetricName` is NOT stored: the handler reports `INTERNAL_ERROR` at that point's index and stores nothing (`batch_put_metrics.rs:234`), which is also what the paragraph at lines 29-31 of the same doc says and what the integration test pins;
2. a LONG `MetricName` is NOT stored either: `state.rs:160-162` refuses a name past `MAX_NAME_BYTES` with `RecordRejection::MetricLimitExceeded`, which the handler maps to `METRIC_LIMIT_EXCEEDED` — and the handler's own `METRIC_LIMIT_EXCEEDED` bullet (lines 36-39) does not mention the name-length refusal at all, so the doc omits one of the three codes' triggers while positively promising the opposite.

The code and both refusals are pinned by existing tests, so the defect is the duplicated/stale documentation, not the behaviour. Collapsing the two paragraphs into one that matches the handler's checks changes no response and no test.

## Evidence

Copy 1 — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/ops/batch_put_metrics.rs:41-46`:
```
//! `VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service
//! validates nothing about a point it can store. An INFINITE `Value` is a number and is
//! stored as one; a `MetricName` is otherwise an opaque series key, so a point is stored
//! under the name as sent — however long, whatever it holds — and read back under that same
//! name; ...
```
Copy 2 (same doc comment) — `src/ops/batch_put_metrics.rs:48-52`:
```
//! `VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service
//! validates nothing about a point it can store. A `MetricName` is an opaque series key,
//! so a point is stored under the name as sent — blank, long or absent — and read back
//! under that same name; ...
```
The code under them — `src/ops/batch_put_metrics.rs:234-237`:
```
            if raw.value.is_nan() || raw.metric_name.trim().is_empty() {
                errors.push(failure(CODE_INTERNAL_ERROR, index));
                continue;
            }
```
and `src/state.rs:160-162`:
```
        if trial_component_name.len() > MAX_NAME_BYTES || metric_name.len() > MAX_NAME_BYTES {
            return Err(RecordRejection::MetricLimitExceeded);
        }
```
whose own doc (`src/state.rs:47-48`) says: "A point naming something longer is refused for its own index with the modelled `PutMetricsErrorCode::METRIC_LIMIT_EXCEEDED`, and stores nothing." — a trigger the handler's `METRIC_LIMIT_EXCEEDED` bullet (`src/ops/batch_put_metrics.rs:36-39`) does not list.

Test coverage that pins the CODE side (so the doc is the stale side):
- `tests/all/envelope_and_per_item_reporting.rs:607` case `("a name with nothing in it", "   ", "2.5", true)` with `assert_eq!(code, "INTERNAL_ERROR", ...)` at line 656-659, and `tests/all/envelope_and_per_item_reporting.rs:775-779` asserting `(Some("METRIC_LIMIT_EXCEEDED"), Some(1))` for a name of `MAX_NAME_BYTES + 1` bytes;
- `tests/all/metrics_roundtrip.rs:778` `a_metric_name_past_the_cap_is_refused_for_that_index`.

## Steps to Reproduce

1. Open `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/ops/batch_put_metrics.rs` and read lines 41-52: the same `VALIDATION_ERROR` paragraph appears twice, with different claims about what happens to a blank or long `MetricName`.
2. Read line 234 of the same file: a point whose `MetricName` trims to empty is reported as `INTERNAL_ERROR` and never recorded — the opposite of "stored under the name as sent — blank ... — and read back under that same name".
3. Read `src/state.rs:160-162` plus its doc at lines 44-48: a `MetricName` longer than `MAX_NAME_BYTES` is refused with `METRIC_LIMIT_EXCEEDED`, contradicting "however long, whatever it holds" and missing from the handler's `METRIC_LIMIT_EXCEEDED` bullet at lines 36-39.
4. Confirm the behaviour (not the doc) is what the tests pin: `tests/all/envelope_and_per_item_reporting.rs:595-688` (blank name -> `INTERNAL_ERROR`) and `:744-779` (over-long name -> `METRIC_LIMIT_EXCEEDED`).
5. Keep one paragraph, stating the handler's actual rules (blank/absent name -> `INTERNAL_ERROR`; name past `MAX_NAME_BYTES` -> `METRIC_LIMIT_EXCEEDED`), and delete the drifted copy; no response body or test changes.
