Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatchomni-access-profile-and-grant-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatchomni-access-profile-and-grant-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|CreateAccessProfile|4xx->4xx|$.message
Issue-Commit: c9b7fc7795b284972b7c862846d545eef7a83f7a

# cloudwatchomni: CreateAccessProfile duplicate-name ConflictException message is not AWS wording and echoes request/state values

> _Note: this issue was identified against the code base at commit `c9b7fc7795b284972b7c862846d545eef7a83f7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessProfile

## Description
ONE net-new wire root cause in the cloudwatchomni emulator: the duplicate-name refusal of CreateAccessProfile returns the correct HTTP status and error code but a `message` AWS never emits, and it splices caller-supplied input and internal state into it. The error message is part of the wire contract, so the message template itself has to be fixed.

Related but NOT covering this: open issue 20261111T022337Z-cloudwatchomni-conflict-and-validation-error-messages-diverge-from.md reports the same wording class from the BEHAVIORAL corpus (its Issue-Corpus is simparity-behavioral-aws-v1 and its keys are check ids such as cloudwatchomni.access-profile-name-is-unique-within-a-space). It carries no wire replay key for this fixture step, so this wire divergence is filed separately and is the thing to re-replay. The other open cloudwatchomni issue, 20261111T022337Z-cloudwatchomni-account-region-singleton-rules-unenforced-and-name.md, is about singleton/cascade state rules, not this message.

Cause 1 of 1: CreateAccessProfile duplicate-name conflict message.
  AWS: Smithy CreateAccessProfile declares ConflictException (errorKind client, @httpError 409); CreateAccessProfileInput.spaceId is required (@pattern ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$) and CreateAccessProfileInput.name is required (@length 1..256, @pattern ^[a-zA-Z0-9 _.:/=+@-]+$). The ConflictException shape does not fix its message text, so the wording is recorded behavior only: a second access profile created under a name already taken inside the same space is refused with HTTP 409, __type ConflictException, and the message "An access profile with that name already exists in this space". That whole sentence is FIXED text — it has NO variable part at all: neither the requested `name` nor the `spaceId` appears in it, and there is no trailing period.
  Emulator: answers the correct 409 / ConflictException but with the message template "An access profile named '<name>' already exists in space '<spaceId>'." — wrong fixed text, plus two interpolated variables AWS withholds: the request's `name` and the space's id from state (in the recorded step those slots happen to hold 'sim2real-probe-cwo-prof-9bd42cb3' and '284e6de6-2c6e-497c-9b83-fa7dc94e8641'; those per-run values merely fill the emulator's template and are not themselves the defect). Only $.message diverges; status and error code match.
  Why: a client that classifies or surfaces the conflict by its message — contract tests, retry/idempotency logic keyed on the refusal text, log scrapers, text shown to an end user — sees wording AWS never produces and so fails to recognise the duplicate-name conflict. It also makes the emulator reflect caller-supplied input and an internal space id back in an error that AWS deliberately keeps opaque, which a client may then log or echo.
  Fix: CreateAccessProfile's handler — the duplicate-name ConflictException message construction (emit AWS's constant sentence; drop the name and spaceId interpolation). The conflictType/resourceId/resourceType fields the emulator also sets are not asserted by this step and are out of scope.
  Traces: cloudwatchomni-access-profile-and-grant-lifecycle#task#4.

## Evidence
Replay of fixture cloudwatchomni-access-profile-and-grant-lifecycle, step task#4 (CreateAccessProfile, verdict=mismatch, expected_status 409, actual_status 409):
  $.message: value_mismatch — expected 'An access profile with that name already exists in this space', got "An access profile named 'sim2real-probe-cwo-prof-9bd42cb3' already exists in space '284e6de6-2c6e-497c-9b83-fa7dc94e8641'."
No other key diverges in this step: the HTTP status (409) and the error code (ConflictException) are already correct, so the defect is entirely the message template.
Fixed vs variable split: AWS's text "An access profile with that name already exists in this space" is fixed in its entirety, with zero substitutions. In the emulator's text, 'sim2real-probe-cwo-prof-9bd42cb3' is the request's `name` member and '284e6de6-2c6e-497c-9b83-fa7dc94e8641' is the target space's id from emulator state — both are per-run values that must not appear at all.
Smithy cited: CreateAccessProfile errors include ConflictException (errorKind client, @httpError 409); input members spaceId (required, uuid @pattern) and name (required, @length 1..256, @pattern ^[a-zA-Z0-9 _.:/=+@-]+$). ConflictException does not model a message template, hence "recorded behavior only" for the wording.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 against it.
3. Replay fixture cloudwatchomni-access-profile-and-grant-lifecycle up to step task#4: it creates a domain, creates the space, creates an access profile with a given name in that space, then calls CreateAccessProfile again with the SAME name in the SAME space.
4. Expected of that second call: HTTP 409 with __type ConflictException and $.message exactly 'An access profile with that name already exists in this space' (no name, no spaceId, no trailing period).
5. Observed: HTTP 409 / ConflictException with $.message "An access profile named '<name>' already exists in space '<spaceId>'."
6. The step passes only when $.message is AWS's constant sentence for any name/space combination.
