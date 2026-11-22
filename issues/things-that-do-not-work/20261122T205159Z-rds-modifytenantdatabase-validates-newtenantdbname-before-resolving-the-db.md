Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-tenant-database-and-restore-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-tenant-database-and-restore-error-paths rds-tenant-databases
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|ModifyTenantDatabase|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|ModifyTenantDatabase|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|ModifyTenantDatabase|4xx->4xx|status
Issue-Commit: bf94000a99dd726d2b53347e005e9132823c9563

# rds: ModifyTenantDatabase validates NewTenantDBName before resolving the DB instance, so a missing instance answers 400 InvalidParameterValue instead of 404 DBInstanceNotFound

> _Note: this issue was identified against the code base at commit `bf94000a99dd726d2b53347e005e9132823c9563`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyTenantDatabase

## Description
One cause.

Cause 1 of 1: ModifyTenantDatabase against a DB instance identifier that does not exist is answered with a parameter-validation error on NewTenantDBName instead of the modeled not-found fault, because the emulator runs its NewTenantDBName check before it resolves DBInstanceIdentifier against state.

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models ModifyTenantDatabase with required members DBInstanceIdentifier and TenantDBName and the optional member NewTenantDBName, whose target is the unconstrained `String` shape - the model places NO @length bound on NewTenantDBName at all. Its modeled errors are DBInstanceNotFoundFault (awsQueryError code `DBInstanceNotFound`, httpResponseCode 404), TenantDatabaseNotFoundFault (`TenantDatabaseNotFound`, 404), InvalidDBInstanceStateFault, KMSKeyNotAccessibleFault and TenantDatabaseAlreadyExistsFault; `InvalidParameterValue` is not among them for this condition. The recorded response shows AWS resolving the instance first: HTTP 404 with an awsQuery ErrorResponse carrying Error.Code `DBInstanceNotFound` and Error.Message `DBInstance not found: <DBInstanceIdentifier>`. In that message `DBInstance not found: ` is the fixed text and the trailing token is the DBInstanceIdentifier taken verbatim from the request (the recording's `sim2real-probe-rds-dbinst-missing-1cd648d7` is one run's value filling that slot, not part of the requirement).

Emulator: returns HTTP 400 with Error.Code `InvalidParameterValue` and Error.Message `The parameter NewTenantDBName must contain from 1 to 8 characters.` - i.e. it enforces an 8-character ceiling on NewTenantDBName that the model does not state, and it enforces it ahead of any lookup of DBInstanceIdentifier, so the request never reaches the point where the missing instance would be reported.

Why: a client that probes for a tenant database's parent instance, or that retries on not-found, cannot distinguish "no such DB instance" from "bad request". SDK error handling keys off the error code and the HTTP status: DBInstanceNotFound is a 404 retry/branch signal that callers use to create-or-modify, while InvalidParameterValue is a non-retryable 400 that callers surface to the user as a malformed request. Control-plane scripts therefore abort with a bogus complaint about a parameter they supplied legitimately, and never learn the instance is absent. The invented 1-to-8-character rule additionally rejects NewTenantDBName values that real RDS accepts.

Fix: ModifyTenantDatabase's handler and its request validation in the RDS service crate. Resolve DBInstanceIdentifier against state and raise DBInstanceNotFoundFault (404, code `DBInstanceNotFound`, message `DBInstance not found: <DBInstanceIdentifier>`) before any NewTenantDBName validation runs, and drop the unmodeled 1-to-8-character length rule on NewTenantDBName (or move it behind resource resolution and give it the error class the model permits).

Traces: rds-db-tenant-database-and-restore-error-paths#task#3 (representative) and rds-tenant-databases#task#3, both in the same cluster `service=rds|ModifyTenantDatabase|status=404->400`.

No already-open issue covers this: the open RDS issues name DB-cluster/global-cluster envelope hosts, ports and the DescribeDBEngineVersions catalog, not ModifyTenantDatabase's error ordering.

## Evidence
Fixture rds-db-tenant-database-and-restore-error-paths, step task#3 (cluster also covers rds-tenant-databases#task#3), operation ModifyTenantDatabase.

Expected (recorded AWS response):
  HTTP 404
  $.ErrorResponse.Error.Code    = 'DBInstanceNotFound'
  $.ErrorResponse.Error.Message = 'DBInstance not found: <DBInstanceIdentifier from the request>'
                                  (recorded run: 'DBInstance not found: sim2real-probe-rds-dbinst-missing-1cd648d7')

Actual (emulator):
  HTTP 400
  $.ErrorResponse.Error.Code    = 'InvalidParameterValue'
  $.ErrorResponse.Error.Message = 'The parameter NewTenantDBName must contain from 1 to 8 characters.'

Replay detail verbatim:
  status: expected 404, got 400
  $.ErrorResponse.Error.Code: value_mismatch (expected 'DBInstanceNotFound', got 'InvalidParameterValue')
  $.ErrorResponse.Error.Message: value_mismatch (expected 'DBInstance not found: sim2real-probe-rds-dbinst-missing-1cd648d7', got 'The parameter NewTenantDBName must contain from 1 to 8 characters.')

Model evidence: ModifyTenantDatabase.NewTenantDBName targets the unconstrained `String` shape (no @length); ModifyTenantDatabase errors include DBInstanceNotFoundFault with awsQueryError {code: DBInstanceNotFound, httpResponseCode: 404} and do not include InvalidParameterValue.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an RDS (awsQuery) client at its data listener.
2. Call ModifyTenantDatabase with a DBInstanceIdentifier that does not exist in emulator state, any TenantDBName, and a NewTenantDBName longer than 8 characters (as fixture rds-db-tenant-database-and-restore-error-paths step task#3 does).
3. Observe HTTP 400 with ErrorResponse/Error/Code = InvalidParameterValue and Message 'The parameter NewTenantDBName must contain from 1 to 8 characters.'.
4. Expected: HTTP 404 with ErrorResponse/Error/Code = DBInstanceNotFound and Message 'DBInstance not found: <the DBInstanceIdentifier sent>'.
5. Re-replay fixtures rds-db-tenant-database-and-restore-error-paths#task#3 and rds-tenant-databases#task#3 to confirm.
