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
