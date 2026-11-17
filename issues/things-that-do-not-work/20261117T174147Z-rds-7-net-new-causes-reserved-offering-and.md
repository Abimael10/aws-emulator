Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.create-db-capacity-reservation-name-and-engine-validation-rejected rds.db-proxy-target-group-idle-above-max-percent-rejected rds.describe-option-groups-major-engine-version-without-engine-name-rejected rds.global-cluster-id-filter-does-not-narrow rds.modify-instance-engine-version-downgrade-rejected rds.modify-instance-pending-engine-version-target-queued-and-withdrawn rds.reserved-db-instance-offering-not-found-rejected rds.switchover-blue-green-deployment-unknown-id-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.create-db-capacity-reservation-name-and-engine-validation-rejected|run#8|AssertionError|assert err_name.get("Message") == expected_name_msg, err_name.get("Message")
Issue-Key: rds.db-proxy-target-group-idle-above-max-percent-rejected|run#14|AssertionError|assert code == "InvalidParameterCombination", f"unexpected error code {code!r}: {message!r}"
Issue-Key: rds.describe-option-groups-major-engine-version-without-engine-name-rejected|run#7|AssertionError|assert (code, message) == _NEEDS_ENGINE_NAME, (code, message)
Issue-Key: rds|DescribeGlobalClusters|InvalidParameterValue
Issue-Key: rds|CreateDBInstance|InvalidParameterCombination
Issue-Key: rds.reserved-db-instance-offering-not-found-rejected|run#4|AssertionError|assert code == ERR_OFFERING_NOT_FOUND, f"unexpected error code: {code!r}"
Issue-Key: rds.switchover-blue-green-deployment-unknown-id-rejected|run#5|AssertionError|assert code == ERR_NOT_FOUND, f"unexpected error code: {code!r}"
Issue-Commit: cd00ce55523bfa99c95686848d372910b65cd86b

# rds: 7 net-new causes — reserved-offering and blue/green switchover answer NotImplemented, the postgres engine-version catalog lacks published minors, DescribeGlobalClusters refuses an unknown filter name, and three refusals are mis-coded or misworded

> _Note: this issue was identified against the code base at commit `cd00ce55523bfa99c95686848d372910b65cd86b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReservedDBInstancesOfferings

## Description
Seven independent defects on the RDS control plane, all grounded in failing behavioral checks under the suite heading `### rds`. Seven causes; all seven must be closed before submitting.

=== Cause 1 of 7: a pattern-constraint violation message carries the regex anchors AWS strips ===
AWS: Smithy `CreateDBCapacityReservationRequest.DBCapacityReservationName` is `@required`, `@length(min:1,max:63)`, `@pattern("^[a-z](?:-?[a-z0-9]+){0,15}$")`. A value violating that pattern is refused with code `InvalidParameterValue`, HTTP 400, and the standard constraint-violation message `1 validation error detected: Value '<value>' at '<member name in lowerCamelCase>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>` where AWS renders `<pattern>` WITHOUT the leading `^` and trailing `$`. Fixed text: `1 validation error detected: Value '`, `' at '`, `' failed to satisfy constraint: Member must satisfy regular expression pattern: `; the value and member name come from the request and the pattern from the model.
Emulator: emits the same message but with the anchors included — `… regular expression pattern: ^[a-z](?:-?[a-z0-9]+){0,15}$` where AWS emits `… regular expression pattern: [a-z](?:-?[a-z0-9]+){0,15}`.
Why: a client matching AWS's exact validation message (or surfacing it to a user as the pattern to satisfy) gets a different string, and the advertised pattern is not the one AWS advertises.
Fix: the shared constraint-violation message formatter used by RDS request validation — strip the `^`/`$` anchors when rendering a `@pattern` into the message. Applies to every member with an anchored pattern, not just this one.
Traces: rds.create-db-capacity-reservation-name-and-engine-validation-rejected.

