Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found
Issue-Step: task#17
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|ModifyDBInstance|2xx->2xx|$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port
Issue-Key: rds|RebootDBInstance|2xx->2xx|$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port
Issue-Commit: 05805e4609bedcc75437da8cd5b42dbc372da9f5

# rds DB instance envelopes publish the emulator's ephemeral listener port as DBInstance.Endpoint.Port

> _Note: this issue was identified against the code base at commit `05805e4609bedcc75437da8cd5b42dbc372da9f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyDBInstance

## Description
TWO net-new causes, one shared fault: the DB-INSTANCE envelopes that the rds emulator's mutation handlers serialize (ModifyDBInstance and RebootDBInstance) fill DBInstance.Endpoint.Port with the emulator's own randomly-chosen data listener port instead of the port the database engine accepts connections on. Each operation is counted separately below because each has its own response projection and its own replay binding; a fix must close BOTH before submitting.

Cause 1 of 2 - ModifyDBInstance (fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found, step task#17).
AWS: Smithy rds (com.amazonaws.rds#AmazonRDSv19) Endpoint.Port - "Specifies the port that the database engine is listening on"; Endpoint is "the information you need to connect to an Amazon RDS DB instance". CreateDBInstanceMessage.Port - "The port number on which the database accepts connections. Valid Values: 1150-65535. Default: RDS for Db2 - 50000; RDS for MariaDB - 3306; RDS for Microsoft SQL Server - 1433; RDS for MySQL - 3306; RDS for Oracle - 1521; RDS for PostgreSQL - 5432". ModifyDBInstance's only port input is DBPortNumber. So for a mysql instance created with no explicit Port and never given a DBPortNumber, the ModifyDBInstance 200 envelope reports DBInstance.Endpoint.Port equal to the engine default for the instance's engine (3306 for RDS for MySQL), and that value is stable across modifications that do not change DBPortNumber.
Emulator: the 200 ModifyDBInstance response carries $.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port set to the emulator's ephemeral data listener port rather than the engine port - the replay records value_mismatch, expected '3306', got '41149' (41149 is this run's listener number and is not itself the requirement).
Why: a client reads Endpoint.Address and Endpoint.Port out of the modify response to rebuild its connection string after a parameter-group or instance-class change. It caches a port that is an internal implementation detail of the emulator rather than the engine's listening port, so every subsequent native-protocol connection is aimed at the wrong port; a client that asserts the port is unchanged by a modification that did not set DBPortNumber also sees it change under it.
Fix: the ModifyDBInstance handler's DBInstance response projection and the instance state it reads - the stored per-instance endpoint port must be seeded from the create request's Port (else the engine default for the instance's Engine) and serialized from state, not from whatever port the emulator's data listener happened to bind.
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#17 (cluster of 1 step).

Cause 2 of 2 - RebootDBInstance (fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found, step task#19).
AWS: RebootDBInstance returns the same DBInstance shape (output member DBInstance; inputs are only DBInstanceIdentifier and ForceFailover), so a reboot cannot change the instance's connection coordinates: DBInstance.Endpoint.Port in the reboot envelope is the same engine port the instance was created with (3306 for RDS for MySQL per the CreateDBInstance Port default list above).
Emulator: the 200 RebootDBInstance response carries $.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port set to the emulator's ephemeral listener port - value_mismatch, expected '3306', got '41149'.
Why: reboot is exactly where a client re-reads the endpoint to reconnect. It gets the emulator's listener port instead of the engine port, so the post-reboot reconnect cannot succeed, and tooling that diffs the pre- and post-reboot envelopes cannot correlate the instance with the one it provisioned.
Fix: the RebootDBInstance handler's DBInstance response projection - it must serialize the stored instance endpoint port, which is the same state fix as cause 1 (one shared endpoint-port source for all DBInstance projections).
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#19 (cluster of 1 step).

Related but NOT covering these wire steps: open issue 20261121T073752Z-rds-db-cluster-envelopes-publish-the-emulator-s (and 20261108T233943Z-rds-db-cluster-envelopes-report-the-emulator-s-random) describe the same ephemeral-port fault on DB CLUSTER envelopes (DBCluster.Port on DescribeDBClusters / CreateDBCluster / DeleteDBCluster), not on DBInstance.Endpoint.Port in the instance mutation paths; 20261120T153000Z-pi-rds-the-published-db-instance-endpoint-is-unreachable-from-the-replay-client and 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners describe the published instance endpoint behaviorally (unreachability) rather than the serialized Port value. The replay reports these two steps as unowned by any open issue.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health answered 200 and the data listener accepts). Both divergences carry stub_path "" and owned_by [] - i.e. neither is blocked by an unimplemented operation nor already owned by an open issue.

1) ModifyDBInstance, expected 200 / actual 200, fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found step task#17 (cluster of 1 step):
   $.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port: value_mismatch (expected '3306', got '41149')
   divergence_key: service=rds|ModifyDBInstance|status=200->200|keys=$.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port

2) RebootDBInstance, expected 200 / actual 200, same fixture, step task#19 (cluster of 1 step):
   $.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port: value_mismatch (expected '3306', got '41149')
   divergence_key: service=rds|RebootDBInstance|status=200->200|keys=$.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port

The requirement is the RULE, not these recorded numbers: '41149' is the emulator's per-run data listener port and is never part of any requirement, and '3306' is the value the rule yields for THIS fixture's engine (RDS for MySQL, no explicit Port at create, no DBPortNumber on modify). What must hold in general is that DBInstance.Endpoint.Port is the instance's own engine connection port - the Port given at CreateDBInstance, else the modeled per-engine default (Db2 50000, MariaDB 3306, Microsoft SQL Server 1433, MySQL 3306, Oracle 1521, PostgreSQL 5432), updated only by ModifyDBInstance DBPortNumber - and that it is identical in the create, describe, modify and reboot projections of the same instance. Statuses are correct in both steps (200/200); only the serialized Port diverges.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found step task#17 - ModifyDBInstance (parameter-group switch) on the mysql DB instance the fixture created with no explicit Port and no DBPortNumber on the modify. Read $.ModifyDBInstanceResponse.ModifyDBInstanceResult.DBInstance.Endpoint.Port: AWS reports 3306 (the RDS for MySQL default), the emulator reports its own ephemeral data listener port.
3. Cause 2: replay the same fixture step task#19 - RebootDBInstance on the same instance - and read $.RebootDBInstanceResponse.RebootDBInstanceResult.DBInstance.Endpoint.Port; it repeats the same ephemeral-listener value even though reboot takes no port input.
4. Cross-check against the create/describe projection of the same instance in the same fixture: the engine port must be identical in every DBInstance envelope.
5. Re-run both steps after the fix; both must report the instance's engine port (3306 here) and the two causes must close together.
