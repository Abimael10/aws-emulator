Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.cache-policy-negative-min-ttl-rejected cloudfront.cf-cache-behavior-trusted-key-group-count-limit-rejected cloudfront.distribution-cache-behavior-count-quota-boundary-rejected cloudfront.response-headers-policy-server-timing-sampling-rate-over-100-rejected cloudfront.realtime-log-config-mutation-requires-exactly-one-identifier cloudfront.list-origin-request-policies-invalid-marker-rejected cloudfront.list-key-value-stores-invalid-marker-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateCachePolicy|InvalidArgument
Issue-Key: cloudfront.cf-cache-behavior-trusted-key-group-count-limit-rejected|run#8|AssertionError|assert msg == want_msg, f"expected message {want_msg!r}, got {msg!r}"
Issue-Key: cloudfront.distribution-cache-behavior-count-quota-boundary-rejected|expect_error#3|AssertionError|assert message == expected_message, (
Issue-Key: cloudfront|CreateResponseHeadersPolicy|MalformedXML
Issue-Key: cloudfront.realtime-log-config-mutation-requires-exactly-one-identifier|run#15|AssertionError|assert u_neither_message == "Invalid parameter received. Name/ARN", (
Issue-Key: cloudfront.list-origin-request-policies-invalid-marker-rejected|run#5|AssertionError|assert code == "NoSuchOriginRequestPolicy", f"expected NoSuchOriginRequestPolicy, got {code!r}: {message!r}"
Issue-Key: cloudfront.list-key-value-stores-invalid-marker-rejected|run#6|AssertionError|assert code == "MalformedXML", f"expected MalformedXML, got {code!r}: {message!r}"
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront: 6 net-new refusal-wording causes — emulator-authored messages and wrong error classes on MinTTL, key-group/cache-behavior quotas, SamplingRate validation, realtime-log identifiers and list Markers

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCachePolicy

## Description
Six net-new root causes where the emulator does refuse the call but the refusal does not carry AWS's error class or AWS's message text. An HTTP status, an error code and the error message are requirements, so each of these is a defect; all six must be closed before submitting.

Cause 1 of 6: CreateCachePolicy's negative MinTTL refusal is emulator-authored prose.
AWS: Smithy: CachePolicyConfig.MinTTL is a required long; CreateCachePolicy binds InvalidArgument (httpError 400). Recorded refusal: 400, Code `InvalidArgument`, Message "The parameter MinTTL is not within allowed range." — entirely fixed text, the member name MinTTL being the only name in it.
Emulator: 400 `InvalidArgument` with "An argument is invalid: MinTTL is an amount of time in seconds and cannot be negative, but the request submits -1" — emulator-authored text that also interpolates the submitted value, which AWS's message does not.
Why: clients and test suites that match on AWS's message text (or log/triage on it) do not recognise the refusal.
Fix: CreateCachePolicy's validation message for an out-of-range MinTTL.
Traces: cloudfront.cache-policy-negative-min-ttl-rejected.

Cause 2 of 6: TooManyKeyGroupsAssociatedToDistribution carries emulator-authored text.
AWS: Smithy: CreateDistribution binds TooManyKeyGroupsAssociatedToDistribution (httpError 400). Recorded refusal: 400, Code `TooManyKeyGroupsAssociatedToDistribution`, Message "Processing your request will cause you to exceed the maximum number of key groups that can be associated to a distribution." — entirely fixed text, with no counts in it.
Emulator: the same code with "This distribution references <n> key groups; at most <m> are allowed per distribution" — emulator-authored, and it embeds counts AWS never reports.
Why: the refusal is unrecognisable to a client matching AWS's quota message, and the invented limit figure misleads whoever reads it.
Fix: the message emitted with TooManyKeyGroupsAssociatedToDistribution in CreateDistribution/UpdateDistribution validation.
Traces: cloudfront.cf-cache-behavior-trusted-key-group-count-limit-rejected.

Cause 3 of 6: TooManyCacheBehaviors carries emulator-authored text.
AWS: Smithy: CreateDistribution binds TooManyCacheBehaviors (httpError 400). Recorded refusal one behavior over the limit: 400, Code `TooManyCacheBehaviors`, Message "Your request contains more CacheBehaviors than are allowed per distribution." — entirely fixed text.
Emulator: the same code with "This distribution states <n> cache behaviors; at most <m> are allowed per distribution".
Why: as Cause 2 — AWS's fixed quota wording is replaced by invented prose. (The limit the emulator enforces is also wrong; that is filed separately as the cache-behavior counting cause.)
Fix: the message emitted with TooManyCacheBehaviors in CreateDistribution validation.
Traces: cloudfront.distribution-cache-behavior-count-quota-boundary-rejected.

