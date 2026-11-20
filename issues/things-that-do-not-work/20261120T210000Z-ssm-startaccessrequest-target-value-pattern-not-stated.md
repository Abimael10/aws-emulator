Issue-Source: fixer-unmasked-divergence
Issue-Trace: ssm-jit-access-request-invalid-target-error
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-jit-access-request-invalid-target-error
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM

# ssm StartAccessRequest: a just-in-time target value outside the expression AWS constrains it with is not refused at the boundary, so the account gate answers instead

## Operation
StartAccessRequest

## Description
Filed by the fixer of `20261109T173225Z-ssm-just-in-time-node-access-the-400.md`, which closed a
DIFFERENT cause on the same fixture cluster (the refusal body carried no `message` member) and
left this one visible once the member was there. It is not the cause that issue describes: that
issue asserts the account-gate sentence for this fixture, and the recording does not.

AWS: the recorded answer to `StartAccessRequest` carrying a target value outside the expression
AWS constrains a just-in-time target value with is HTTP 400 `ValidationException` with
`message` = `1 validation error detected: Value at 'targets.1.member.values' failed to satisfy
constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern:
^(?:…` — the replay truncates the expression after its opening `^(?:`. The clause is stated at
the COLLECTION member (`targets.<index>.member.values`) with the element shape's constraint set
nested inside `Member must satisfy constraint: [ … ]`, and it is reached BEFORE the account-level
feature gate, so a request with such a target is refused for its shape whether or not the account
holds an access-request approval policy.

Emulator: the request passes the modelled-constraint walk and the handler answers the
account-gate `ValidationException` (`Just-In-Time Node Access service is not enabled in this
account.`) instead. Status (400) and error class (`ValidationException`) already match; the whole
divergence is `$.message`.

Why it could not be closed with that issue: the expression is not available anywhere in this
checkout. `smithy_query(query="closure", operation="StartAccessRequest")` shows
`StartAccessRequestRequest$Targets` -> `Targets` (@length 0..5) -> `Target` -> `Values` ->
`TargetValues` (@length 0..50) -> `TargetValue`, a plain string carrying NO `@pattern`, and no
`@pattern` anywhere in `services/service-com-amazonaws-ssm-amazonssm/smithy-model/
com-amazonaws-ssm-amazonssm.json` even begins `^(?:`. The replay withholds the rest of the
recorded text and AWS publishes no such expression for a just-in-time target value, so writing
one would be inventing a regular expression no rule here produces — and a guessed one would also
refuse the `tag:` and `resource-groups:` target keys the modelled `Target$Key` admits.

Fix: state the constraint where this crate already states the ones the live service enforces and
the on-disk model does not —
`services/service-com-amazonaws-ssm-amazonssm/src/service/refusal/stated_constraints.rs`, whose
`STATED_ELEMENTS` rows carry exactly this clause form
(`service_common::validation_message::element_set_requirement` renders
`Member must satisfy constraint: [ … ]`) — adding the expression AWS states for a just-in-time
target value, keyed by the member path with its element steps folded away. What the fix needs
that this checkout does not have is the full recorded expression: read it off the recorded
response for `ssm-jit-access-request-invalid-target-error#task#1` rather than guessing it.

## Evidence
Fixture `ssm-jit-access-request-invalid-target-error`, step `task#1`, StartAccessRequest
[verdict mismatch, status 400 -> 400]:
  expected `$.message`: `1 validation error detected: Value at 'targets.1.member.values' failed
  to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression
  pattern: ^(?:…` (truncated by the replay)
  actual   `$.message`: `Just-In-Time Node Access service is not enabled in this account.`
The three sibling fixtures of that cluster
(`ssm-access-request-service-not-enabled-errors#task#1`,
`ssm-jit-access-request-service-not-enabled-error#task#1`,
`ssm-jit-access-request-workflow#task#1`) pass: for a target whose values are inside the
expression, the gate sentence IS the recorded answer.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set
   simparity-wire-aws-v1).
2. Fixture `ssm-jit-access-request-invalid-target-error`, step `task#1`: call
   `ssm.StartAccessRequest` with a `Reason` inside 1..256 and a `Targets` entry whose `Values`
   carry the fixture's invalid node value, against an account holding no access-request approval
   policy.
3. Observe HTTP 400 `ValidationException` whose `message` is the account-gate sentence, where
   AWS states the `targets.<index>.member.values` constraint clause — AWS refuses the shape
   before it consults the account.
