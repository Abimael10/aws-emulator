Issue-Source: trace-sweep-divergence
Issue-Trace: rds-read-replica-and-instance-promotion
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-read-replica-and-instance-promotion rds-start-stop-switchover-error-paths
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|SwitchoverReadReplica|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: e969d65a4cbd4e5e2964a789d43abfb7f6f19eda

# RDS SwitchoverReadReplica DBInstanceNotFound refusal carries an emulator-authored message

> _Note: this issue was identified against the code base at commit `e969d65a4cbd4e5e2964a789d43abfb7f6f19eda`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SwitchoverReadReplica

## Description
SwitchoverReadReplica refuses an unknown DB instance identifier with the right HTTP status (404) and the right error code, but with an error MESSAGE the emulator wrote itself rather than the message AWS words for this error class on this operation. There is ONE cause in this issue.

=== Cause 1 of 1: SwitchoverReadReplica's DBInstanceNotFoundFault message uses the emulator's generic '<Type> <id> not found.' template ===

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19): SwitchoverReadReplica input is SwitchoverReadReplicaMessage with a single member DBInstanceIdentifier (target String, @required), output SwitchoverReadReplicaResult{DBInstance}; modeled errors are DBInstanceNotFoundFault (errorKind client, httpError 404, awsQueryError code 'DBInstanceNotFound' httpResponseCode 404) and InvalidDBInstanceStateFault (400). The model does not state message text, so the wording requirement is taken from the recorded response: AWS words this operation's instance-not-found refusal "The DB instance wasn't found: <DBInstanceIdentifier>." The FIXED text is "The DB instance wasn't found: " followed by a terminating period; the only VARIABLE part is the DBInstanceIdentifier echoed verbatim from the request. (The recording's identifier is a generated probe name and is not itself part of the requirement.) Note this is NOT the same wording AWS uses for the sibling instance operations — e.g. PromoteReadReplica words it 'DB Instance not found: <id>' — so the correct text is per-operation, not one shared template.

Emulator: answers 404 with the correct error code but message "DBInstance <DBInstanceIdentifier> not found." — the emulator's crate-wide generic not-found template: resource type spelled 'DBInstance' with no space, the identifier placed mid-string instead of at the end, 'not found.' instead of "wasn't found: ", and no colon.

Why: the error message is part of the error contract. A client or operator tooling that string-matches the refusal to tell a genuinely absent read replica apart from another 404 condition, and any assertion or log scraper written against AWS's text, does not match and mis-classifies the failure. Because the emulator serves one generic template for every resource type and operation, this path will stay wrong even after other instance-not-found paths are corrected individually.

Fix: the SwitchoverReadReplica handler's instance-resolution / DBInstanceNotFoundFault refusal path in the RDS service crate — it needs this operation's own message template rather than the shared generic '<Type> <id> not found.' builder in the RDS error/validation layer. Related but NOT covering: open issue 20261114T091417Z-rds-not-found-and-switchover-refusals-carry-emulator.md bundles the same generic-template defect for PromoteReadReplica, RebootDBCluster and FailoverGlobalCluster; SwitchoverReadReplica is not among its causes and its expected wording differs from PromoteReadReplica's, so it must be fixed and verified separately.

Traces: rds-read-replica-and-instance-promotion#task#5 (representative) and rds-start-stop-switchover-error-paths#task#7 — 2 steps in the cluster, both the same cause.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false).

Divergence key: service=rds|SwitchoverReadReplica|status=404->404|keys=$.ErrorResponse.Error.Message
expected_status 404, actual_status 404 — status and error code agree; only the message diverges.

  $.ErrorResponse.Error.Message: value_mismatch
    expected: "The DB instance wasn't found: sim2real-probe-rds-rrinst-730410d1."
    actual:   'DBInstance sim2real-probe-rds-rrinst-730410d1 not found.'

Fixed text (the requirement): "The DB instance wasn't found: " and the trailing period.
Variable part (taken from the request): the DBInstanceIdentifier — here the generated probe identifier 'sim2real-probe-rds-rrinst-730410d1', which is a per-run value and NOT part of the requirement.

affected_count = 2: rds-read-replica-and-instance-promotion#task#5, rds-start-stop-switchover-error-paths#task#7.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1.
3. Replay fixture rds-read-replica-and-instance-promotion step task#5: call SwitchoverReadReplica with a DBInstanceIdentifier that does not exist in the emulator's state.
4. Assert HTTP 404, Error.Code 'DBInstanceNotFound', and Error.Message exactly "The DB instance wasn't found: <that identifier>." — with the identifier echoed verbatim from the request.
5. Repeat for fixture rds-start-stop-switchover-error-paths step task#7, which exercises the same refusal path.
6. Both steps must pass before closing.
