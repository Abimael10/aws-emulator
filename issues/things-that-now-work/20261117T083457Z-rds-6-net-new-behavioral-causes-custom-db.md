Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.cluster-endpoint-duplicate-name-rejected rds.concurrent-create-cluster-endpoint-duplicate-name-race rds.describe-cluster-endpoints rds.cluster-endpoint-lifecycle rds.aurora-mysql-custom-reader-endpoint-routes-to-static-member rds.aurora-mysql-backtrack-removes-rows-after-time-keeps-before rds.maintenance-window-controls-pending-change-application rds.engine-dialect-discriminator-query
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.cluster-endpoint-duplicate-name-rejected|run#15|AssertionError|assert message == expected, f"unexpected message: {message!r} (expected {expected!r})"
Issue-Key: rds.concurrent-create-cluster-endpoint-duplicate-name-race|run#21|AssertionError|assert failures[0]["message"] == expected, failures[0]["message"]
Issue-Key: rds.describe-cluster-endpoints|run#15|AssertionError|assert message == (
Issue-Key: rds.cluster-endpoint-lifecycle|_wait_endpoint_gone#21|Timeout|return
Issue-Key: rds.aurora-mysql-custom-reader-endpoint-routes-to-static-member|_my_connect_with_retry#26|Timeout|time.sleep(2)
Issue-Key: rds.aurora-mysql-backtrack-removes-rows-after-time-keeps-before|_sql_rows#22|ProgrammingError|cur.execute(statement)
Issue-Key: rds.maintenance-window-controls-pending-change-application|run#19|Timeout|instance = rds.describe_db_instances(DBInstanceIdentifier=instance_id)["DBInstances"][0]
Issue-Key: rds.engine-dialect-discriminator-query|run#17|AssertionError|assert pg_host != my_host, (
Issue-Commit: 7f5dd6cdee8fc233932a8e3b3b251e9cc2539379

# rds: 6 net-new behavioral causes — custom DB-cluster-endpoint refusals are emulator-authored, the endpoint lifecycle and reader-endpoint routing never converge, BacktrackDBCluster discards the cluster schema, a deferred ModifyDBInstance never settles, and every instance publishes one endpoint address

> _Note: this issue was identified against the code base at commit `7f5dd6cdee8fc233932a8e3b3b251e9cc2539379`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBClusterEndpoint

## Description
Six net-new root causes on the rds control plane, all found by replaying the behavioral suite against the emulator built from this checkout. They are independent and each must be closed on its own.