=== Cause 2 of 7: ModifyDBProxyTargetGroup refuses MaxIdleConnectionsPercent > MaxConnectionsPercent with the wrong code and its own wording ===
AWS: Smithy `ConnectionPoolConfiguration.MaxIdleConnectionsPercent` documents the constraint "Must be between 0 and the value of MaxConnectionsPercent" and `MaxConnectionsPercent` "Must be between 1 and 100". When the pair violates that relation RDS refuses the whole call with code `InvalidParameterCombination` (HTTP 400) and message `MaxConnectionsPercent <max> must be greater than MaxIdleConnectionsPercent <idle>` — fixed text `MaxConnectionsPercent ` and ` must be greater than MaxIdleConnectionsPercent `, with both numbers taken from the request. The refusal is atomic: every other pool field in the same ConnectionPoolConfig (ConnectionBorrowTimeout, InitQuery, SessionPinningFilters) keeps its prior value.
Emulator: refuses with the WRONG code `InvalidParameterValue` and an emulator-authored message `Invalid value 80 for MaxIdleConnectionsPercent. Must be between 0 and 73.` — it states the rule as a range against the other member rather than AWS's comparison wording.
Why: a client that branches on `InvalidParameterCombination` (the documented code for a cross-member conflict) does not recognise the refusal at all and treats a conflicting pool config as an unknown value error.
Fix: the ModifyDBProxyTargetGroup handler's ConnectionPoolConfig validation — classify this as a parameter combination and emit AWS's message template.
Traces: rds.db-proxy-target-group-idle-above-max-percent-rejected.

=== Cause 3 of 7: DescribeOptionGroups' MajorEngineVersion-without-EngineName refusal is mis-coded and misworded ===
AWS: Smithy `DescribeOptionGroupsMessage.MajorEngineVersion` documents "Filters the list of option groups to only include groups associated with a specific database engine version. If specified, then EngineName must also be specified." Supplying MajorEngineVersion alone is refused with code `InvalidParameterCombination` (HTTP 400) and message `If MajorEngineVersion is specified, then EngineName must also be specified.` — entirely fixed text, no request-derived parts.
Emulator: refuses with code `ValidationException` and the emulator-authored message `MajorEngineVersion was specified without EngineName: if MajorEngineVersion is specified, then EngineName must also be specified` (restated prefix, lowercased clause, no terminating period).
Why: `ValidationException` is not an error RDS's query protocol produces for this operation, so a client's error handling for parameter-combination failures never fires and the message does not match AWS's.
Fix: the DescribeOptionGroups handler's parameter validation.
Traces: rds.describe-option-groups-major-engine-version-without-engine-name-rejected.

=== Cause 4 of 7: DescribeGlobalClusters rejects an unrecognised filter name instead of ignoring it ===
AWS: Smithy `DescribeGlobalClustersMessage.Filters` documents that the only supported filter is `region`; the generic `Filter` shape's doc lists the describe operations that honour filters and DescribeGlobalClusters is not among them. Recorded behavior: a filter whose Name is not supported — e.g. `global-cluster-id` — is ACCEPTED, the call answers HTTP 200, and the filter does not narrow the result: an owned global cluster is returned both when the filter names its own identifier and when it names an identifier that was never created. The `GlobalClusterIdentifier` parameter, by contrast, matches exactly and faults with `GlobalClusterNotFoundFault` / `Global cluster '<id>' not found` for an identifier that does not exist.
Emulator: the call is refused — `InvalidParameterValue: Unrecognized filter name: global-cluster-id` — so the whole flow aborts on the first filtered describe.
Why: a client passing a filter AWS tolerates gets a hard failure instead of an unnarrowed list; paginated inventory code that attaches a filter defensively breaks outright.
Fix: the DescribeGlobalClusters handler's filter handling — accept and ignore unsupported filter names rather than refusing (and keep the exact-match/fault behavior of the GlobalClusterIdentifier parameter).
Traces: rds.global-cluster-id-filter-does-not-narrow.

