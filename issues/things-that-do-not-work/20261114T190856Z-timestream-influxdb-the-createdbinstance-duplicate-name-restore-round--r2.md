Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected
Issue-Shape-Id: com.amazonaws.timestreaminfluxdb#AmazonTimestreamInfluxDB
Issue-Key: timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected|_poll_until#32|Timeout|time.sleep(interval_s)
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# timestream-influxdb: the CreateDbInstance duplicate-name restore round trip still never completes — create → AVAILABLE → delete → identical re-create is cut off at the check budget (60 s)

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDbInstance

## Description
One net-new root cause in the timestream-influxdb emulator, grounded in the failing behavioral check timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected. The check is NOT marked owned by any open issue: the only issue that ever carried this timeout signature (Issue-Key `timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected|_poll_until#32|Timeout|time.sleep(interval_s)`) is the RESOLVED 20261113T092956Z-timestream-influxdb-the-createdbinstance-duplicate-name-restore-round.md, and the check still fails with exactly that signature, so the defect it described is still present in the emulator. This is not a refusal — nothing is answered with an error code — so it is reported here as the surviving defect rather than as a refused call.

Cause 1 of 1: the DB-instance lifecycle the duplicate-name scenario needs — CreateDbInstance (CREATING) → GetDbInstance reports AVAILABLE → DeleteDbInstance frees the name → the identical CreateDbInstance is accepted again and the NEW instance reaches AVAILABLE — still does not complete when driven the way a client drives it. The replay produces only "Error: Check timed out after 60.0s": nothing past the conflict leg is observed, so the emulator never demonstrates that a created instance becomes AVAILABLE, that the delete releases the name, or that the identical re-create then succeeds with a new id. Failing check ids: timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected.

AWS: Smithy model of this service — CreateDbInstance returns required `id`, `name`, `arn` plus `status` (enum Status: AVAILABLE | CREATING | DELETED | DELETING | FAILED | MAINTENANCE | MODIFYING | REBOOTING | REBOOT_FAILED | RESTORE_FAILED | RESTORING | UPDATING | UPDATING_DEPLOYMENT_TYPE | UPDATING_INSTANCE_TYPE); `name` is DbInstanceName @length 3..40 @pattern ^[a-zA-Z][a-zA-Z0-9]*(-[a-zA-Z0-9]+)*$; CreateDbInstance models ConflictException (client, httpError 409); GetDbInstance (readonly) returns the same record and models ResourceNotFoundException (HTTP 404); DeleteDbInstance models ConflictException (409) and ResourceNotFoundException (404). Recorded behavior: a freshly created instance reports CREATING and then settles to AVAILABLE; a second CreateDbInstance with a name a live instance still owns is refused 409 ConflictException while the ORIGINAL stays the only instance with that name; after that instance is deleted (DeleteDbInstance → DELETING, then the id stops resolving) the identical CreateDbInstance succeeds and returns a NEW `id` with status CREATING, which then reaches AVAILABLE. The requirement is the LIVENESS of that sequence: a transient status this service reports must settle before a polling client's follow-up read, because the duplicate name is a STATE (a live instance owns it), not an always-invalid input, so the post-delete re-create must become possible for a client that polls on its own cadence.

Emulator: the sequence is still cut off — the replay output for this check is only "Error: Check timed out after 60.0s", with no emulator response recorded past the conflict leg. Hypothesis (the harness does not isolate the leg, and the mechanism is not stated by any recording): the settle of a transient status is still materialized lazily on read after a non-zero span, so a client whose next GetDbInstance arrives sooner than that span sees CREATING and then burns a whole probe interval before reading again — once for the original instance and once for the restored one — which is more wall time than the scenario is allowed. The earlier fix shortened that span (services/service-com-amazonaws-timestreaminfluxdb-amazontimestreaminfluxdb/src/state.rs now defines TRANSITION_SPAN = Duration::from_millis(25), applied by DbInstance::settle only when a reporting operation reads the record) but did not make the created instance read AVAILABLE on the client's very next read, so the round trip still does not converge.

Why: a client cannot use the ordinary create-if-absent pattern against this emulator. It creates an instance, correctly receives the 409 for the duplicate name, and then waits on readiness or on the name becoming reusable without ever getting there; no caller can distinguish "still provisioning" from "never going to settle", and the duplicate-name conflict reads as permanent rather than as the transient state AWS makes it. Every workflow that provisions, waits, deletes and re-provisions the same instance name hangs.

