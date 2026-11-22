Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 6b0d7166eeee5b3e65d1a0791f9b11ca91cde3cf

# A whitespace-only ResourceArn fails the whole call, though the crate reads blank text as no value

_Filed: 2026-11-22T10:10:16Z_
_Operation: BatchGetMetrics | Class: error-response_

> _Note: this issue was identified against the code base at commit `6b0d7166eeee5b3e65d1a0791f9b11ca91cde3cf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`names_a_sagemaker_resource` exempts only the EMPTY string from the SageMaker-ARN expression, so a query carrying `"ResourceArn": "   "` is treated as a malformed reference and refuses the WHOLE call with HTTP 400 ValidationException, while the same query carrying `""` (or null, or no member at all) is served in-band as a query that selects nothing. Everywhere else this crate states the opposite rule — blank text says no more than an absent member does: the envelope layer trims before deciding a member carries no value, and the sibling BatchPutMetrics handler trims a `MetricName` before deciding it names no series. On this one path the blank reference is judged as a submitted value instead, so one call of a 200-reporting batch API is turned into a 400 for a member the crate elsewhere reads as unset.

## Evidence

The check that exempts only the empty string: `src/ops/batch_get_metrics.rs:363-365` — `fn names_a_sagemaker_resource(resource_arn: &str) -> bool { resource_arn.is_empty() || SAGEMAKER_RESOURCE_ARN.is_match(resource_arn) }`, reached from `src/ops/batch_get_metrics.rs:517-525` — `.any(|query| !names_a_sagemaker_resource(&query.resource_arn)) { return Err(crate::scaffolded::Scaffolded::validation(INVALID_RESOURCE_ARN_MESSAGE)) }` (HTTP 400, `src/scaffolded.rs:383`).

The crate's own rule for blank text: `src/service/envelope.rs:477-479` — "Whether the request carries NO value for a member: the key is absent, its value is JSON `null`, or it is an empty collection or blank text — a list with no element and a name with no non-blank character state no more than an absent member does.", implemented at `src/service/envelope.rs:487` — `Some(Value::String(text)) => text.trim().is_empty(),`. The sibling handler applies the same trim to an item's name: `src/ops/batch_put_metrics.rs:234` — `if raw.value.is_nan() || raw.metric_name.trim().is_empty() {`.

Existing coverage pins only the empty/null/absent forms as served in-band: `tests/all/resource_arn_refusal.rs:351-355` — `async fn a_reference_the_request_does_not_state_is_not_a_malformed_one()` with rows `("empty-reference", query(""))`, `("null-reference", …)`, `("no-reference-member", …)`; no row covers whitespace, and `malformed_arns()` (`tests/all/resource_arn_refusal.rs:214-248`) does not list it either.

## Steps to Reproduce

1. POST /BatchGetMetrics with {"MetricQueries":[{"MetricName":"loss","ResourceArn":"","MetricStat":"Last","Period":"IterationNumber","XAxisType":"IterationNumber"}]} — 200, one result Status=Complete with empty lists (tests/all/resource_arn_refusal.rs:351).
2. POST the same body with "ResourceArn": "   " — the whole call is refused with HTTP 400 ValidationException "Invalid SageMaker ARN", although src/service/envelope.rs:477-487 states blank text carries no value and src/ops/batch_put_metrics.rs:234 trims an item's name before judging it.
