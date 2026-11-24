Issue-Source: trace-sweep-divergence
Issue-Trace: secretsmanager-rotation-configuration-and-cancel
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: secretsmanager-rotation-configuration-and-cancel secretsmanager-rotation-configuration-errors
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager|DescribeSecret|2xx->2xx|$.RotationEnabled
Issue-Commit: 26e3724e83756038c0478f8426d4bacc8a1fccc5

# secretsmanager: DescribeSecret omits RotationEnabled for a secret whose rotation was turned off, instead of reporting false

> _Note: this issue was identified against the code base at commit `26e3724e83756038c0478f8426d4bacc8a1fccc5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSecret

## Description
One cause, reached through secretsmanager.DescribeSecret in both rotation-configuration fixtures the replay reports as mismatched (neither is marked stub_path or owned_by).

Cause 1 of 1: once rotation has been explicitly turned OFF on a secret, DescribeSecret must report RotationEnabled as the boolean false; the emulator leaves the member out of the envelope altogether, so a client cannot tell "rotation was turned off" from "rotation was never configured".

AWS: Smithy secretsmanager DescribeSecretResponse$RotationEnabled -> RotationEnabledType, a boolean with no modeled default (member doc: "Specifies whether automatic rotation is turned on for this secret. If the secret has never been configured for rotation, Secrets Manager returns null. To turn on rotation, use RotateSecret. To turn off rotation, use CancelRotateSecret."). The member is therefore THREE-valued on the wire: absent for a secret rotation was never configured for, true while rotation is on, and present-and-false once a call has turned it off. CancelRotateSecret is documented as "Turns off automatic rotation, and if a rotation is currently in progress, cancels the rotation ... To turn on automatic rotation again, call RotateSecret" — the call decides the flag, so after it the flag has a value to report. Recorded behavior confirms exactly that: in both rotation-configuration fixtures the recorded DescribeSecret response for the secret whose rotation these steps turn off carries RotationEnabled = false (HTTP 200). The response is otherwise identical to the emulator's, so the only requirement at stake here is the presence and value of that one member; the surrounding ARN, name, version ids and timestamps are per-run values.

Emulator: the same DescribeSecret call answers HTTP 200 with NO RotationEnabled member at all — the replay reports "$.RotationEnabled: missing (expected False, got None)" for secretsmanager-rotation-configuration-and-cancel#task#4 and, identically, for secretsmanager-rotation-configuration-errors#task#5 (same divergence key service=secretsmanager|DescribeSecret|status=200->200|keys=$.RotationEnabled). The emulator models the member correctly as a tri-state Option<bool> and DescribeSecret projects it verbatim, so the member is absent because nothing in the exercised path ever stored a value for it. Hypothesis (the recordings fix the required OUTPUT, not the internal cause): the turn-off path refuses to materialise the flag — it only overwrites an ALREADY-SET value and leaves an unset one unset, so a CancelRotateSecret that turns rotation off on a secret whose stored rotation flag is still unset (never configured, or configured by a path that did not persist the flag) records nothing and the member stays absent. A secret for which rotation configuration was persisted and is then cancelled does report false, which is why only these two steps diverge.

Why: a client relying on AWS's behavior reads RotationEnabled's PRESENCE to distinguish "rotation is configured but currently off" from "rotation was never set up", and its VALUE to decide whether rotation is on. Against the emulator the member is absent after a turn-off, so compliance/drift reporting classifies the secret as never-configured, "enable rotation if it was never set up" remediation re-configures a secret whose rotation an operator deliberately cancelled, and SDK code that reads the boolean (boto3 renders the absent member as None rather than False) takes a None branch or raises where AWS hands it a definite False.

Fix: the Secrets Manager rotation state and its DescribeSecret projection — the turn-off path must record the flag rather than only overwrite a previously recorded one, so that any call which explicitly turns rotation off leaves RotationEnabled reportable as false while a secret no rotation call ever touched keeps it absent. DescribeSecret's own serializer already forwards the stored tri-state unchanged and needs no change if the state is recorded.

Traces: secretsmanager-rotation-configuration-and-cancel#task#4 (representative) and secretsmanager-rotation-configuration-errors#task#5. For file/line orientation: services/service-com-amazonaws-secretsmanager-secretsmanager/src/ops/describe_secret.rs:261 forwards the stored flag; src/state.rs:2804-2815 (Secret::cancel_rotation) and src/state.rs:2025-2038 (Secret::schedule_deletion) are the turn-off paths that write false only when a value is already present; src/state.rs:2725-2741 (Secret::configure_rotation) is the only path that ever sets it.

Related open issues that do NOT cover this step: 20261114T104717Z-rds-secretsmanager-2-net-new-causes-describesecret-invents.md and 20261108T102113Z-rds-secretsmanager-the-rds-managed-master-user-secret.md concern the OPPOSITE direction (RotationEnabled present-and-false on a secret rotation was NEVER configured for) and are grounded in behavioral check ids, not in these fixture steps; 20261112T222923Z-cloudformation-iam-lambda-secretsmanager-2-net-new-causes-104be0.md covers AutomaticallyAfterDays in RotationRules. A fix that simply stops emitting the member for un-configured secrets must not regress this case: the two requirements are complementary, not alternatives.

## Evidence
Replay of trace set simparity-wire-aws-v1 (26 fixtures, 216 steps, 214 passed, 2 mismatched, score 99.1%, emulator up). The single divergence cluster, verbatim:

  verdict: mismatch
  service: secretsmanager, operation: DescribeSecret
  fixture_id: secretsmanager-rotation-configuration-and-cancel, step_key: task#4
  expected_status: 200, actual_status: 200
  detail: "$.RotationEnabled: missing (expected False, got None)"
  divergence_key: service=secretsmanager|DescribeSecret|status=200->200|keys=$.RotationEnabled
  affected (2): secretsmanager-rotation-configuration-and-cancel#task#4,
                secretsmanager-rotation-configuration-errors#task#5
  stub_path: "" (not blocked by an unimplemented op), owned_by: [] (not owned by an open issue)

Expected (AWS, from the recording): HTTP 200 with RotationEnabled present and equal to false.
Actual (emulator): HTTP 200 with the RotationEnabled member absent from the response body (boto3 -> None). Every other member of both responses matched, including the status code, so the status and error code are not at issue — only the presence/value of RotationEnabled.

Model citation: smithy_query(query="doc", shape="DescribeSecretResponse", member="RotationEnabled") -> "Specifies whether automatic rotation is turned on for this secret. If the secret has never been configured for rotation, Secrets Manager returns null. To turn on rotation, use RotateSecret. To turn off rotation, use CancelRotateSecret." DescribeSecret output signature lists RotationEnabled -> RotationEnabledType with default null (i.e. no default), alongside the equally optional RotationLambdaARN / RotationRules / LastRotatedDate / NextRotationDate.

## Steps to Reproduce
Build and boot the emulator from this checkout, then replay the two fixture steps in Traces (secretsmanager-rotation-configuration-and-cancel#task#4 is the representative; secretsmanager-rotation-configuration-errors#task#5 is the same cause).

Manually, against the emulator's secretsmanager endpoint:
1. secretsmanager.create_secret(Name=<n>, SecretString='{"username":"u","password":"p"}').
2. Turn rotation OFF on it with secretsmanager.cancel_rotate_secret(SecretId=<n>) (the fixtures reach this point through the rotation-configuration sequence; the call is documented to succeed and to turn automatic rotation off).
3. secretsmanager.describe_secret(SecretId=<n>).
   AWS: HTTP 200 and the response contains RotationEnabled = false.
   Emulator: HTTP 200 and the response has no RotationEnabled key at all (boto3 renders it as None).

Control (must keep passing): a secret created in step 1 and described WITHOUT any rotation call in between must still omit RotationEnabled entirely.
