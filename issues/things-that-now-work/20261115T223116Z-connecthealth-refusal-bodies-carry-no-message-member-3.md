Issue-Source: trace-sweep-divergence
Issue-Trace: connecthealth-get-subscription-not-found
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connecthealth-get-subscription-not-found connecthealth-subscription-error-paths
Issue-Shape-Id: com.amazonaws.connecthealth#ConnectHealth
Issue-Key: connecthealth|GetSubscription|4xx->4xx|$.Message
Issue-Key: connecthealth|ListSubscriptions|4xx->4xx|$.Message
Issue-Key: connecthealth|ActivateSubscription|4xx->4xx|$.Message
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# connecthealth refusal bodies carry no Message member: 3 causes across GetSubscription, ActivateSubscription and ListSubscriptions

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSubscription

## Description
Three connecthealth refusals answer the RIGHT HTTP status and the right error class, but their JSON body carries no `Message` member at all: the recorded wire body puts the exception's detail text under the member name `Message`, and nothing is present at that key in the emulator's body. The handlers themselves build the correct sentence (they all pass `message: Some(...)`), so the defect is in HOW that member reaches the wire, not in the text: `services/service-com-amazonaws-connecthealth-connecthealth/src/service/error_detail.rs` states in its own module documentation that "on the restJson1 wire that text travels under the Smithy member name - `message`, lowercase - which is exactly what the framework's renderers write". The recordings contradict that premise for this service: ConnectHealth serves the detail under `Message`. The consequence is one shared fault with three recorded instances, one per error class and operation below. All three are net-new: no open connecthealth issue carries these replay keys (the nearest open neighbours - 20261115T073931Z-connecthealth-deletedomain-answers-a-never-allocated-domainid-the.md, about DeleteDomain's error CLASS, and 20261029T064433Z-connecthealth-untagresource-error-response.md, about UntagResource answering 501 - cover neither this member nor these steps), and the FIXED 20261029T202808Z-connecthealth-response-bodies-diverge-from-the-recorded-wire.md does not cover a failure.

=== Cause 1 of 3: GetSubscription's ResourceNotFoundException body has no Message member ===
AWS: Smithy (services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json): `GetSubscription` is `GET /domains/{domainId}/subscriptions/{subscriptionId}`, success 200, and models `ResourceNotFoundException` (errorKind client, httpError 404, single member `message` targeting `String`, no `@jsonName`) for an unresolvable resource. The recording shows the refusal served at HTTP 404 with the detail text under the wire key `Message`, worded `Subscription not found: <domainId>/<subscriptionId>` - fixed text `Subscription not found: ` plus the two identifiers from the REQUEST, joined by `/`. The ids in the recording (`dom-3592204305683d97/sub-000000000000000000000`) only fill that template and are not part of it.
Emulator: status is correct (404) but `$.Message` is absent - replay reports `$.Message: missing (expected 'Subscription not found: dom-3592204305683d97/sub-000000000000000000000', got None)`. `src/ops/get_subscription.rs` (the `store.live_subscription(...)` else-arm) does construct `ResourceNotFoundException { message: Some(subscription_not_found_message(domain_id, subscription_id)) }`, i.e. the sentence itself is right; it simply never appears under the member name AWS uses.
Why: a client that reads `Error.Message` off a 404 - a generated SDK exception's `.message`, a CLI error line, a retry/branching rule that distinguishes a missing subscription from a missing parent domain - gets nothing. The whole discriminator between the two 404s of this service is that sentence, so a caller cannot tell them apart and operator output for a failed read is blank.
Fix: the restJson1 error serializer / error-rendering path this crate's refusals go through (the renderer named in `src/service/error_detail.rs`'s module documentation and the error channel in `src/scaffolded.rs`), so every modelled exception of this service emits its detail under the member name the service's wire uses, for ALL error classes rather than per handler. Not the handler and not the text constructors - those are already correct.
Traces: connecthealth-get-subscription-not-found#task#4.

