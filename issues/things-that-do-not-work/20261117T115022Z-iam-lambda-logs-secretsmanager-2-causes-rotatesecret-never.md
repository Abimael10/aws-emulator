Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__secretsmanager.rotate-secret-duration-and-cron-window-boundaries iam__lambda__logs__secretsmanager.rotate-secret-duration-exceeding-the-schedule-window-rejected
Issue-Key: iam__lambda__logs__secretsmanager.rotate-secret-duration-and-cron-window-boundaries|_refusal#26|AssertionError|raise AssertionError(f"expected a refusal, but the call SUCCEEDED and returned {result!r}")
Issue-Key: iam__lambda__logs__secretsmanager.rotate-secret-duration-exceeding-the-schedule-window-rejected|_expect_error#11|AssertionError|assert got == expected, f"expected refusal {expected!r}, got {got!r}"
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# iam__lambda__logs__secretsmanager: 2 causes — RotateSecret never validates the RotationRules Duration/cron window, and the missing-rotation-function refusal pre-empts that validation

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RotateSecret

## Description
Two net-new root causes in Secrets Manager's RotateSecret request validation.

Cause 1 of 2: a RotationRules window AWS refuses is accepted.
AWS: Smithy secretsmanager RotateSecret models RotationRules and binds InvalidParameterException (client, HTTP 400); the documented schedule rules (https://docs.aws.amazon.com/secretsmanager/latest/userguide/rotate-secrets_schedule.html) bound Duration by the schedule, and each violation has its own fixed message — 'Duration cannot be greater than Rate in hour.' when Duration exceeds the rate() interval, 'Duration must be at least 1 hour, at most 24 hour.' when it is outside those bounds, and 'Starting hour and duration must be within a day.' when a cron() start hour plus Duration would cross the UTC day. A refused call changes nothing: RotationEnabled stays false, no RotationRules are stored and no NextRotationDate is scheduled.
Emulator: the call that must be refused SUCCEEDS — RotateSecret answers 200 with ARN/Name/VersionId, so an out-of-bounds rotation window is stored.
Why: an invalid rotation window is silently accepted, so a schedule AWS would never run looks configured; a client relying on the refusal to validate operator input gets none.
Fix: the RotateSecret handler's RotationRules validation (Duration against rate()/AutomaticallyAfterDays and the cron() start-hour + duration day bound), each raising InvalidParameterException with its message and leaving the secret untouched.
Failing check: iam__lambda__logs__secretsmanager.rotate-secret-duration-and-cron-window-boundaries

Cause 2 of 2: the refusal order is wrong — a parameter error is reported as a missing rotation function.
AWS: for a secret with no rotation configured, a RotateSecret carrying only RotationRules with an invalid Duration is refused on the PARAMETER: InvalidParameterException 'Duration cannot be greater than Rate in hour.' (parameter validation precedes the check that a rotation Lambda is associated).
Emulator: the same call is refused InvalidRequestException 'No Lambda rotation function ARN is associated with this secret.' — a different error class and a different rule, so the parameter error is never surfaced.
Why: the caller is told to attach a rotation function when the actual defect is its Duration; after attaching one it hits the same request again and gets a second, different error. A client branching on InvalidParameterException never sees it.
Fix: the RotateSecret handler's validation ordering — validate RotationRules before the rotation-function association check.
Failing check: iam__lambda__logs__secretsmanager.rotate-secret-duration-exceeding-the-schedule-window-rejected

## Evidence
Cause 1: [mismatch] iam__lambda__logs__secretsmanager.rotate-secret-duration-and-cron-window-boundaries — AssertionError: 'expected a refusal, but the call SUCCEEDED and returned {ARN..., Name..., VersionId..., HTTPStatusCode 200}'. The check's required refusals are ('InvalidParameterException', 'Duration cannot be greater than Rate in hour.'), ('InvalidParameterException', 'Duration must be at least 1 hour, at most 24 hour.') and ('InvalidParameterException', 'Starting hour and duration must be within a day.').

Cause 2: [mismatch] iam__lambda__logs__secretsmanager.rotate-secret-duration-exceeding-the-schedule-window-rejected — AssertionError: expected refusal ('InvalidParameterException', 'Duration cannot be greater than Rate in hour.'), got ('InvalidRequestException', 'No Lambda rotation function ARN is associated with this secret.') for RotateSecret(SecretId=..., RotationRules={'ScheduleExpression': 'rate(4 hours)', 'Duration': '20h'}).

## Steps to Reproduce
1. CreateSecret; call RotateSecret with RotationRules={'ScheduleExpression': 'rate(4 hours)', 'Duration': '20h'} (no RotationLambdaARN) and compare the error class/message with InvalidParameterException 'Duration cannot be greater than Rate in hour.'; repeat with {'AutomaticallyAfterDays': 5, 'Duration': '25h'}.
2. With a rotation Lambda attached, call RotateSecret(RotateImmediately=False) with a cron() start hour whose Duration crosses the UTC day (e.g. cron(0 22 * * ? *) + 4h) and with a Duration over the rate() interval; each must be refused and must leave RotationEnabled/RotationRules/NextRotationDate unchanged.
