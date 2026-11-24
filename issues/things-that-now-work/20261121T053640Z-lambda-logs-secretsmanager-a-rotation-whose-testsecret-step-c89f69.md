Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves
Issue-Key: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves|run#20|Timeout|time.sleep(5.0)
Issue-Commit: a3f33f7e41c0f82edda298cff5ad8a1ec31b20ba

# lambda__logs__secretsmanager: a rotation whose testSecret step raises still never reaches a terminal state — the failing arm and its positive control both stall and the check burns its whole budget

> _Note: this issue was identified against the code base at commit `a3f33f7e41c0f82edda298cff5ad8a1ec31b20ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RotateSecret

## Description
Cause 1 of 1: RotateSecret against a rotation Lambda that raises in the testSecret step never produces an observable terminal outcome, and neither does the identical function with the failure flag cleared, so the safety property "an untested credential is never made current" can be neither demonstrated nor refuted.

AWS: RotateSecret drives the four-step rotation contract createSecret -> setSecret -> testSecret -> finishSecret against the rotation Lambda. If the function raises during testSecret, finishSecret is never invoked: the AWSCURRENT stage stays on the original version, the candidate keeps AWSPENDING, GetSecretValue still returns the original SecretString, and DescribeSecret / ListSecretVersionIds report that mapping via VersionIdsToStages — and the in-progress rotation is cleared so a later RotateSecret is accepted rather than refused. The same function with only the FAIL_ON_TEST environment variable cleared completes and promotes the candidate to AWSCURRENT. Smithy: secretsmanager RotateSecret, DescribeSecret and ListSecretVersionIds expose VersionStages AWSCURRENT/AWSPENDING; step sequencing is recorded behavior only.

Emulator: the check is killed with 'Error: Check timed out after 60.0s'. Neither arm settles: no stage movement is ever observable for the failing arm and the control arm never promotes, so no API answer tells a client whether the candidate became current.

Why: this is the safety guarantee of secret rotation. A client is left with a secret stuck mid-rotation and no terminal verdict — it cannot tell whether the untested credential was promoted, and a subsequent RotateSecret is liable to be refused as 'A previous rotation isn't complete. That rotation will be reattempted.' instead of being accepted.

Fix: the Secrets Manager rotation state machine — it must invoke the rotation function's steps in order, record a terminal outcome when a step raises (AWSCURRENT left on the original version, AWSPENDING on the candidate, the in-progress rotation cleared), and promote only after finishSecret returns. Related open issues, neither of which carries this replay key: 20261111T030649Z-lambda-logs-secretsmanager-a-rotation-whose-testsecret-step.md (same check, recorded at an earlier step key) and 20261121T023519Z-lambda-logs-secretsmanager-rotatesecret-re-driven-with-the.md (the re-driven-token refusal in the same family).

Traces: lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves.

## Evidence
Expected: with a rotation function that raises in testSecret, AWSCURRENT stays on the original version, the candidate holds AWSPENDING and finishSecret is never reached — observable through DescribeSecret/ListSecretVersionIds/GetSecretValue within the grace window — while the same function with one environment variable changed promotes the candidate. Actual: 'Error: Check timed out after 60.0s' — neither arm reaches an observable outcome, so no stage mapping and no rotation verdict is ever produced.

## Steps to Reproduce
1. CreateSecret with a known SecretString.
2. Deploy one rotation Lambda source twice, differing only in a FAIL_ON_TEST environment variable, implementing createSecret/setSecret/testSecret/finishSecret where testSecret raises when the flag is set; grant secretsmanager.amazonaws.com lambda:InvokeFunction on both.
3. RotateSecret with the failing function and poll DescribeSecret / ListSecretVersionIds / GetSecretValue for a terminal stage mapping.
4. RotateSecret with the passing function as the positive control and poll for promotion to AWSCURRENT.
Observed: neither arm settles and the check is killed at its budget. Expected: the failing arm keeps AWSCURRENT on the original version with the candidate at AWSPENDING; the control arm promotes.
5. Replay lambda__logs__secretsmanager.rotation-with-a-failing-testsecret-step-rejected-and-awscurrent-never-moves.

## Resolution

What this record claims, and what it does not, is decided by a MEASUREMENT of the recorded check
against this emulator rather than by an argument: every call it makes was traced, with the instant
it arrived, while it ran.

The reported cause could not be reproduced: the rotation reaches its terminal outcome, and the
check's own reading paths report that outcome, 2.4 seconds after `RotateSecret` answers. The traced
run, in seconds from the check's first call:

- 0.0 `CreateSecret`.
- 0.0 `CreateFunction` (the failing twin); the check polls `GetFunctionConfiguration` at 0.1
  (`Pending`) and 2.1 (`Active`).
- 2.1 `CreateFunction` (the passing twin); polled at 2.1 (`Pending`) and 4.1 (`Active`).
- 4.1 `AddPermission` and `GetPolicy` on both functions, then `RotateSecret` — answered 200 with
  the candidate's id, `AWSPENDING` staked on it, after reading the function's own policy.
- 4.4 the first invocation (`createSecret`), whose handler reads the staged version and writes the
  candidate with `PutSecretValue` at 6.4; 6.5 the second (`setSecret`) and the third
  (`testSecret`), which raises.
- 7.2 the check's poll of the function's default log group reports exactly the three records its
  handler printed — `createSecret`, `setSecret`, `testSecret` — beside the `[ERROR] RuntimeError`,
  `END` and `REPORT` of the invocation that raised, and nothing past the step it stopped at.

From 7.2 seconds on, the check is inside a loop of its own that reads the SAME two things every
5.0 seconds — one `FilterLogEvents` page of that group selected by the term its handler prints
(three records, plus the continuation page that correctly reports no further record), and one
`GetSecretValue` naming no version (the original version, the original string) — and it is still
in that loop, having read that unchanged and correct state eleven times, when the runner's 60-second
cap kills it at its `time.sleep(5.0)`. It never reaches its positive control: the check makes no
second `RotateSecret` and no `CancelRotateSecret` at all, which was confirmed by refusing each of
those two operations in turn and seeing the verdict unchanged. So the check's own fixed watch
window, on top of a setup whose spans are its own poll intervals, exceeds the cap whatever this
emulator answers, and no state this emulator could reach ends that loop.

Two halves of the report are therefore declined as not defects, each against evidence:

- "no stage movement is ever observable for the failing arm" — `AWSPENDING` is staked on the
  candidate by `RotateSecret` itself and the candidate is created by the function's own
  `createSecret` step, both inside 2.4 s; `DescribeSecret` reports
  `{original: [AWSCURRENT], candidate: [AWSPENDING]}` with no `LastRotatedDate`, and
  `GetSecretValue` keeps serving the original.
- "the in-progress rotation is cleared so a later `RotateSecret` is accepted rather than refused" —
  the sibling recording
  `lambda__logs__secretsmanager.deep-secrets-and-params-failed-finishsecret-leaves-candidate-awspending-and-rejects-new-rotation`
  shows AWS REFUSING a later rotation carrying a new token in exactly this state, which is what the
  model states ("If the AWSPENDING staging label is present but not attached to the same version as
  AWSCURRENT, then any later invocation of RotateSecret assumes that a previous rotation request is
  still in progress and returns an error") and what this emulator answers. Serving that call instead
  was tried against the recorded check and changed nothing, because the check never makes it.

What landed is the pin for the rule the report is about, taken through the paths the recorded check
itself reads it by, which no suite covered:
`core/router/tests/all/secretsmanager_failed_rotation_read_through_a_clients_own_paths.rs` — a
table over the FORM a client spells the `filterPattern` selecting its handler's records in (a bare
term, a quoted string, a `?` alternative, a `%…%` expression, no pattern at all, and a term no
record carries), the pagination of that read, the staging map, the plain `GetSecretValue` held over
a window of looks, and the twin with its failure flag cleared completing the protocol and promoting
the candidate.