=== Cause 2 of 3: ListSubscriptions' ValidationException body for an out-of-range maxResults has no Message member ===
AWS: Smithy: `ListSubscriptions` is `GET /domains/{domainId}/subscriptions`, `maxResults` targets `Integer` with `@range` min 1 max 100, bound to the `maxResults` query string; `ValidationException` (errorKind client, httpError 400, single member `message`) is modelled. A `maxResults` above the bound is refused at HTTP 400 with the standard constraint sentence under `Message`: `1 validation error detected: Value '<value>' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100` - fixed text apart from the rejected value (from the request) and the bound (from the model). `101` in the recording is only the value that filled it.
Emulator: status is correct (400) but `$.Message` is absent - replay reports `$.Message: missing (expected "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100", got None)`. `src/ops/list_subscriptions.rs` lifts the page-resolution failure into `ValidationException` via `crate::state::invalid_request(detail)`, and `src/service/error_detail.rs::range_violation_message` renders exactly the expected sentence (its own unit test `the_range_violation_sentence_names_the_value_the_member_and_the_bound` asserts that string verbatim) - so again the text exists and only the wire member is missing.
Why: a validation refusal whose body names neither the member nor the bound is undebuggable - a client paging this API cannot learn that `maxResults` was the offending member or that 100 is the ceiling, and any client-side handling keyed on the constraint clause sees an empty message.
Fix: same component as Cause 1 - the service's error-body serializer, which must carry the member for `ValidationException` too (the framework `Violation`/`envelope` rendering upstream of it is correct).
Traces: connecthealth-subscription-error-paths#task#3.

=== Cause 3 of 3: ActivateSubscription's ResourceNotFoundException body has no Message member ===
AWS: Smithy: `ActivateSubscription` is `POST /domains/{domainId}/subscriptions/{subscriptionId}/activate`, success 200, modelling `ResourceNotFoundException` (httpError 404, member `message`). An unresolvable subscription is refused at HTTP 404 with the same `Subscription not found: <domainId>/<subscriptionId>` wording as the reader, under `Message` - the identifiers come from the request (the recording's `dom-3c105961e47ddcd8/sub-d4d4d4d4d4d4d4d4d4d4d` only fills the template).
Emulator: status is correct (404) but `$.Message` is absent - replay reports `$.Message: missing (expected 'Subscription not found: dom-3c105961e47ddcd8/sub-d4d4d4d4d4d4d4d4d4d4d', got None)`. The handler shares the `subscription_not_found_message` constructor documented in `src/service/error_detail.rs` as the single wording for the reader and the two status transitions, so the sentence is right and unsent.
Why: a state-transition call is the one a caller retries or rolls back on, and with no message it cannot distinguish "the subscription is gone" from "its parent domain is gone" - the two cases this sentence is explicitly built to separate.
Fix: same component as Cause 1; this cause exists to confirm the fault is the service-wide error body and not one operation's handler, so a fix must be verified on a write path as well as a read path.
Traces: connecthealth-subscription-error-paths#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: 62 steps, 55 passed, 7 mismatched (score 88.7%), emulator_up=true, bootstrap_signal=false. Four of the seven mismatches are owned by open issues and skipped; the three below are net-new and all on service `connecthealth`.

1) fixture connecthealth-get-subscription-not-found, step task#4 - GetSubscription [mismatch]
   divergence_key: service=connecthealth|GetSubscription|status=404->404|keys=$.Message
   Expected: HTTP 404 with $.Message = 'Subscription not found: dom-3592204305683d97/sub-000000000000000000000'
   Actual:   HTTP 404, $.Message missing (got None). Status matches; only the member is absent.

2) fixture connecthealth-subscription-error-paths, step task#3 - ListSubscriptions [mismatch]
   divergence_key: service=connecthealth|ListSubscriptions|status=400->400|keys=$.Message
   Expected: HTTP 400 with $.Message = "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
   Actual:   HTTP 400, $.Message missing (got None).