Cause 1 of 6: CreateDBClusterEndpoint's duplicate-identifier refusal carries emulator-authored text.
AWS: CreateDBClusterEndpoint requires DBClusterIdentifier, DBClusterEndpointIdentifier and EndpointType and models DBClusterEndpointAlreadyExistsFault (Smithy: CreateDBClusterEndpoint -> errors include DBClusterEndpointAlreadyExistsFault, errorKind client, httpError 400, awsQueryError code 'DBClusterEndpointAlreadyExistsFault' httpResponseCode 400). The model does not fix the message text, so recorded behavior supplies it: the error code is DBClusterEndpointAlreadyExistsFault and the Message is the fixed text 'An endpoint with the identifier ' + ' already exists: ' + ' ' assembled as 'An endpoint with the identifier <DBClusterEndpointIdentifier> already exists: <Endpoint of the endpoint already holding that identifier>'. Fixed text: 'An endpoint with the identifier ' and ' already exists: '. Variable parts: the request's DBClusterEndpointIdentifier, and the DNS address the existing endpoint object publishes in its Endpoint member (read out of the emulator's own state, never a literal from one run).
Emulator: the error code is right, but the Message is "The specified custom endpoint can't be created because it already exists." — the error shape's class description. It names neither the identifier that collided nor the endpoint already holding it, and shares no fixed text with AWS's sentence.
Why: a client that creates several custom endpoints in one flow cannot tell from the response WHICH identifier was already taken, and every assertion, log scraper or runbook keyed on AWS's 'An endpoint with the identifier <id> already exists: <endpoint>' never matches. Because the code is correct the divergence is silent — the caller sees the right error class carrying text no AWS-targeted test accepts.
Fix: the CreateDBClusterEndpoint handler's identifier-uniqueness gate, where DBClusterEndpointAlreadyExistsFault is constructed — format AWS's sentence from the request's DBClusterEndpointIdentifier and the stored endpoint's Endpoint address instead of emitting the error shape's description. The same wording must come out of the concurrent path: the loser of two simultaneous creates gets the identical message.
Traces: rds.cluster-endpoint-duplicate-name-rejected, rds.concurrent-create-cluster-endpoint-duplicate-name-race (the second also proves the single-winner behaviour is right — exactly one create succeeds and one endpoint object exists; only the loser's message diverges).

Cause 2 of 6: DescribeDBClusterEndpoints' cluster-not-found refusal is worded by the emulator.
AWS: DescribeDBClusterEndpoints takes an optional DBClusterIdentifier and models exactly one error (Smithy: DescribeDBClusterEndpoints -> errors [DBClusterNotFoundFault], errorKind client, httpError 404, awsQueryError code 'DBClusterNotFoundFault' httpResponseCode 404). Recorded behavior supplies the message: the fixed text 'The source cluster could not be found or cannot be accessed: ' followed by the DBClusterIdentifier exactly as the request sent it. The identifier is the only variable part.
Emulator: the DBClusterNotFoundFault code is right; the Message is the emulator's generic state-lookup form 'DBCluster <identifier> not found.' — the internal state-type name in a '<Type> <id> not found.' debug format, which drops the 'or cannot be accessed' half of the condition.
Why: a client that branches on the refusal text reports an ordinary missing-or-inaccessible-cluster condition as an unrecognised failure; because status and code match, nothing else signals the problem.
Fix: the DescribeDBClusterEndpoints handler's cluster-resolution path — emit RDS's fixed sentence with the request's DBClusterIdentifier interpolated instead of the shared '<Type> <id> not found.' formatter. NOTE: open issue 20261116T115846Z-rds-4-net-new-causes-db-cluster-endpoint.md describes this same wording defect from the WIRE corpus (its cause 1) and carries the wire replay key; it does not carry this behavioral check, so this cause is listed here so the behavioral check is re-replayed when that fix lands — the two can be closed together.
Traces: rds.describe-cluster-endpoints.

Cause 3 of 6: the custom DB-cluster-endpoint lifecycle and a custom reader endpoint's routing never converge, so neither reaches a verdict.
AWS: a custom cluster endpoint created by CreateDBClusterEndpoint reports Status 'creating', then 'available'; ModifyDBClusterEndpoint moves it to 'modifying' and back to 'available'; DeleteDBClusterEndpoint moves it to 'deleting' and then DescribeDBClusterEndpoints answers DBClusterEndpointNotFoundFault for it (Smithy: DBClusterEndpoint output carries Status, Endpoint, EndpointType, CustomEndpointType, StaticMembers, ExcludedMembers). A READER endpoint created with StaticMembers routes sessions only to the listed members. Status progression and the endpoint actually accepting a session are both required.
Emulator: both flows spend their whole per-check budget without the endpoint ever reaching the next status / without a session arriving at the static member — the replay output for both checks is only 'Check timed out after 60.0s', with no refusal and no state transition observed.
Why: a client that waits for a custom endpoint to become available — every CDK/CloudFormation deploy and every connection-string handoff does — never proceeds, and a reader endpoint whose StaticMembers are ignored silently sends reads to the wrong instance.
Fix: the custom-cluster-endpoint state machine (the CreateDBClusterEndpoint / ModifyDBClusterEndpoint / DeleteDBClusterEndpoint handlers and the stored endpoint's status transitions), and the listener that fronts a custom endpoint so a session opened against it lands on the member the endpoint selects.
Traces: rds.cluster-endpoint-lifecycle, rds.aurora-mysql-custom-reader-endpoint-routes-to-static-member.

Cause 4 of 6: BacktrackDBCluster discards the cluster's schema instead of rewinding it to the target time.
AWS: BacktrackDBCluster requires DBClusterIdentifier and BacktrackTo and rewinds an Aurora MySQL cluster created with a BacktrackWindow to that instant (Smithy: BacktrackDBCluster -> output DBClusterBacktrack {BacktrackTo, BacktrackedFrom, Status}; errors DBClusterNotFoundFault / InvalidDBClusterStateFault). Everything committed at or before BacktrackTo survives; only what was committed after it is gone. A table created before BacktrackTo therefore still exists after the backtrack, with exactly the rows it had at that instant.
Emulator: after BacktrackDBCluster to a timestamp strictly between the two inserts, a fresh session on the writer endpoint is answered MySQL error 1146 'Table '<database>.<table>' doesn't exist' — the table, which was created well before BacktrackTo, is gone along with the pre-target row. The backtrack wipes the database rather than rewinding it.
Why: backtrack is advertised as a non-destructive rewind; a client that backtracks to undo a bad write loses its entire schema and all data, and the post-backtrack read fails with a missing-table error instead of returning the pre-target rows.
Fix: the BacktrackDBCluster handler and the engine state it rewinds — the rewind must restore the cluster's contents as of BacktrackTo (schema included) rather than resetting the database.
Traces: rds.aurora-mysql-backtrack-removes-rows-after-time-keeps-before.

Cause 5 of 6: a deferred ModifyDBInstance never settles into the live/pending split, so the check never reaches a verdict.
AWS: ModifyDBInstance with ApplyImmediately=false records the requested change in PendingModifiedValues and leaves the live value untouched until the PreferredMaintenanceWindow opens (Smithy: ModifyDBInstance input ApplyImmediately, AllocatedStorage, PreferredMaintenanceWindow; DBInstance output carries PendingModifiedValues). With the maintenance window set days ahead, DescribeDBInstances must keep reporting the original AllocatedStorage and show the new value only under PendingModifiedValues for the whole observation.
Emulator: the check spends its entire per-check budget without that state ever being observable — the replay output is only 'Check timed out after 60.0s', so the instance never reaches the state in which the live/pending split can be read.
Why: a client that polls PendingModifiedValues to decide whether a deferred change is still queued gets no usable answer, and a deploy that waits for the instance to settle after a deferred modify never finishes.
Fix: the ModifyDBInstance handler's deferred-change path and the DB-instance status/PendingModifiedValues state it drives.
Traces: rds.maintenance-window-controls-pending-change-application.

Cause 6 of 6: two DB instances with different engines publish the same Endpoint.Address, so they are indistinguishable by address.
AWS: every DB instance gets its own Endpoint.Address — a per-instance DNS name (<instance-id>.<token>.<region>.rds.amazonaws.com) — so two instances, let alone two instances of different engines, never share an address. The address is the only thing a client has to route a session to a specific instance.
Emulator: a postgres instance and a mysql instance created in the same run both report Endpoint.Address '127.0.0.1' (the replay asserts only that the two differ, not what either is — the literal loopback value is this environment's, not a requirement). With one shared address the engine a session reaches is decided by the port alone, and nothing identifies the instance.
Why: a client holding two connection strings built from DescribeDBInstances cannot tell the instances apart and can send a PostgreSQL-only statement to the MySQL instance; any code that keys a connection pool or a secret on the endpoint address collapses the two instances into one.
Fix: the DB-instance endpoint-address minting in the RDS crate's state — derive a per-instance address from the instance identifier instead of publishing one shared host for every instance. NOTE: open issue 20261115T181413Z-rds-3-net-new-behavioral-causes-every-db.md reports the same shared-address defect from a different check and does not carry this check's replay key; closing that one should re-replay this check too.
Traces: rds.engine-dialect-discriminator-query.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 417/676 checks passed (61.7%), 196 mismatched, 63 errored. The failures below are the ones the replay reported neither ALREADY FILED (blocked by an unimplemented stub) nor OWNED by an open issue.

Cause 1 — rds.cluster-endpoint-duplicate-name-rejected:
  error code DBClusterEndpointAlreadyExistsFault matched; message diverged
  expected: 'An endpoint with the identifier <DBClusterEndpointIdentifier> already exists: <Endpoint of the existing endpoint>'
  got:      "The specified custom endpoint can't be created because it already exists."
  AssertionError: unexpected message: "The specified custom endpoint can't be created because it already exists." (expected 'An endpoint with the identifier <id> already exists: <endpoint address>')
  Fixed text of AWS's message: 'An endpoint with the identifier ' ... ' already exists: '. Variable parts: the request's DBClusterEndpointIdentifier and the existing endpoint's Endpoint address.
  Effect check in the same replay passed: exactly one endpoint object exists under the identifier, and after deleting the holder the identical create succeeds — so only the wording diverges.
Cause 1 — rds.concurrent-create-cluster-endpoint-duplicate-name-race: exactly one of two simultaneous creates succeeded and the loser's code was DBClusterEndpointAlreadyExistsFault (both correct); the loser's message was again "The specified custom endpoint can't be created because it already exists."

Cause 2 — rds.describe-cluster-endpoints: code DBClusterNotFoundFault matched.
  expected: 'The source cluster could not be found or cannot be accessed: <DBClusterIdentifier sent by the request>'
  got:      'DBCluster <DBClusterIdentifier sent by the request> not found.'

Cause 3 — rds.cluster-endpoint-lifecycle: 'Check timed out after 60.0s' while waiting for the custom endpoint to walk creating -> available -> modifying -> deleting -> not-found.
Cause 3 — rds.aurora-mysql-custom-reader-endpoint-routes-to-static-member: 'Check timed out after 60.0s' before any session reached the static member.

Cause 4 — rds.aurora-mysql-backtrack-removes-rows-after-time-keeps-before: the seed row landed and both rows were present before the backtrack (asserted and passed). After rds.backtrack_db_cluster(BacktrackTo=<instant between the two inserts>, Force=True) and the cluster returning to 'available', a fresh MySQL session reading the table is answered:
  pymysql.err.ProgrammingError: (1146, "Table '<database>.<table>' doesn't exist")
  Expected: the table still exists and contains exactly the row committed before BacktrackTo.

Cause 5 — rds.maintenance-window-controls-pending-change-application: 'Check timed out after 60.0s'; the live AllocatedStorage / PendingModifiedValues split after ModifyDBInstance(ApplyImmediately=False) was never observable.

Cause 6 — rds.engine-dialect-discriminator-query:
  AssertionError: the postgres and mysql instances must have different endpoint addresses; both resolve to '127.0.0.1'
  (the assertion is on the two addresses being distinct; the loopback literal is environment-specific and not itself the requirement)

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1).
3. Cause 1: run rds.cluster-endpoint-duplicate-name-rejected — create an Aurora cluster, create a custom endpoint, then CreateDBClusterEndpoint again with the same DBClusterEndpointIdentifier. The refusal is DBClusterEndpointAlreadyExistsFault (correct) with message "The specified custom endpoint can't be created because it already exists." instead of 'An endpoint with the identifier <id> already exists: <existing endpoint address>'. Then run rds.concurrent-create-cluster-endpoint-duplicate-name-race and read the losing call's message.
4. Cause 2: run rds.describe-cluster-endpoints — DescribeDBClusterEndpoints with a DBClusterIdentifier naming no cluster answers DBClusterNotFoundFault with 'DBCluster <id> not found.' instead of 'The source cluster could not be found or cannot be accessed: <id>'.
5. Cause 3: run rds.cluster-endpoint-lifecycle and rds.aurora-mysql-custom-reader-endpoint-routes-to-static-member — both exhaust the per-check budget with no endpoint status transition and no session reaching the static member.
6. Cause 4: run rds.aurora-mysql-backtrack-removes-rows-after-time-keeps-before — create an aurora-mysql cluster with BacktrackWindow, CREATE TABLE, insert one row, note the server clock, insert a second row, BacktrackDBCluster to the noted instant, then re-read the table: MySQL 1146 'table doesn't exist' instead of the single pre-target row.
7. Cause 5: run rds.maintenance-window-controls-pending-change-application — ModifyDBInstance(AllocatedStorage=30, ApplyImmediately=False) with the maintenance window three days out, then poll DescribeDBInstances for the live 20 / pending 30 split; the check never reaches a verdict.
8. Cause 6: run rds.engine-dialect-discriminator-query — create one postgres and one mysql instance and compare DescribeDBInstances Endpoint.Address: both are the same host.
