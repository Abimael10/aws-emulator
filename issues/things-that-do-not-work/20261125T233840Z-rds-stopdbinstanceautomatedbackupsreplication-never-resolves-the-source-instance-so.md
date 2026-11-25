Issue-Source: trace-sweep-divergence
Issue-Trace: rds-automated-backups-replication-errors
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-automated-backups-replication-errors rds-read-replica-and-instance-promotion
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|StopDBInstanceAutomatedBackupsReplication|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|StopDBInstanceAutomatedBackupsReplication|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|StopDBInstanceAutomatedBackupsReplication|4xx->4xx|status
Issue-Commit: c11f37d9666aeec66c301c48be9695cec998b743

# rds: StopDBInstanceAutomatedBackupsReplication never resolves the source instance, so a nonexistent ARN gets 400 InvalidDBInstanceState instead of 404 DBInstanceNotFound

> _Note: this issue was identified against the code base at commit `c11f37d9666aeec66c301c48be9695cec998b743`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopDBInstanceAutomatedBackupsReplication

## Description
One cause (Cause 1 of 1), from the only wire divergence cluster in this replay that no open issue owns.

=== Cause 1 of 1: the existence of the source DB instance is never checked, so the not-found fault is unreachable and every unresolvable stop is answered as a state fault ===

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models StopDBInstanceAutomatedBackupsReplication with input member SourceDBInstanceArn (String, required) and exactly two errors — DBInstanceNotFoundFault, awsQueryError code `DBInstanceNotFound`, httpError 404 ("DBInstanceIdentifier doesn't refer to an existing DB instance"), and InvalidDBInstanceStateFault, awsQueryError code `InvalidDBInstanceState`, httpError 400 ("The DB instance isn't in a valid state"). The two faults are ordered: AWS resolves the instance named by the ARN FIRST, in the SOURCE region the ARN names, and only judges the replication state of an instance that exists. Recorded behavior for the wording: when the ARN names an instance that does not exist, the response is HTTP 404 with Error.Code `DBInstanceNotFound` and Error.Message `Requested DB Instance <db-instance-identifier> not found in source region <source-region>`. Fixed text: `Requested DB Instance ` and ` not found in source region `, with NO trailing period. Variable parts: the DB instance identifier taken from the resource part of the request's SourceDBInstanceArn, and the region taken from the region field of that same ARN. The 400/InvalidDBInstanceState refusal (message `DB Instance <source-arn> is not replicating to the current region.`) is for an instance that EXISTS but has no replication into the region of the call; it is not the answer for an identifier that resolves to nothing.

Emulator: the handler resolves nothing but the replication record. It maps the ARN to a source, then looks up `replicated_instance_backup_from(destination, source)` and, whenever that is absent or not replicating, returns InvalidDBInstanceStateFault — HTTP 400, Error.Code `InvalidDBInstanceState`, Error.Message `DB Instance <source-arn> is not replicating to the current region.` Its own module doc states the design explicitly: "the source instance is never looked up, so every call that names no replication into this Region — one never enabled, one already stopped, one made in another Region, and one whose ARN names an instance that does not exist — is refused the SAME modelled InvalidDBInstanceStateFault." As a result DBInstanceNotFoundFault is dead code for this operation and the status class is wrong (400 where AWS answers 404), the error code is wrong, and the message is a different sentence entirely (fixed text `DB Instance ` / ` is not replicating to the current region.`, variable part the whole ARN, with a trailing period).

Why: a client cannot tell "this ARN names no such DB instance" from "this instance exists but is not replicating here". The two cases have different remedies — fix the identifier/region in the ARN, versus enable replication or call in the right destination region — and AWS separates them by both status class (404 vs 400) and error code. SDK retry/classification layers and error-handling code that branch on DBInstanceNotFound (or on a 404 generally) never see it, so a typo'd or stale instance identifier is reported as a state problem; cleanup and teardown paths that tolerate a missing instance by catching DBInstanceNotFound instead propagate a hard InvalidDBInstanceState failure.

Fix: the StopDBInstanceAutomatedBackupsReplication handler must resolve the source DB instance from SourceDBInstanceArn in the source region the ARN names BEFORE it judges replication state, and raise DBInstanceNotFoundFault (404, code DBInstanceNotFound) with this operation's source-region message when that instance does not exist; the existing InvalidDBInstanceStateFault branch must then apply only to an instance that exists and has no active replication into the region of the call. That ordering belongs in the operation's handler/validation, with the message built by the same state-level message helper used for the Start leg's source-region not-found sentence.

