Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 483716e1c498104caa88e0cc5f8ffa67f1e1067f

# BatchGetMetrics aggregates a stored infinite Value into a NaN MetricValues entry, contradicting its own reduce() invariant (StdDev of a one-point bucket is NaN, not zero)

_Filed: 2026-11-22T08:40:44Z_
_Operation: BatchGetMetrics | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `483716e1c498104caa88e0cc5f8ffa67f1e1067f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`BatchPutMetrics` deliberately STORES an infinite `Value` (the handler only refuses `NaN`), and the crate pins that with a test. `BatchGetMetrics::Statistic::reduce` then computes `Avg` and `StdDev` with plain float arithmetic over the bucket's values, which produces NOT A NUMBER as soon as a stored value is infinite:

* `StdDev` over a bucket holding one infinite point: `mean = inf`, `(v - mean) = inf - inf = NaN`, `variance = NaN`, `variance.sqrt() = NaN` — so the result is `NaN`, even though the branch's own comment states that "a one-point bucket has a spread of zero rather than an undefined one".
* `Avg` over a bucket holding both `Infinity` and `-Infinity`: `sum = NaN`, so `sum / count = NaN`.

The `MetricValues` entry handed back is therefore not a number — exactly the thing this crate refuses everywhere else. `reduce`'s own doc claims the opposite ("the reduction is the number the statistic names"), `crate::state::MetricDatum.value` documents the store's invariant as "Never `NaN`: a value with no position in any ordering is not stored, so every stored value is comparable and aggregatable", and `BatchPutMetrics` reports a `NaN` point as `INTERNAL_ERROR` precisely because such a value "can be neither stored at a coordinate nor aggregated". The read path manufactures that same non-number and serializes it into the response, so a client reading `MetricValues` as a list of doubles receives a non-finite entry for a query the service reports `Complete`.

Note the defect is in the AGGREGATION, not in ingestion: that an infinite `Value` is stored is pinned by an existing test (`("unbounded above", "series", "\"Infinity\"", false)` — not reported, i.e. ingested), so the reduction has to cope with the values the store is documented to hold.

## Evidence

Handler doc that states the rule the code breaks — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/ops/batch_get_metrics.rs:202-206`:
```
    /// Reduce one bucket's values, which arrive ascending by `(timestamp, step)`, to the
    /// single number this statistic names. `values` is never empty: a bucket exists only
    /// because a point fell in it. No stored value is `NaN` (`BatchPutMetrics` stores
    /// none), so every comparison below is total and the reduction is the number the
    /// statistic names.
```
The code under it — `src/ops/batch_get_metrics.rs:208-223`:
```
        let count = values.len() as f64;
        let sum: f64 = values.iter().sum();
        match self {
...
            Self::Avg => sum / count,
...
            Self::StdDev => {
                // Population standard deviation over the bucket's own points: the
                // bucket is the whole population being summarised, so a one-point
                // bucket has a spread of zero rather than an undefined one.
                let mean = sum / count;
                let variance =
                    values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / count;
                variance.sqrt()
            }
```
For a one-point bucket whose value is `f64::INFINITY`: `mean = inf`, `(v - mean) = NaN`, `variance = NaN`, `variance.sqrt() = NaN` — not the "spread of zero" the comment promises.

That an infinite value IS stored — `src/ops/batch_put_metrics.rs:41-46`:
```
//! `VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service
//! validates nothing about a point it can store. An INFINITE `Value` is a number and is
//! stored as one; ...
```
and `src/ops/batch_put_metrics.rs:234`: `if raw.value.is_nan() || raw.metric_name.trim().is_empty() {` — only `NaN` is refused, `±Infinity` falls through to `state.record(..)`.

The store invariant the read path violates — `src/state.rs:83-85`:
```
    /// `RawMetricData.Value`. Never `NaN`: a value with no position in any ordering is
    /// not stored, so every stored value is comparable and aggregatable.
```
and the ingest rule it mirrors — `src/ops/batch_put_metrics.rs:27-31`: "`INTERNAL_ERROR` — the point never became a measurement: a `Value` that is NOT A NUMBER (`NaN` ...), which has no position in any ordering and so can be neither stored at a coordinate nor aggregated".

Test coverage: the ingest side is pinned — `tests/all/envelope_and_per_item_reporting.rs:603-608`:
```
        ("not a number", "series", "\"NaN\"", true),
        ("unbounded above", "series", "\"Infinity\"", false),
        ("unbounded below", "series", "\"-Infinity\"", false),
```
with `assert!(errors.is_empty(), "case {case:?}: a point that IS a measurement is stored, got {errors:?}")` at `:633-636`, and `tests/all/envelope_and_per_item_reporting.rs:559` ingests `{"MetricName": "loss", "Timestamp": T0 + 11, "Value": "Infinity"}` in a batch asserted to report no error. NO test aggregates a non-finite stored value: the only `StdDev` cases are `tests/all/metrics_roundtrip.rs:418` and `:462`, both over finite series — so this output is unpinned and net-new.

## Steps to Reproduce

1. PUT /BatchPutMetrics with body {"TrialComponentName":"tc-inf","MetricData":[{"MetricName":"loss","Timestamp":1700000000,"Step":0,"Value":"Infinity"}]} (the quoted non-finite form this protocol's codec reads a float from, as `src/service/envelope.rs:395` NON_FINITE_TEXTS records). The answer is 200 with Errors: [] — the point is stored (`src/ops/batch_put_metrics.rs:234` only refuses NaN).
2. POST /BatchGetMetrics with one query: MetricName "loss", ResourceArn "arn:aws:sagemaker:us-east-1:123456789012:experiment-trial-component/tc-inf", MetricStat "StdDev", Period "IterationNumber", XAxisType "IterationNumber".
3. The one stored point forms a single bucket, so by the comment at src/ops/batch_get_metrics.rs:216-218 the spread must be 0; instead `reduce` computes mean = inf, (inf - inf) = NaN, variance = NaN, sqrt(NaN) = NaN, and MetricValues[0] is handed back as a non-number while Status is "Complete".
4. Repeat with MetricStat "Avg" over a bucket holding both "Infinity" and "-Infinity" (two points in the same Period window): sum = NaN, so Avg is NaN as well.
5. Compare with src/state.rs:83-85 ("every stored value is comparable and aggregatable") and src/ops/batch_get_metrics.rs:204-206 ("the reduction is the number the statistic names") — the aggregation emits the very not-a-number the ingest path refuses, without changing the pinned rule that an infinite Value is stored.