=== Cause 5 of 7: the postgres engine-version catalog lacks published minor versions, so CreateDBInstance refuses them ===
AWS: `CreateDBInstance` with `Engine=postgres` and a published RDS for PostgreSQL `EngineVersion` succeeds; the version is also listed by DescribeDBEngineVersions for that engine. Recorded: versions `13.23` and `14.20` are accepted. A version RDS genuinely does not offer is refused with `InvalidParameterCombination` / `Cannot find version <version> for <engine>` (fixed text `Cannot find version ` and ` for `, with the version and engine from the request) — so the message itself is right, but it is being produced for versions AWS does serve.
Emulator: `CreateDBInstance` is refused for both — `InvalidParameterCombination: Cannot find version 14.20 for postgres` and `InvalidParameterCombination: Cannot find version 13.23 for postgres` — so two engine-version flows never get past setup.
Why: a client cannot create a PostgreSQL instance pinned to a current minor version, and every downstream behavior that depends on it (engine-version downgrade refusal, a queued `PendingModifiedValues.EngineVersion` target and its withdrawal) is unreachable. Expected downstream behaviors recorded for AWS: a downgrade request is refused with `InvalidParameterCombination` / `Cannot upgrade postgres from <from> to <to>`; a major target with AllowMajorVersionUpgrade queues in `PendingModifiedValues.EngineVersion` while the live `EngineVersion` stays current, and resubmitting the current version withdraws it.
Fix: the engine-version catalog seed data behind DescribeDBEngineVersions and the version-existence check in the CreateDBInstance handler. A related catalog defect on a different check is tracked by open issue 20261111T194847Z-rds-7-net-new-causes-cluster-writer-reader.md, which does not cover these two checks.
Traces: rds.modify-instance-engine-version-downgrade-rejected, rds.modify-instance-pending-engine-version-target-queued-and-withdrawn.

=== Cause 6 of 7: the reserved-DB-instance offering surface answers NotImplemented ===
AWS: `DescribeReservedDBInstancesOfferings` serves AWS's published offering catalog — an unfiltered call returns at least one offering and a real `ReservedDBInstancesOfferingId` describes on its own. For an offering id that was never issued, both `DescribeReservedDBInstancesOfferings` and `PurchaseReservedDBInstancesOffering` are refused with Smithy error `ReservedDBInstancesOfferingNotFoundFault` — awsQueryError code `ReservedDBInstancesOfferingNotFound`, HTTP 404 — and message `ReservedDBInstanceOffering <offering id> not found.` `DescribeReservedDBInstances` with a never-purchased reservation id is refused with `ReservedDBInstanceNotFound` and message `ReservedDBInstance <reservation id> not found.` In both messages the leading noun and the trailing ` not found.` are fixed text and the identifier comes from the request.
Emulator: the first call already answers error code `NotImplemented`, so neither the catalog nor any of the three refusals exists.
Why: a client enumerating or purchasing reserved capacity gets an error code that is not in the RDS model at all, cannot distinguish "no such offering" from "service broken", and cannot read the offering catalog.
Fix: implement the DescribeReservedDBInstancesOfferings, PurchaseReservedDBInstancesOffering and DescribeReservedDBInstances handlers (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/) over a seeded offering catalog in RDS state, with the three not-found refusals above. No open issue names these operations.
Traces: rds.reserved-db-instance-offering-not-found-rejected.