3) fixture connecthealth-subscription-error-paths, step task#4 - ActivateSubscription [mismatch]
   divergence_key: service=connecthealth|ActivateSubscription|status=404->404|keys=$.Message
   Expected: HTTP 404 with $.Message = 'Subscription not found: dom-3c105961e47ddcd8/sub-d4d4d4d4d4d4d4d4d4d4d'
   Actual:   HTTP 404, $.Message missing (got None).

Model: smithy_query(query="operation", operation="GetSubscription,ActivateSubscription,ListSubscriptions", model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json") - each models AccessDeniedException(401), InternalServerException(500), ResourceNotFoundException(404), ValidationException(400); ListSubscriptions.maxResults has @range min 1 max 100. smithy_query(query="shape", shape="ResourceNotFoundException,ValidationException") - both are structures with the single member `message` targeting String and no @jsonName.

Code: src/ops/get_subscription.rs lines 160-172 and src/ops/list_subscriptions.rs lines 162-192 both build the exception with `message: Some(...)`, so the detail is populated in the handler. src/service/error_detail.rs lines 3-22 assert the body therefore goes out as {"message":"Subscription not found: ..."} (lowercase), and lines 193-207 / 74-86 hold the two text constructors that produce exactly the expected sentences - the text is correct, the wire member is not the one the recordings carry (`Message`). The casing premise is stated once in that module and applies to every exception of the service, which is why three unrelated operations and two error classes all diverge identically.

Not filed as defects here: the four remaining mismatches of this replay are owned - connecthealth StartPatientInsightsJob/GetPatientInsightsJob (20261109T171442Z-connecthealth-patient-insights-job-responses-diverge-epoch-number.md), s3 GetObject (20261109T171442Z-s3-getobject-answers-404-for-the-patient-insights.md) and s3 DeleteBucket (20261113T230556Z-s3-deletebucket-answers-409-for-an-emptied-bucket.md).

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or reproduce each cause by hand with a client pointed at the connecthealth endpoint and read the RAW HTTP error body (e.g. `aws --debug`, or a botocore event hook on `before-parse`) rather than the SDK-parsed exception, since the key name is what diverges:

1. Cause 1 - fixture connecthealth-get-subscription-not-found step task#4: CreateDomain(name=<unique>), note the returned domainId, then GetSubscription(domainId=<that id>, subscriptionId="sub-" + "0"*21) for a subscription that was never created. Expect HTTP 404, x-amzn-errortype ResourceNotFoundException, body {"Message":"Subscription not found: <domainId>/sub-000000000000000000000"}. Observe HTTP 404 with no `Message` member in the body.
2. Cause 2 - fixture connecthealth-subscription-error-paths step task#3: ListSubscriptions(domainId=<a live domainId>, maxResults=101). Expect HTTP 400, ValidationException, body {"Message":"1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"}. Observe HTTP 400 with no `Message` member. Repeat with maxResults=0 to confirm the minimum clause ("Member must have value greater than or equal to 1") is also carried once the member is emitted.
3. Cause 3 - fixture connecthealth-subscription-error-paths step task#4: ActivateSubscription(domainId=<a live domainId>, subscriptionId="sub-d4d4d4d4d4d4d4d4d4d4d") for a subscription that does not exist. Expect HTTP 404, ResourceNotFoundException, body {"Message":"Subscription not found: <domainId>/sub-d4d4d4d4d4d4d4d4d4d4d"}. Observe HTTP 404 with no `Message` member.
4. Guard the whole service, not these three steps: after the fix, re-replay connecthealth-get-subscription-not-found and connecthealth-subscription-error-paths in full, and also exercise one AccessDeniedException (401) and one 500 path so the member is confirmed for every modelled exception class; then re-run the connecthealth error-semantics behavioral group to confirm no refusal lost or duplicated its detail text.
