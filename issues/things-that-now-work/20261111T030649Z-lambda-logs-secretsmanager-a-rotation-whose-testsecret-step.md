Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves
Issue-Key: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves|run#20|Timeout|current_now = sm.get_secret_value(SecretId=secret_arn)
Issue-Commit: 6e1ef7d8ebea1acbe69ee2691b3319db9dddeedf

# lambda__logs__secretsmanager: a rotation whose testSecret step fails never settles — AWSCURRENT neither stays put observably nor moves, and the check burns its whole budget

> _Note: this issue was identified against the code base at commit `6e1ef7d8ebea1acbe69ee2691b3319db9dddeedf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RotateSecret

## Description
One net-new root cause. The check is not marked ALREADY FILED and is not OWNED. The open issue 20261108T035337Z-secretsmanager-5-net-new-causes-a-deferred-rotateimmediately.md covers sibling rotation checks (the suite's own listing shows the neighbouring check failing with `InvalidRequestException: A previous rotation isn't complete. That rotation will be reattempted.`) but does not carry this replay key.

Cause 1 of 1: a rotation whose rotation function raises in the testSecret step never reaches a terminal state, so neither the refusal nor the positive control is observable.
AWS: RotateSecret drives the four-step contract createSecret -> setSecret -> testSecret -> finishSecret. If the rotation Lambda raises during testSecret, finishSecret is never invoked: the AWSCURRENT stage stays on the original version, the candidate keeps AWSPENDING, DescribeSecret reports the rotation as not having completed, and the secret remains readable at its original value — while the same function with one environment variable changed completes and promotes the candidate. The rotation therefore reaches an observable outcome in both arms. Recorded behavior for the step sequencing; Smithy: secretsmanager RotateSecret / DescribeSecret / ListSecretVersionIds expose VersionStages (AWSCURRENT/AWSPENDING) and the rotation state.
Emulator: the check is killed with `Error: Check timed out after 60.0s` — neither arm settles, so the "an untested credential is never made current" guarantee can be neither demonstrated nor refuted. Hypothesis: the rotation state machine does not drive the steps to a terminal state when a step raises (consistent with the neighbouring check being refused with `A previous rotation isn't complete. That rotation will be reattempted.`), so both the failing arm and the positive-control arm stall.
Why: this is the safety property of secret rotation — a credential that failed its test must never become AWSCURRENT. A client gets a secret stuck mid-rotation with no terminal verdict: subsequent RotateSecret calls are refused as "a previous rotation isn't complete", and no API answer tells the client whether the new credential was promoted.
Fix: the Secrets Manager rotation state machine — it must invoke the rotation function's steps in order, record a terminal outcome when a step raises (leaving AWSCURRENT on the original version and AWSPENDING on the candidate, and clearing the in-progress rotation so a later RotateSecret is accepted), and promote only after finishSecret.
Traces: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves.

## Evidence
Cause 1 — Expected: with a rotation function that raises in testSecret, AWSCURRENT stays on the original version and finishSecret is never reached (observable within the check's grace window), while the same function with one environment variable changed promotes the candidate. Actual: `Error: Check timed out after 60.0s` — neither arm reaches an observable outcome. The neighbouring rotation check in the same service family is refused with `botocore.errorfactory.InvalidRequestException: ... RotateSecret ...: A previous rotation isn't complete. That rotation will be reattempted.`, showing rotations left in progress indefinitely.

## Steps to Reproduce
1. CreateSecret with a known SecretString.
2. Create a rotation Lambda implementing createSecret/setSecret/testSecret/finishSecret whose testSecret raises (gated on one environment variable).
3. RotateSecret and poll DescribeSecret / ListSecretVersionIds for AWSCURRENT and AWSPENDING until the rotation reaches a terminal outcome.
4. Flip the environment variable and RotateSecret again as the positive control.
Observed: neither arm settles; the check is killed at its budget. Expected: the failing arm leaves AWSCURRENT on the original version with the candidate at AWSPENDING, and the control arm promotes.

## Resolution

What landed: a rotation whose function RAN a step and did not complete it now RECORDS that
terminal outcome on the secret (`Secret::rotation_stopped`, written by the driver in
`services/service-com-amazonaws-secretsmanager-secretsmanager/src/state/rotation.rs`), and
`RotateSecret` reads it: the "A previous rotation isn't complete. That rotation will be
reattempted." refusal is for a call asking for a rotation BESIDE the outstanding one, so a call
whose `ClientRequestToken` names the ended rotation's own candidate is served and RE-DRIVES that
rotation on the version already staked, minting no second version
(`src/ops/rotate_secret.rs::re_driven_candidate`). That is the "a later RotateSecret is accepted
rather than refused" half of this report, narrowed to the call the model's `@idempotencyToken`
`ClientRequestToken` identifies — a call carrying any other token still gets the refusal, which is
what the recorded trace of the sibling check
(`lambda__logs__secretsmanager.deep-secrets-and-params-failed-finishsecret-leaves-candidate-awspending-and-rejects-new-rotation`)
shows AWS answering. Pinned by
`core/router/tests/all/secretsmanager_rotation_is_re_driven_by_its_own_token.rs`, a table over
which step the earlier rotation stopped at.

What this record does NOT claim: the reported symptom — "a rotation whose testSecret step fails
never reaches a terminal state" — could not be reproduced, and the recorded check's replay verdict
is still `Timeout`. Instrumenting the emulator while the recorded check ran showed it serving every
observable this report names, in order: the rotation function is invoked once per step, its log
group holds `createSecret`, `setSecret`, `testSecret` and the `[ERROR] RuntimeError` of the step it
raised in and nothing past it, `DescribeSecret` reports `{original: [AWSCURRENT], candidate:
[AWSPENDING]}` with no `LastRotatedDate`, and `GetSecretValue` keeps serving the original version.
The check then spends the rest of its 60-second cap in a fixed grace window that re-reads that same
(correct) state every 5 seconds, so it is killed before it reaches its positive control: the
rotation settles, but the check has no budget left to say so.