=== Cause 7 of 7: SwitchoverBlueGreenDeployment answers NotImplemented instead of BlueGreenDeploymentNotFoundFault ===
AWS: Smithy error `BlueGreenDeploymentNotFoundFault` — errorKind client, httpError 404, awsQueryError code `BlueGreenDeploymentNotFoundFault`, documented as "`BlueGreenDeploymentIdentifier` doesn't refer to an existing blue/green deployment." `SwitchoverBlueGreenDeployment` on a well-formed identifier (`bgd-` plus a 26-character lowercase/digit suffix) that was never issued is refused with that code and message `Blue Green Deployment '<identifier>' doesn't exist.` — fixed text `Blue Green Deployment '` and `' doesn't exist.`, identifier taken from the request.
Emulator: answers error code `NotImplemented`.
Why: a client cannot tell a missing deployment from an unsupported service, and the documented 404 fault its retry/branching logic is written against never arrives.
Fix: the SwitchoverBlueGreenDeployment handler (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/switchover_blue_green_deployment.rs) — at minimum resolve the identifier against blue/green state and raise BlueGreenDeploymentNotFoundFault. Open issue 20261109T135215Z-rds-switchoverbluegreendeployment-stub.md names the stub but does not carry this check's replay key.
Traces: rds.switchover-blue-green-deployment-unknown-id-rejected.

## Evidence
Cause 1 — rds.create-db-capacity-reservation-name-and-engine-validation-rejected [mismatch]:
  Expected Message: "1 validation error detected: Value 'Sim2Real-Bad-Name' at 'dBCapacityReservationName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-z](?:-?[a-z0-9]+){0,15}"
  Actual   Message: "1 validation error detected: Value 'Sim2Real-Bad-Name' at 'dBCapacityReservationName' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-z](?:-?[a-z0-9]+){0,15}$"
  Code InvalidParameterValue matched; only the rendered pattern diverges (leading ^ and trailing $ present).

Cause 2 — rds.db-proxy-target-group-idle-above-max-percent-rejected [mismatch]:
  Expected: code 'InvalidParameterCombination', message 'MaxConnectionsPercent 73 must be greater than MaxIdleConnectionsPercent 80'
  Actual:   code 'InvalidParameterValue', message 'Invalid value 80 for MaxIdleConnectionsPercent. Must be between 0 and 73.'

Cause 3 — rds.describe-option-groups-major-engine-version-without-engine-name-rejected [mismatch]:
  Expected: ('InvalidParameterCombination', 'If MajorEngineVersion is specified, then EngineName must also be specified.')
  Actual:   ('ValidationException', 'MajorEngineVersion was specified without EngineName: if MajorEngineVersion is specified, then EngineName must also be specified')

Cause 4 — rds.global-cluster-id-filter-does-not-narrow [error]:
  Expected: DescribeGlobalClusters(Filters=[{Name:'global-cluster-id', Values:[<id>]}]) -> HTTP 200, result not narrowed (the owned global cluster is listed under both its own id and a never-created id)
  Actual:   botocore.exceptions.ClientError: An error occurred (InvalidParameterValue) when calling the DescribeGlobalClusters operation: Unrecognized filter name: global-cluster-id

Cause 5 — rds.modify-instance-engine-version-downgrade-rejected [error] and rds.modify-instance-pending-engine-version-target-queued-and-withdrawn [error]:
  Expected: CreateDBInstance(Engine='postgres', EngineVersion='14.20' | '13.23', DBInstanceClass='db.t3.micro', AllocatedStorage=20, StorageType='gp2') succeeds
  Actual:   ClientError (InvalidParameterCombination) when calling the CreateDBInstance operation: Cannot find version 14.20 for postgres
            ClientError (InvalidParameterCombination) when calling the CreateDBInstance operation: Cannot find version 13.23 for postgres
  Both checks abort at setup, so the downgrade refusal ('Cannot upgrade postgres from 14.20 to 13.23') and the PendingModifiedValues.EngineVersion queue/withdraw behavior are never exercised.