Fix: the DB-instance lifecycle in the service state (DbInstance::settle / DbInstance::begin_transition and the transient-status span in the service's state module) together with the CreateDbInstance and DeleteDbInstance handlers: a created instance must report AVAILABLE to the next read a client issues after the create, and a deleted instance must release its name and stop resolving at once, so create → AVAILABLE → delete → identical-re-create → AVAILABLE completes without the client spending a polling interval on each transition.

Traces: timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected. Related but NOT covering: open issue 20261109T183400Z-timestream-influxdb-2-net-new-causes-createdbinstance-duplicate.md owns this check only under its ConflictException-message replay key (Cause 1, the "The DbInstance with id […], name […], customer […] already exists" wording) and its endpoint-publication cause; neither carries this timeout signature, so closing that issue leaves the restore leg of this scenario unrun.

## Evidence
Check: timestream-influxdb.create-db-instance-duplicate-name-conflict-rejected [mismatch]
  reported failure (verbatim, the only output the replay produced for it): "Error: Check timed out after 60.0s"

What the scenario asks of the emulator, in order (from the check body):
  1. CreateDbInstance(name=<N>, password=<random>, dbInstanceType=db.influx.medium, vpcSubnetIds=[<2 subnets>], vpcSecurityGroupIds=[<sg>], allocatedStorage=20, dbStorageType=InfluxIOIncludedT1, deploymentType=SINGLE_AZ) → expects status CREATING and name <N>.
  2. CreateDbInstance with the identical name → expects 409 ConflictException (that message rule is owned by the related open issue, not by this one).
  3. ListDbInstances → expects exactly one instance named <N>, the original id.
  4. GetDbInstance(identifier=<id>) polled until status is in (AVAILABLE, FAILED) → expects AVAILABLE.
  5. DeleteDbInstance(identifier=<id>), then GetDbInstance polled until the id no longer resolves or reports DELETED.
  6. CreateDbInstance with the identical name → expects success with a NEW id and status CREATING, exactly one instance owning <N>, and that new instance polled to AVAILABLE.

Expected: the whole sequence completes and legs 4-6 are observable.
Actual: the check is cut off at 60.0s with nothing recorded past leg 2 — legs 4, 5 and 6 produce no observation at all.

Prior-art status: the resolved issue 20261113T092956Z-timestream-influxdb-the-createdbinstance-duplicate-name-restore-round.md recorded this same timeout signature; the span it named was reduced (state.rs: TRANSITION_SPAN = Duration::from_millis(25); DbInstance::settle applies the owed change only on read; DeleteDbInstance removes the record while reporting DELETING) yet the check still times out, so the round trip is still not live. Code citations are for navigation only — the span value and the 60 s budget are this environment's values, not a requirement; the requirement is that the created instance reads AVAILABLE on the client's next read and the deleted name is immediately reusable.

## Steps to Reproduce
1. Boot the emulator and create the networking prerequisites (a VPC with at least two subnets and a security group) via EC2.
2. Call CreateDbInstance(name=<N>, password=<8-64 chars matching ^[a-zA-Z0-9]+$>, dbInstanceType="db.influx.medium", vpcSubnetIds=[<subnet-a>,<subnet-b>], vpcSecurityGroupIds=[<sg>], allocatedStorage=20, dbStorageType="InfluxIOIncludedT1", deploymentType="SINGLE_AZ", tags={...}); keep the returned id. Expect status CREATING.
3. Call CreateDbInstance again with the identical name and a different password; expect 409 ConflictException.
4. ListDbInstances and confirm exactly one instance owns <N>, the original id.
5. Poll GetDbInstance(identifier=<id>) on a client-chosen interval (the check uses 30 s) until status is AVAILABLE or FAILED; expect AVAILABLE on the first probe rather than after a full probe interval.
6. DeleteDbInstance(identifier=<id>) and poll GetDbInstance until the id no longer resolves (ResourceNotFoundException) or reports DELETED.
7. Call CreateDbInstance with the identical name <N> again; expect success with a NEW id and status CREATING, then poll it to AVAILABLE.
Observed: replaying the behavioral check that performs exactly these steps ends in "Check timed out after 60.0s" with nothing observed after step 3.