Cause 4 of 6: the constraint-violation message for SamplingRate formats a double as an integer.
AWS: Smithy: ResponseHeadersPolicyServerTimingHeadersConfig.SamplingRate targets SamplingRate, a double with @range min 0.0 max 100.0. Recorded refusal for 150: "1 validation error detected: Value '150.0' at 'responseHeadersPolicyConfig.serverTimingHeadersConfig.samplingRate' failed to satisfy constraint: Member must have value less than or equal to 100.0" — the fixed text is the "1 validation error detected: Value '...' at '...' failed to satisfy constraint: Member must have value less than or equal to ..." frame; the variable parts are the submitted value and the modeled bound, both rendered as doubles because the member is a double.
Emulator: renders both as integers — "Value '150' ... less than or equal to 100".
Why: a client comparing the message (or parsing the reported bound) sees a different constraint than AWS states, and the double-typed member is misrepresented.
Fix: the constraint-validation message formatter for double members (CreateResponseHeadersPolicy's input validation).
Traces: cloudfront.response-headers-policy-server-timing-sampling-rate-over-100-rejected.

Cause 5 of 6: UpdateRealtimeLogConfig's identifier refusals use DeleteRealtimeLogConfig's wording.
AWS: Smithy: UpdateRealtimeLogConfig takes optional Name and ARN and binds InvalidArgument (400). Recorded refusals, both entirely fixed text: UpdateRealtimeLogConfig with neither identifier → "Invalid parameter received. Name/ARN"; with a Name and ARN that resolve to different configs → "Invalid parameter received. Name/ARN"; DeleteRealtimeLogConfig with neither → "The parameter Name/ARN must be provided."; DeleteRealtimeLogConfig with conflicting identifiers → "The parameter Name/ARN must refer to the same realtime log config." AWS words the Update and Delete refusals differently.
Emulator: answers the Delete wording ("The parameter Name/ARN must be provided.") on UpdateRealtimeLogConfig.
Why: the two operations' refusals become indistinguishable, so a client keying on the message cannot tell which precondition failed.
Fix: UpdateRealtimeLogConfig's identifier validation messages (per-operation, not shared with Delete).
Traces: cloudfront.realtime-log-config-mutation-requires-exactly-one-identifier.

Cause 6 of 6: an invalid Marker on list operations is answered with a generic InvalidArgument instead of each operation's AWS error class.
AWS: Smithy: ListOriginRequestPolicies takes Marker as an httpQuery string and binds NoSuchOriginRequestPolicy (httpError 404) alongside InvalidArgument; ListKeyValueStores likewise binds the list errors. Recorded refusals: a Marker that is not a policy id → 404 `NoSuchOriginRequestPolicy`; a malformed Marker on ListKeyValueStores → `MalformedXML`.
Emulator: both answer `InvalidArgument` with emulator-authored text — "Marker not a valid marker <marker> is not an origin request policy id returned by ListOriginRequestPolicies" and "Marker must be a valid CF key value store Name".
Why: a paginating client distinguishes "my marker is stale/gone" (not-found) from "my request is malformed" by the error class; collapsing both into InvalidArgument breaks that branch.
Fix: the shared CloudFront list-Marker validation — raise each operation's modeled error class with AWS's message.
Traces: cloudfront.list-origin-request-policies-invalid-marker-rejected, cloudfront.list-key-value-stores-invalid-marker-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1; all checks below are listed under `### cloudfront`, neither ALREADY FILED nor OWNED.

Cause 1 — cache-policy-negative-min-ttl-rejected: "expected message 'The parameter MinTTL is not within allowed range.', got 'An argument is invalid: MinTTL is an amount of time in seconds and cannot be negative, but the request submits -1'".
Cause 2 — cf-cache-behavior-trusted-key-group-count-limit-rejected: "expected message 'Processing your request will cause you to exceed the maximum number of key groups that can be associated to a distribution.', got 'This distribution references 5 key groups; at most 4 are allowed per distribution'".
Cause 3 — distribution-cache-behavior-count-quota-boundary-rejected: "expected message 'Your request contains more CacheBehaviors than are allowed per distribution.', got 'This distribution states 75 cache behaviors; at most 25 are allowed per distribution'".
Cause 4 — response-headers-policy-server-timing-sampling-rate-over-100-rejected: expected "... Value '150.0' ... Member must have value less than or equal to 100.0", got "... Value '150' ... less than or equal to 100".
Cause 5 — realtime-log-config-mutation-requires-exactly-one-identifier: UpdateRealtimeLogConfig with no identifier — "expected AWS's exact message, got 'The parameter Name/ARN must be provided.'" (AWS: 'Invalid parameter received. Name/ARN').
Cause 6 — list-origin-request-policies-invalid-marker-rejected: "expected NoSuchOriginRequestPolicy, got 'InvalidArgument': 'Marker not a valid marker !! ### is not an origin request policy id returned by ListOriginRequestPolicies'"; list-key-value-stores-invalid-marker-rejected: "expected MalformedXML, got 'InvalidArgument': 'Marker must be a valid CF key value store Name'".

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run the checks in trace_ids. Each issues one refusing call (CreateCachePolicy with MinTTL=-1; CreateDistribution over the key-group and cache-behavior limits; CreateResponseHeadersPolicy with SamplingRate=150; UpdateRealtimeLogConfig with no identifier and with conflicting Name/ARN; ListOriginRequestPolicies and ListKeyValueStores with a bogus Marker).
4. Compare the Error.Code and Error.Message byte-for-byte with the AWS text quoted per cause.
