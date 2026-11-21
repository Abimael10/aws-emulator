Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds
Issue-Shape-Id: com.amazonaws.timestreaminfluxdb#AmazonTimestreamInfluxDB
Issue-Key: timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds|_error_detail#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: a3b58e14d285d1a9926a18260a24d7b68c00bc3c

# timestream-influxdb: RebootDbInstance on a still-CREATING DB instance is accepted (200, status REBOOTING) instead of refused with 400 ValidationException — the 1 ms transition span makes the rebootable-state guard unreachable

> _Note: this issue was identified against the code base at commit `a3b58e14d285d1a9926a18260a24d7b68c00bc3c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RebootDbInstance

## Description
One net-new root cause.

Cause 1 of 1: a reboot requested while the DB instance is still provisioning is ACCEPTED by the emulator and actually starts a reboot, where AWS refuses it. Failing check id: timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds.

Relationship to already-filed issues (neither covers this): the FIXED issue 20261113T235718Z-timestream-influxdb-2-net-new-causes-rebootdbinstance-resourcenotfoundexception.md, Cause 2, covered the WORDING of this refusal's ValidationException message; the handler now builds AWS's sentence (services/.../src/ops/reboot_db_instance.rs:237-243 calls st::resource_not_rebootable(NAMED_DB_INSTANCE, &input.identifier, st::arn_account_id(&record.arn))), so that message defect is addressed. This is NOT a reopen of it: the call is no longer refused at all, so the branch that builds that message is never entered. The divergence has moved from the message text to whether the refusal happens, and this is a different (over-permissive) defect, not the refused call that issue described. The other open timestream-influxdb issues (20261114T190856Z duplicate-name restore round trip, 20261116T094729Z InfluxDB v2 data plane, 20261022T105412Z TagResource) concern other operations.

AWS: Smithy (com.amazonaws.timestreaminfluxdb#AmazonTimestreamInfluxDB) models RebootDbInstance with input member `identifier` (target DbInstanceIdentifier, required, @length 3..64, @pattern ^[a-zA-Z0-9]+$), @idempotent, output member `status` (enum Status), and errors [AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400]. The model does not state when a reboot is permitted, so: recorded behavior only — a DB instance whose status is a transient provisioning status (CREATING) is not in a rebootable state, and AWS refuses the reboot with HTTP 400, Error.Code "ValidationException" and Error.Message "DbInstance with identifier <id> and customer id <accountId> is not in a rebootable state". Fixed text of that message: "DbInstance with identifier " + <id> + " and customer id " + <accountId> + " is not in a rebootable state"; the only variable parts are <id>, the identifier sent in the request, and <accountId>, the 12-digit account id that owns the instance (field 4 of its arn). AWS does not name the current or required status and does not use ConflictException for this state conflict. The refusal is also inert: the instance's status is unchanged by it (it remains CREATING, or AVAILABLE if provisioning has since completed) and never becomes REBOOTING. Once the instance reaches AVAILABLE the identical reboot succeeds (HTTP 200) and the instance returns to AVAILABLE. Implied by this: a DB instance is observably in a transient status for a client-visible interval after CreateDbInstance — a client that issues its next request right after the create still sees CREATING.

Emulator: CreateDbInstance followed immediately by RebootDbInstance(identifier=<new id>) answers HTTP 200 with the full DbInstance payload and status "REBOOTING" — no error at all — and the record is mutated into REBOOTING. Replay: AssertionError: expected the call to be rejected, but it succeeded and returned {... 'status': 'REBOOTING' ...} with HTTPStatusCode 200. The handler's guard itself is present and correct (reboot_db_instance.rs:237 `if !record.is_available()`), but it is preceded by record.settle(now) at line 230, and the record's settle is due only crate::state::TRANSITION_SPAN after the create. TRANSITION_SPAN is Duration::from_millis(1) (services/.../src/state.rs:228), i.e. one order of magnitude BELOW a client round trip, so every record has already settled to AVAILABLE by the time the client's very next request arrives. There is therefore no client-observable CREATING window anywhere in this service, the rebootable-state refusal is unreachable in practice, and the same holds for every other state-conflict refusal keyed to a transient status (DELETING/UPDATING/RESTORING).

Why: a client relying on AWS's behavior gets two things wrong. First, a request AWS rejects is accepted: code that handles the ValidationException (retry-until-rebootable loops, operator-facing errors, negative tests asserting the refusal) never sees it, and provisioning-safety logic that depends on the service serialising lifecycle actions is silently unexercised. Second, the accepted reboot mutates a provisioning instance into REBOOTING, a state transition real AWS never performs from CREATING, so a client that observes status while creating sees a lifecycle AWS cannot produce. Because the suite's assertion aborts here, the recorded behaviour it guards — that the refusal mutates nothing and that the identical reboot succeeds once AVAILABLE — stays unverified and must be re-replayed with the fix.

Fix: the service's transient-status lifecycle timing in the state layer (the DB-instance record's settle/transition clock), not the RebootDbInstance serializer and not its message construction: the transient status a lifecycle call announces must still be the record's status on the client's next request, so that a create followed by a reboot takes the handler's `!record.is_available()` branch and emits the modelled 400/ValidationException with AWS's rebootable-state sentence. The create path's dwell and the reboot handler's pre-guard settle are the components to change; file/line citations are under Traces.

Traces: timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds. Code: services/service-com-amazonaws-timestreaminfluxdb-amazontimestreaminfluxdb/src/state.rs:228 (TRANSITION_SPAN = Duration::from_millis(1)), state.rs:359-371 (settle / begin_transition), src/ops/reboot_db_instance.rs:230 (record.settle(now) before the guard) and :237-243 (the unreachable refusal branch).

## Evidence
Check timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds [mismatch]
  Expected (real AWS): CreateDbInstance(...) -> id; RebootDbInstance(identifier=<id>) issued while that instance is still CREATING -> HTTP 400, Error.Code = "ValidationException", Error.Message = "DbInstance with identifier <id> and customer id <accountId> is not in a rebootable state".
    Fixed text: "DbInstance with identifier " + <id> + " and customer id " + <accountId> + " is not in a rebootable state".
    Variable parts: <id> = the identifier sent in the request; <accountId> = the 12-digit owning account id (field 4 of the instance arn). No status names, no brackets, not ConflictException.
    The refusal is inert: a following GetDbInstance reports CREATING or AVAILABLE, never REBOOTING. Once AVAILABLE, the identical RebootDbInstance succeeds (200) and the instance settles back to AVAILABLE.
  Actual (emulator): HTTP 200 with a full DbInstance payload and status "REBOOTING" — no error raised, and the provisioning record was moved into REBOOTING.
  Replay output:
    File "check_timestream_influxdb_reboot_db_instance_conflict_while_creating_then_succeeds.py", line 39, in run
      code, message = _error_detail(lambda: influx.reboot_db_instance(identifier=instance_id))
    File "...", line 78, in _error_detail
      raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
    AssertionError: expected the call to be rejected, but it succeeded and returned {'id': '0fe1a20198', 'name': 'sim2real-rebootconflict-...', 'arn': 'arn:aws:timestream-influxdb:us-west-2:000000000000:db-instance/0fe1a20198', 'status': 'REBOOTING', ..., 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
  (The id, name, arn, endpoint, port and timestamps in that payload are per-run generated values and are not part of the requirement; the requirement is that this call is refused with the 400/ValidationException above and that no status change occurs.)

Mechanism in the checkout:
  services/service-com-amazonaws-timestreaminfluxdb-amazontimestreaminfluxdb/src/state.rs:228
    pub const TRANSITION_SPAN: Duration = Duration::from_millis(1);
    (its own doc comment states it is "NOT an AWS-documented duration (a real instance takes minutes)" and "an ORDER OF MAGNITUDE below the round trip a client needs to issue its next request")
  state.rs:359-364  settle(): if settles_at <= now -> status = AVAILABLE
  state.rs:368-371  begin_transition(): settles_at = now + TRANSITION_SPAN
  src/ops/reboot_db_instance.rs:230  record.settle(now);            // already AVAILABLE on the client's next call
  src/ops/reboot_db_instance.rs:237-243  if !record.is_available() { ... resource_not_rebootable(...) }   // unreachable in practice
  src/ops/reboot_db_instance.rs:249  record.begin_transition(st::STATUS_REBOOTING, now);  // taken instead

Model citation: RebootDbInstance — input `identifier` required, @length 3..64, @pattern ^[a-zA-Z0-9]+$, @idempotent; output includes `status` (Status enum); errors = [AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400]; ValidationException requires members message + reason. The model fixes the status/code of the refusal but not its text or the rebootable-state rule, so those come from the recording.

Related filed issues: FIXED 20261113T235718Z-timestream-influxdb-2-net-new-causes-rebootdbinstance-resourcenotfoundexception.md (Cause 2) fixed this refusal's message wording; the refusal itself is now never produced, which is the defect filed here.

## Steps to Reproduce
1. Boot the timestream-influxdb emulator.
2. CreateDbInstance(name=<unique>, password=<random>, dbInstanceType=db.influx.medium, vpcSubnetIds=[<two subnets>], vpcSecurityGroupIds=[<one sg>], allocatedStorage=20, dbStorageType=InfluxIOIncludedT1, deploymentType=SINGLE_AZ). Keep the returned `id` and `arn`; derive <accountId> from field 4 of `arn`.
3. Immediately — as the very next request — call RebootDbInstance(identifier=<id>).
4. Observe HTTP 200 and status "REBOOTING". Real AWS, where the instance is still CREATING at this point, answers HTTP 400 / Error.Code "ValidationException" / Error.Message "DbInstance with identifier <id> and customer id <accountId> is not in a rebootable state" and leaves the instance's status untouched.
5. Confirm the window is the cause: call GetDbInstance(identifier=<id>) as the next request after the create instead of rebooting — it already reports AVAILABLE rather than CREATING, because crate::state::TRANSITION_SPAN is 1 ms.
6. After the fix, also confirm the behaviour the assertion currently blocks: the refused reboot mutates nothing (GetDbInstance reports CREATING or AVAILABLE, never REBOOTING); then poll to AVAILABLE, call RebootDbInstance again, and confirm it succeeds (200) and the instance settles back to AVAILABLE.
7. Equivalent: replay timestream-influxdb.reboot-db-instance-conflict-while-creating-then-succeeds.
