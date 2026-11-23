Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 78710bf1ded34debb3f13a52d5e5c883d8b9537d

# An envelope refusal is answered in front of the IAM decision, so an unauthorized caller is told about its request instead of being refused 403

_Filed: 2026-11-23T09:54:25Z_
_Operation: BatchPutMetrics | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `78710bf1ded34debb3f13a52d5e5c883d8b9537d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states ONE ordering for the authorization decision: IAM decides before the service judges anything about the request, and a request IAM refuses "never reached the service", so it reports nothing about the resource. Both handlers honour it — `crate::service::authorization::refusal` is the first statement in each `handle`.

The envelope layer does not. `EmulatedService::override_operation` hands the router `REFUSED_ENVELOPE` for any request whose envelope this service refuses, and `RefusedEnvelope::run` re-derives that refusal and answers it directly — it never calls `crate::service::authorization::refusal`, and the modelled operation (the only place the IAM gate lives) is never run. So a request-serving path of this crate SKIPS a helper the crate already has and every sibling path calls first.

The consequence is that for a caller whose identity-based policies allow nothing, the three envelope refusals pre-empt the 403:

* `PUT /BatchPutMetrics` with a blank/absent/null `TrialComponentName` → 400 `ValidationException` with the constant sentence `"Invalid resource."` — the service telling an unauthorized caller something about the resource, which is exactly what `authorization.rs:9-12` says must not happen;
* `PUT /BatchPutMetrics` with no `MetricData`, and `POST /BatchGetMetrics` with no `MetricQueries` → 500 `InternalFailure`;
* either operation carrying more than `crate::state::MAX_ITEMS_PER_REQUEST` items, or a member whose JSON type its shape cannot be read from → 500 / 400 `SerializationException`.

In each case the answer is decided from what the request SAID, with no IAM decision taken at all, which contradicts the ordering the crate states twice. The fix is to take the same decision the handlers take before the envelope refusal is worded (the helper is already there: `authorization::trial_component_addressed` / `referenced_resources` + `authorization::refusal`), not to change any refusal's status, code or sentence.

## Evidence

The ordering this crate states — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/service/authorization.rs:9-13`:
```
//! request at the IAM layer, ahead of the service looking at what the request named: a
//! principal whose identity-based policies do not allow the action is told it is not
//! authorized rather than told anything about the resource, and the call has NO effect — a
//! refused `BatchPutMetrics` stores no point, so a later `BatchGetMetrics` for it reports an
//! empty series.
```
and the contract on the helper itself — `src/service/authorization.rs:130-132`:
```
/// Call it FIRST in a handler, before the metric store is read or written and before this
/// service judges anything about the request: a request IAM refuses never reached the service
/// on AWS, so it ingests nothing and reports nothing.
```
repeated at `src/service.rs:46-52`: "That decision is a property of the request's identity, the action it names and the resources it addresses rather than of what a handler goes on to do, so it is stated ONCE in [`authorization`] and taken by each operation before it reads or writes the metric store."

The sibling paths that honour it — `src/ops/batch_put_metrics.rs:200-208`:
```
        let resources = [crate::service::authorization::trial_component_addressed(
            ctx,
            &input.trial_component_name,
        )];
        if let Some(denial) =
            crate::service::authorization::refusal(&*self.engine, ctx, Self::NAME, &resources)
        {
            return Err(crate::scaffolded::Scaffolded::access_denied(denial));
```
and `src/ops/batch_get_metrics.rs:514-524` (the same two calls, first in `handle`).

The path that skips it — `src/service.rs:148`:
```
        envelope::refuses(self.definition(), req).then_some(&envelope::REFUSED_ENVELOPE as _)
```
and `src/service/envelope.rs:507-535`, whose whole body is:
```
        let definition = svc.definition();
...
        let refused = refusal(definition, req).and_then(|refusal| refusal.response(definition));
        match refused {
            Some(response) => Box::pin(std::future::ready(response)),
            None => match definition
                .protocol()
                .routing_key(req)
...
```
— no `crate::service::authorization` call anywhere in the module (its only authorization-adjacent note is `src/service/envelope.rs:538-541`, which speaks of AUTHENTICATION only: "a request this layer refuses is authenticated exactly as the operation it addressed is").

That this short-circuits the modelled operation (and therefore the IAM gate) — `vendor/smithy-server-runtime/src/multi_service_router.rs:239-245`:
```
        let op = match svc.override_operation(&rt_req) {
            Some(op) => Some(op),
            None => match wire.protocol().routing_key(&rt_req) {
```
The sentence an unauthorized caller gets instead — `src/service/envelope.rs:98`: `pub const INVALID_RESOURCE: &str = "Invalid resource.";` answered at 400 `ValidationException` (`src/service/envelope.rs:170-183`).

Test coverage: the 403 rule is pinned only for WELL-FORMED envelopes — `tests/all/resource_arn_refusal.rs:1177-1181` ("a caller no readable statement grants … is answered `403 AccessDeniedException`") drives the bodies at `tests/all/resource_arn_refusal.rs:1023-1049`, both of which carry a full `TrialComponentName`/`MetricData` and one well-formed query. The envelope-refusal tests (`tests/all/envelope_and_per_item_reporting.rs:154-296`) all call as `ACCOUNT_KEY`, the credential `tests/all/resource_arn_refusal.rs:1234-1235` documents as holding no policies but being ALLOWED. No test crosses the two, so this answer is unpinned.

## Steps to Reproduce

1. Build the emulator with the identity services mounted, as tests/all/resource_arn_refusal.rs does, and assume a role whose identity-based policy allows nothing (the refusing row of its GRANTS table).
2. Signed with that role's access key, send PUT /BatchPutMetrics with body {"TrialComponentName":"   ","MetricData":[{"MetricName":"loss","Timestamp":1700000000,"Step":0,"Value":7.5}]}.
3. Observed: 400 with {"__type":"ValidationException","message":"Invalid resource."} — src/service.rs:148 hands the router envelope::REFUSED_ENVELOPE, and src/service/envelope.rs:520 answers the refusal without ever calling crate::service::authorization::refusal, so the handler at src/ops/batch_put_metrics.rs:200-208 never runs.
4. Expected from the crate's own stated ordering (src/service/authorization.rs:9-13 and :130-132, src/service.rs:46-52): the IAM decision is taken first, so the same caller is answered 403 AccessDeniedException with IAM's sentence and is told nothing about the resource.
5. Repeat with the same unauthorized credential and POST /BatchGetMetrics body {"MetricQueries":[]} → 500 InternalFailure with no detail member, again with no IAM decision taken; and with a batch of more than crate::state::MAX_ITEMS_PER_REQUEST items → 500 "Unhandled internal failure".
6. Compare with the authorized path: the same bodies sent by a permitted caller are answered by exactly the same envelope refusals, so the refusal wording/status need not change — only the missing authorization step in front of it.
