Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.engine-dialect-discriminator-query
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.engine-dialect-discriminator-query|run#17|AssertionError|assert pg_host != my_host, (
Issue-Commit: 74429669c22f4248895c195473d5b207144eaced

# rds: a served engine's DBInstance Endpoint.Address is the core listener's bind host, so a postgres and a mysql instance publish the same address (127.0.0.1) and cannot be told apart

> _Note: this issue was identified against the code base at commit `74429669c22f4248895c195473d5b207144eaced`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstances

## Description
One net-new root cause on the rds surface, found by replaying the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout. The failing check below was reported neither ALREADY FILED (blocked by an unimplemented scaffold stub) nor OWNED by an open issue.

Cause 1 of 1: every DB instance of an engine whose wire this emulator SERVES publishes the one host the core binds, so Endpoint.Address is not per-resource and two live instances of two different engines are indistinguishable by the address a client is told to dial.
AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models DBInstance$Endpoint as an Endpoint with Address documented as "the DNS address of the DB instance" and Port as "the port that the database engine is listening on". That address is derived from the instance's own identifier (one DNS name per DB instance, of the form <db-instance-identifier>.<hash>.<region>.rds.amazonaws.com), so two distinct DB instances NEVER share one Address, and a PostgreSQL instance's Port defaults to 5432 while a MySQL instance's defaults to 3306 (CreateDBInstanceMessage$Port, per-engine defaults) unless the request names another Port. The requirement is the SHAPE of the address (one name per instance, resolvable to that instance) and the engine's port, not any literal host from one run.
Emulator: DescribeDBInstances answers, for a postgres instance and a mysql instance created side by side in one region and both polled to DBInstanceStatus 'available', an Endpoint.Address of '127.0.0.1' for BOTH — the process's bind interface rather than a per-instance name — and an OS-assigned Port rather than the engine's. The replay fails with: AssertionError: the postgres and mysql instances must have different endpoint addresses; both resolve to '127.0.0.1' (check_rds_engine_dialect_discriminator_query.py line 298).
Why: a client that reads the endpoint cannot distinguish two instances at all. A dialect-discriminating client (an ORM or a migration tool that probes the server before choosing SQL), a connection-string handoff, a restore verification and any read/write split all key on the published address; here one address stands for every served resource, so dialing "the postgres instance" may land on the mysql one, and "postgres listens on 5432" is false.
Fix: the endpoint publication for served engines — the DescribeDBInstances / CreateDBInstance representation of DBInstance$Endpoint in the rds crate's state, together with the per-resource listener address it is derived from, so each instance publishes its own address on its engine's port and that address reaches that instance.
NOTE: open issue 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md describes this same cause (its cause 1, including the core-listener constraint in vendor/server/src/listeners.rs bind_std/bind_tokio and ResourceListener::host()/port()), and open issue 20261123T041052Z-rds-4-net-new-endpoint-publication-causes-a.md covers endpoint publication for other checks. Neither carries this check's replay key, so the replay did not treat this failure as owned; this check must be re-replayed on its own when that work lands.
Traces: rds.engine-dialect-discriminator-query.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 556/709 checks passed (78.4%); service rds 311/411 (76%), 100 failed.

rds.engine-dialect-discriminator-query [mismatch]:
Error: File "run.py", line 456, in <lambda> body = calls.in_phase(calls.BODY, lambda: fn(ctx)) / File "check_rds_engine_dialect_discriminator_query.py", line 298, in run / assert pg_host != my_host, ... / AssertionError: the postgres and mysql instances must have different endpoint addresses; both resolve to '127.0.0.1'

Sequence the check ran: create a security group opening 5432 and 3306; CreateDBSubnetGroup; CreateDBInstance(Engine=postgres, DBInstanceClass=db.t3.micro, AllocatedStorage=20, DBName=<db>, BackupRetentionPeriod=0, PubliclyAccessible=False, VpcSecurityGroupIds=[sg], EngineVersion=<default>) and the same for Engine=mysql; poll DescribeDBInstances for each until DBInstanceStatus=='available'; read Endpoint.Address and Endpoint.Port of each.
Expected per AWS: two different Address values, each a DNS name derived from that instance's own identifier, postgres on 5432 and mysql on 3306 (or the requested Port).
Actual: both Address values are the identical string '127.0.0.1' (the core's bind interface) and the ports are OS-assigned, so the check stops before it can prove each endpoint speaks only its own dialect (SELECT current_setting('server_version') on postgres, SELECT @@version_comment on mysql).
Note on per-run values: the literal host, the ports and the generated identifiers above are this environment's; the requirement is that each instance publish its OWN address and its engine's port.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateDBSubnetGroup over the run's subnets, then CreateDBInstance(Engine=postgres, DBInstanceClass=db.t3.micro, AllocatedStorage=20, DBName=<db>, MasterUsername/MasterUserPassword, BackupRetentionPeriod=0, PubliclyAccessible=False) and CreateDBInstance with the same shape but Engine=mysql.
3. Poll DescribeDBInstances on each identifier until DBInstanceStatus=='available'.
4. Compare Endpoint.Address of the two instances: they are the same host ('127.0.0.1'). Compare Endpoint.Port with the engine defaults 5432 / 3306: neither matches.
5. Replay the behavioral suite (simparity-behavioral-aws-v1), check rds.engine-dialect-discriminator-query, to see the same assertion.
