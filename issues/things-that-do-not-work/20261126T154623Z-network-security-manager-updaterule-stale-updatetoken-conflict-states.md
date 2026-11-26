Issue-Source: trace-sweep-divergence
Issue-Trace: network-security-manager-rule-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-security-manager-rule-lifecycle
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager|UpdateRule|4xx->4xx|$.message
Issue-Commit: 80a7c7f5468625a96d1dbbdd9db26cb93639edde

# network-security-manager UpdateRule stale-updateToken conflict states a generic token message instead of AWS's concurrent-modification wording

> _Note: this issue was identified against the code base at commit `80a7c7f5468625a96d1dbbdd9db26cb93639edde`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRule

## Description
Single cause.

Cause 1 of 1: UpdateRule's optimistic-concurrency refusal carries the wrong error message text.

AWS: UpdateRule (PATCH /rules/{ruleIdentifier}) takes a required `updateToken` member (Smithy: UpdateRule.updateToken -> UpdateToken, @required, @length min 36 max 36, @pattern ^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$). When the supplied token is not the resource's current token the operation fails with the modeled error ConflictException (Smithy: UpdateRule errors include ConflictException, @error client, @httpError 409; ConflictException.message is @required, and the shape is documented as "The request conflicts with the current state of the resource. For example, the resource was modified concurrently, or it is in a state that does not allow the requested operation."). The model does not fix the message string, so the requirement for the exact wording is recorded behavior only: the recording shows HTTP 409 with $.message = "Invalid update token - entity was modified by another process". That message is entirely FIXED text for this error class — it interpolates no identifier, token, name or timestamp from the request or from state, so there is no variable part to substitute and nothing in it is a per-run value.

Emulator: the emulator does detect the stale token and does answer HTTP 409 (status matches), but serializes $.message as "The provided updateToken is not the current updateToken for the resource" — a generic, locally-invented sentence about the token rather than AWS's concurrent-modification wording. Replay verdict: mismatch, `$.message: value_mismatch (expected 'Invalid update token - entity was modified by another process', got 'The provided updateToken is not the current updateToken for the resource')`, expected_status 400->no: expected 409, actual 409.

Why: an error message is part of the contract a client sees. Callers and SDK-level retry/diagnostic layers that branch on, log, or surface the ConflictException message for an optimistic-concurrency collision get text that never appears on real AWS; anything matching the message (test assertions, operator runbooks, log-based alerting on concurrent-modification collisions, UI strings that explain to a user that another process changed the rule) silently fails to fire or reports a different cause than AWS reports. Because the status code and error type are right, the divergence is invisible until something reads the text.

Fix: belongs in the UpdateRule handler's optimistic-concurrency check in the network-security-manager service crate — the site that compares the request's `updateToken` against the stored rule's current token and constructs the ConflictException. That construction must emit AWS's fixed message for the stale-token case rather than a generic token-mismatch sentence; the generic text should be reserved for conflict causes AWS actually words that way, if any. Note the same class of defect is already filed for a different operation and error (GetTemplate not-found message, issue 20261124T100554Z-network-security-manager-gettemplate-not-found-refusal-states.md) and for the CreatePolicy resourcesCleanUp field message (issue 20261121T005237Z-network-security-manager-policy-validation-and-scope-read.md); neither covers UpdateRule's ConflictException, so a per-operation sweep of refusal message text in this service is likely warranted alongside this fix.

Traces: network-security-manager-rule-lifecycle#task#4 (UpdateRule with a superseded updateToken -> 409 ConflictException).

## Evidence
Fixture network-security-manager-rule-lifecycle, step task#4, operation UpdateRule.

Expected (recorded AWS response): HTTP 409, ConflictException, body $.message = "Invalid update token - entity was modified by another process".
Actual (emulator): HTTP 409, body $.message = "The provided updateToken is not the current updateToken for the resource".

Replay detail verbatim: "$.message: value_mismatch (expected 'Invalid update token - entity was modified by another process', got 'The provided updateToken is not the current updateToken for the resource')".
Divergence key: service=network-security-manager|UpdateRule|status=409->409|keys=$.message. Verdict: mismatch. affected_count 1. owned_by: none (net-new).

Message anatomy: the whole string "Invalid update token - entity was modified by another process" is fixed text for this error class; no part of it is drawn from the request or from emulator state, so no per-run value (token, rule id, version, timestamp) is being compared here.

Model evidence (Smithy, com-amazonaws-networksecuritymanager-piccolocustomerapiservice.json): UpdateRule http {method: PATCH, uri: /rules/{ruleIdentifier}, code: 200}; input member updateToken @required @length(36,36) @pattern ^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$; errors include ConflictException (client, httpError 409) whose `message` member is @required.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1); or replay fixture network-security-manager-rule-lifecycle alone.
3. The fixture creates a rule, reads/updates it so that the rule's updateToken is rotated, then at step task#4 issues PATCH /rules/{ruleIdentifier} with the now-superseded updateToken (a 36-char UUID matching UpdateToken's pattern) plus the required isPublished member.
4. Observe the response: status 409 as expected, but the JSON body's `message` is "The provided updateToken is not the current updateToken for the resource" instead of the recorded AWS text "Invalid update token - entity was modified by another process".
5. Step network-security-manager-rule-lifecycle#task#4 is reported as a mismatch on key $.message.