Cause 6 — rds.reserved-db-instance-offering-not-found-rejected [mismatch]:
  Expected: DescribeReservedDBInstancesOfferings(ReservedDBInstancesOfferingId=<never-issued uuid>) -> code 'ReservedDBInstancesOfferingNotFound', message 'ReservedDBInstanceOffering <id> not found.'; PurchaseReservedDBInstancesOffering the same; DescribeReservedDBInstances(ReservedDBInstanceId=<never-purchased>) -> 'ReservedDBInstanceNotFound' / 'ReservedDBInstance <id> not found.'; and an unfiltered DescribeReservedDBInstancesOfferings returns at least one published offering
  Actual:   AssertionError: unexpected error code: 'NotImplemented'

Cause 7 — rds.switchover-blue-green-deployment-unknown-id-rejected [mismatch]:
  Expected: code 'BlueGreenDeploymentNotFoundFault' (HTTP 404), message "Blue Green Deployment 'bgd-<26 lowercase/digit chars>' doesn't exist."
  Actual:   AssertionError: unexpected error code: 'NotImplemented'
  Positive control in the same check (CreateDBParameterGroup + DescribeDBParameterGroups) is unaffected.

## Steps to Reproduce
Replay the eight behavioral checks listed in trace_ids against the booted emulator.

Cause 1: CreateDBCapacityReservation(DBCapacityReservationName='Sim2Real-Bad-Name', DBInstanceClass='db.r7g.large', Engine='mysql', CapacityPerAvailabilityZone=1, AvailabilityZones=[<an available AZ>]). Read Error.Code and Error.Message; the message's rendered pattern must omit the ^ and $ anchors.

Cause 2: create a DB proxy, ModifyDBProxyTargetGroup(DBProxyName=<proxy>, TargetGroupName='default', ConnectionPoolConfig={MaxConnectionsPercent:73, MaxIdleConnectionsPercent:41, ConnectionBorrowTimeout:37, InitQuery:"SET sql_mode='STRICT_ALL_TABLES'", SessionPinningFilters:['EXCLUDE_VARIABLE_SETS']}) as the baseline, then repeat with MaxIdleConnectionsPercent:80 (and every other field changed). Expect InvalidParameterCombination with 'MaxConnectionsPercent 73 must be greater than MaxIdleConnectionsPercent 80' and the baseline pool config unchanged.

Cause 3: CreateOptionGroup(EngineName='mysql', MajorEngineVersion=<mysql default major>), then DescribeOptionGroups(MajorEngineVersion=<that version>) with no EngineName. Expect InvalidParameterCombination / 'If MajorEngineVersion is specified, then EngineName must also be specified.' Control: the same MajorEngineVersion paired with EngineName='mysql' must list the group.

Cause 4: CreateGlobalCluster(GlobalClusterIdentifier=<id>, Engine='aurora-mysql'), then paginate DescribeGlobalClusters(Filters=[{Name:'global-cluster-id', Values:[<id>]}]) and again with a never-created id. Both must answer 200 and include <id>; the emulator refuses with InvalidParameterValue.

Cause 5: CreateDBInstance(Engine='postgres', EngineVersion='14.20', DBInstanceClass='db.t3.micro', AllocatedStorage=20, StorageType='gp2', MasterUsername/MasterUserPassword set, DBSubnetGroupName=<a subnet group>) and the same with EngineVersion='13.23'. Both must succeed; the emulator refuses with 'Cannot find version <v> for postgres'. Cross-check DescribeDBEngineVersions(Engine='postgres') for those minors.

Cause 6: DescribeReservedDBInstancesOfferings(ReservedDBInstancesOfferingId=<well-formed uuid never issued>), then PurchaseReservedDBInstancesOffering with the same id, then DescribeReservedDBInstances(ReservedDBInstanceId=<never purchased>), then an unfiltered DescribeReservedDBInstancesOfferings.

Cause 7: SwitchoverBlueGreenDeployment(BlueGreenDeploymentIdentifier='bgd-' + 26 lowercase hex chars, never issued). Expect BlueGreenDeploymentNotFoundFault / "Blue Green Deployment '<id>' doesn't exist."