Related but NOT covering this: open issue 20261120T222703Z-rds-2-net-new-causes-startdbinstanceautomatedbackupsreplication-s-dbinstancenotfound.md is about the START leg's DBInstanceNotFound MESSAGE TEXT, and in its prose it asserts the Stop leg should answer InvalidDBInstanceState for the same ghost ARN — which is what the emulator does today and what the recorded wire fixture contradicts. This issue is the Stop leg's status/code/precedence defect and carries its own replay key; it is not closed by fixing the Start leg's wording.

Traces: rds-automated-backups-replication-errors#task#2 (representative) and rds-read-replica-and-instance-promotion#task#8, both in the same divergence cluster `service=rds|StopDBInstanceAutomatedBackupsReplication|status=404->400`. Handler: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/stop_db_instance_automated_backups_replication.rs:193-208 (the `replicating` lookup and its InvalidDBInstanceStateFault branch), message helpers in services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs (`backup_replication_not_to_this_region_message`).

## Evidence
Cluster service=rds|StopDBInstanceAutomatedBackupsReplication|status=404->400|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status (2 affected steps).

Request: StopDBInstanceAutomatedBackupsReplication(SourceDBInstanceArn="arn:aws:rds:<source-region>:<account>:db:<identifier-that-does-not-exist>").

Expected (recorded):
  HTTP 404
  $.ErrorResponse.Error.Code    = "DBInstanceNotFound"
  $.ErrorResponse.Error.Message = "Requested DB Instance <identifier> not found in source region <source-region>"
  (fixed text "Requested DB Instance " + " not found in source region "; variables = the identifier and the region parsed from the request ARN; no trailing period)

Actual (emulator):
  HTTP 400
  $.ErrorResponse.Error.Code    = "InvalidDBInstanceState"
  $.ErrorResponse.Error.Message = "DB Instance <source-arn> is not replicating to the current region."

Verbatim replay detail for the representative step rds-automated-backups-replication-errors#task#2:
  status: expected 404, got 400
  $.ErrorResponse.Error.Code: value_mismatch (expected 'DBInstanceNotFound', got 'InvalidDBInstanceState')
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Requested DB Instance nonexistent-db-probe-x9z not found in source region us-east-1', got 'DB Instance arn:aws:rds:us-east-1:000000000000:db:nonexistent-db-probe-x9z is not replicating to the current region.')
(the identifier, region, account and ARN above are that run's values filling the template, not part of the requirement)

Model evidence: smithy_query(query="operation", operation="StopDBInstanceAutomatedBackupsReplication") returns errors [DBInstanceNotFoundFault: httpError 404, awsQueryError code DBInstanceNotFound; InvalidDBInstanceStateFault: httpError 400, awsQueryError code InvalidDBInstanceState].

Code evidence: the handler's own module doc (lines 13-17) states the source instance is never looked up and that an ARN naming a nonexistent instance is refused the same InvalidDBInstanceStateFault as one not replicating here, so the modeled 404 is never emitted.

## Steps to Reproduce
1. Boot the emulator and point an rds client at it (region us-east-1 in the fixture, any region works).
2. Without creating any DB instance, call StopDBInstanceAutomatedBackupsReplication with SourceDBInstanceArn="arn:aws:rds:<region>:<account>:db:<identifier-that-does-not-exist>".
3. Observe HTTP 400 with Error.Code "InvalidDBInstanceState" and Error.Message "DB Instance <arn> is not replicating to the current region." instead of HTTP 404 with Error.Code "DBInstanceNotFound" and Error.Message "Requested DB Instance <identifier> not found in source region <region>".
4. Regression guard for the branch that must stay: create a DB instance, do NOT start backup replication into this region, then call StopDBInstanceAutomatedBackupsReplication with that instance's ARN — this one must still be HTTP 400 InvalidDBInstanceState with "DB Instance <arn> is not replicating to the current region."
5. Re-replay wire fixtures rds-automated-backups-replication-errors (step task#2) and rds-read-replica-and-instance-promotion (step task#8).
