Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found
Issue-Step: task#18
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$
Issue-Key: rds|DescribeDBInstances|2xx->2xx|$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus
Issue-Related: 20261111T091517Z-rds-db-instance-read-delete-envelopes-and-the.md
Issue-Commit: 4952bdb35e1f11079a9f3e7fa1c51520672789b0

# rds DescribeDBInstances never reports a switched DB parameter group as pending-reboot, so the post-modify convergence poll never settles

> _Note: this issue was identified against the code base at commit `4952bdb35e1f11079a9f3e7fa1c51520672789b0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstances

## Description
One net-new root cause on the rds surface, found by replaying the wire fixture corpus (simparity-wire-aws-v1) against the emulator built from this checkout. All 64 divergence clusters in this replay carried service 'rds'; 63 were reported owned_by an open issue that already carries their replay key and are skipped, and none was blocked by an unimplemented scaffold stub. The single unowned cluster is below.

Cause 1 of 1: after a ModifyDBInstance that switches DBParameterGroupName, DescribeDBInstances never publishes the group's outstanding apply status, so a poll that waits for it to become 'pending-reboot' never converges; the same envelope also publishes the emulator process's listener port as the instance endpoint port. Operation DescribeDBInstances; fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found step task#18.
AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models DescribeDBInstances -> DBInstanceMessage.DBInstances, a list of DBInstance whose DBParameterGroups member is a list of DBParameterGroupStatus. DBParameterGroupStatus.ParameterApplyStatus is documented with the value set 'applying' | 'failed-to-apply' | 'in-sync' | 'pending-database-upgrade' | 'pending-reboot', where 'pending-reboot' is defined as "The parameter group change will be applied after the DB instance reboots" and 'in-sync' as "The parameter group change is synchronized with the database". A ModifyDBInstance that associates a different (non-default) DB parameter group is therefore a change that is NOT yet synchronized: AWS reports that association with ParameterApplyStatus 'pending-reboot' on every DescribeDBInstances read until a reboot actually applies it, and only then flips it to 'in-sync'. The recorded response for this step is exactly that: DBParameterGroups.DBParameterGroup = {DBParameterGroupName: <the newly associated group>, ParameterApplyStatus: 'pending-reboot'} on an instance whose DBInstanceStatus is 'available'. The same response's DBInstance.Endpoint.Port is modelled as com.amazonaws.rds#Endpoint.Port, documented "Specifies the port that the database engine is listening on" — a stable property of the instance (the port it was created with, defaulting to the engine's default, 3306 for mysql), not a property of whatever host process answers; the recorded value is 3306.
Emulator: the emulator answers DescribeDBInstances 200 but never transitions the switched parameter group out of 'in-sync', so the fixture's convergence poll on ParameterApplyStatus == 'pending-reboot' observes 'in-sync' on every attempt and the step ends with the replay harness's poll cap reached ('$: poll capped at 180s') — the state the recording reaches is never reachable on the emulator. In the same envelope DBInstance.Endpoint.Port is an OS-assigned per-run listener port (this run: a five-digit ephemeral port) rather than the instance's engine port.
Why: the pending-reboot / in-sync handshake is the only signal a client has that a parameter-group change still needs a reboot. A client that switches a parameter group and then waits for 'pending-reboot' before issuing RebootDBInstance (Terraform's aws_db_instance, CloudFormation/CDK DBInstance updates, and operational runbooks all do this) waits forever against the emulator, so the whole modify-then-reboot sequence is unrunnable; a client that instead reads the status to decide whether a change is live sees 'in-sync' immediately and concludes a change that was never applied is already in effect. A client that reads Endpoint.Port to build a connection string dials a port that is not the database's, and the modelled per-engine default is simply not observable.
Fix: the DBInstance state behind the ModifyDBInstance / DescribeDBInstances handlers in the rds crate — the parameter-group apply-status transition (ModifyDBInstance must record the new association as pending-reboot and only RebootDBInstance may clear it to in-sync), and the DBInstance endpoint publication its serializer reads, which must report the instance's own engine port rather than the bound listener's. NOTE: open issue 20261123T220118Z-rds-3-net-new-causes-rebootdbinstance-publishes-in.md reports the sibling half of this fault on RebootDBInstance (step task#19 of the same fixture) and 20261122T000359Z-rds-db-instance-envelopes-publish-the-emulator-s.md / 20261123T132332Z-rds-a-served-engine-s-dbinstance-endpoint-address.md cover the endpoint-publication half for other envelopes; none of them carries THIS step's replay key and none covers the DescribeDBInstances read that the convergence poll uses, so this step must be re-replayed on its own.
Traces: rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#18.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: bootstrap_signal false, emulator_up true (/__admin/health answered 200 and the data listener accepts), 106 fixtures replayed, 1132 steps total, 722 passed, 406 mismatched, 0 errored, 4 blocked, score 63.8%, 64 divergence clusters. Every cluster carried service 'rds'; 63 were reported owned_by an open issue and skipped; none carried a stub_path.

The one unowned cluster:
rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found#task#18 — DescribeDBInstances, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.
divergence_key: service=rds|DescribeDBInstances|status=200->200|keys=$,$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus,$.DescribeDBInstancesResponse.DescribeDBInstancesResult.DBInstances.DBInstance.Endpoint.Port
detail: "$: poll capped at 180s" — the step is a convergence poll and the emulator never reaches the recorded state. The recorded (expected) DBInstance carries DBParameterGroups.DBParameterGroup = {DBParameterGroupName: <the group created and associated earlier in the fixture>, ParameterApplyStatus: 'pending-reboot'}, DBInstanceStatus 'available', and Endpoint = {Address: <shaped-not-asserted>, Port: '3306', HostedZoneId: ...}.

Expected per AWS (and per the recording): ParameterApplyStatus 'pending-reboot' for the group switched by the preceding ModifyDBInstance, and Endpoint.Port the instance's engine port (3306 for this mysql instance).
Actual on the emulator: ParameterApplyStatus stays 'in-sync' for the whole 180 s poll window, and Endpoint.Port is the run's OS-assigned listener port.
Per-run note: the specific ephemeral port number, the generated identifiers (sim2real-probe-rds-db-*, sim2real-probe-rds-dbpg-*), InstanceCreateTime, DbiResourceId and the VPC/subnet/security-group ids in the recording are per-run values and are not part of the requirement; the requirements are the 'pending-reboot' apply status for an unapplied group association and the instance's own engine port as Endpoint.Port.

Smithy citations: DBParameterGroupStatus.ParameterApplyStatus value set and definitions ('pending-reboot' = "The parameter group change will be applied after the DB instance reboots"; 'in-sync' = "The parameter group change is synchronized with the database"); com.amazonaws.rds#Endpoint.Port = "Specifies the port that the database engine is listening on".

## Steps to Reproduce
1. Clone this checkout, build the emulator and boot it.
2. Replay the wire corpus simparity-wire-aws-v1 (106 fixtures, 1132 steps) against it.
3. Replay fixture rds-db-instance-provider-explicit-defaults-parameter-group-switch-reboot-and-not-found and inspect step task#18: the fixture creates a mysql DB instance with the family's default parameter group, creates a new DB parameter group (CreateDBParameterGroup), associates it with ModifyDBInstance(DBParameterGroupName=<new group>), then polls DescribeDBInstances(DBInstanceIdentifier=<id>) waiting for DBInstances.DBInstance.DBParameterGroups.DBParameterGroup.ParameterApplyStatus == 'pending-reboot'.
4. Observe that every poll iteration returns ParameterApplyStatus 'in-sync' and the step ends at the 180 s poll cap without ever reaching the recorded state.
5. In the same envelope read DBInstance.Endpoint.Port: it is the emulator's per-run listener port instead of the instance's engine port (3306).
6. Re-replay the fixture step named in Issue-Trace/Issue-Step to confirm a fix closes this cause: ParameterApplyStatus must be 'pending-reboot' until a RebootDBInstance applies the group, and Endpoint.Port must be the instance's engine port.
