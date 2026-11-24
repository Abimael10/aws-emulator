Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-capacity-reservation-errors
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-capacity-reservation-errors
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|CreateDBCapacityReservation|2xx->4xx|status
Issue-Key: rds|DescribeDBCapacityReservations|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: bd074da195e985a650177e1d18021ce4ba224aa9

# rds DB capacity reservations: a reservation AWS creates is refused 400, and the not-found refusal is emulator-authored prose

> _Note: this issue was identified against the code base at commit `bd074da195e985a650177e1d18021ce4ba224aa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCapacityReservation, DescribeDBCapacityReservations

## Description
Two net-new root causes in the emulated RDS DB capacity reservation family, both from the wire replay of fixture rds-db-capacity-reservation-errors, neither blocked by an unimplemented scaffold nor owned by an open issue (list_existing_issues finds no issue naming DBCapacityReservation; the only nearby rds entries are 20261104T053605Z-rds-modifycurrentdbclustercapacity-stub.md, which is a different operation, and 20261119T043432Z-rds-5-net-new-behavioral-causes-two-automated.md, which covers automated-backup messages). They are independent and a fixer must close BOTH before submitting.

Cause 1 of 2: CreateDBCapacityReservation refuses a reservation AWS creates — 400 instead of 200.
AWS: Smithy CreateDBCapacityReservation takes the required DBCapacityReservationName (@length 1..63, pattern ^[a-z](?:-?[a-z0-9]+){0,15}$), DBInstanceClass (@length 5..30, pattern ^db\.[0-9a-z]{2,6}\.[0-9a-z\.]{4,20}$), Engine, CapacityPerAvailabilityZone and AvailabilityZones, plus the optional StartDate / CommitmentDuration / EndDate / LicenseModel / Tags, and returns 200 with a DBCapacityReservation whose State is one of the modeled DBCapacityReservationState values (an immediate reservation reports 'pending' then 'active'; a future-dated one 'assessing' then 'scheduled'). Its only modeled refusals are DBCapacityReservationAlreadyExistsFault, DBCapacityReservationUnsupportedFault, InstanceQuotaExceededFault and InsufficientDBInstanceCapacityFault (all 400). For a valid, first-time configuration of a reserve-able engine and a published DB instance class, AWS creates the reservation and answers 200 — this is the fixture's SETUP step, i.e. the baseline every error probe in that fixture is then run against.
Emulator: the create is refused with HTTP 400 instead of answering 200 with the reservation envelope, so no reservation exists and the fixture's error-path steps that depend on it cannot run (this replay reports 4 blocked steps).
Why: a client cannot reserve capacity at all. Every downstream call that takes a DBCapacityReservationIdentifier (DescribeDBCapacityReservations, ModifyDBCapacityReservation, CancelDBCapacityReservation, and a CreateDBInstance that wants to launch into reserved capacity) is unreachable, and the 400 misreports a supported configuration as unsupported or over quota.
Hypothesis (the replay reports only the status, not the body): one of the pre-store refusal gates in the handler fires on a configuration AWS accepts. The candidates, in the order the handler applies them, are the DBCapacityReservationUnsupported gate on Engine (crate::state::reserves_capacity_for_engine, whose list is the model's 18 valid values), the DBCapacityReservationUnsupported gate on DBInstanceClass (crate::ops::describe_orderable_db_instance_options::publishes_instance_family, which admits only the instance families this emulator's own orderable-option tables publish — a class in a family AWS offers but those tables omit is refused here), the Engine/LicenseModel InvalidParameterCombination gate, and the future-dated gates (check_reservation_start_date, check_commitment_duration, future_dated_class_is_eligible, check_future_dated_reserved_capacity). The requirement is that a configuration AWS accepts is accepted: the 200 envelope, not any one of those gates being deleted.
Fix: the validation and refusal gates of the CreateDBCapacityReservation handler, and the family/engine tables they consult.
Traces: rds-db-capacity-reservation-errors#setup#1 (CreateDBCapacityReservation, expected 200, got 400).

Cause 2 of 2: DescribeDBCapacityReservations' not-found refusal is reworded.
AWS: Smithy DescribeDBCapacityReservations models exactly one error, DBCapacityReservationNotFoundFault (awsQueryError code DBCapacityReservationNotFound, httpResponseCode 404, doc "DBCapacityReservationIdentifier doesn't refer to an existing DB capacity reservation"). When the request names a reservation that does not exist, the message is "The specified DBCapacityReservation '<named>' does not exist." — the fixed text is "The specified DBCapacityReservation '" … "' does not exist.", including the single quotes and the trailing period, and its ONLY variable part is the value the request sent in DBCapacityReservationIdentifier.
Emulator: the HTTP status (404) and the error code are right, but the message is "DBCapacityReservation <named> not found." — different fixed text, a different frame, and the identifier is unquoted.
Why: a client that classifies, logs or alarms on this refusal by its message text sees an unrecognised sentence for a condition AWS words one way, and a parser that lifts the identifier out of AWS's quoted sentence finds nothing to extract.
Fix: the refusal wording helper the DescribeDBCapacityReservations lookup uses — it is emulator-authored by design, see services/service-com-amazonaws-rds-amazonrdsv19/src/state/capacity_reservations.rs lines 114-128 (db_capacity_reservation_not_found_message, which formats "DBCapacityReservation {named} not found." and is documented as borrowing the DB-shard-group wording). Every other caller of that helper must word the fault the same way.
Traces: rds-db-capacity-reservation-errors#task#3 (DescribeDBCapacityReservations, 404/404, message mismatch).

The reservation names, identifiers and account/region values quoted in the evidence are per-run values that fill these templates; only the statuses, error codes, the fixed message text and the request-derived variable part named above are requirements.

## Evidence
Cause 1 — fixture rds-db-capacity-reservation-errors, step setup#1, operation CreateDBCapacityReservation:
  expected status: 200 (DBCapacityReservation envelope)
  actual status:   400
  replay detail:   "status: expected 200, got 400"
  divergence_key:  service=rds|CreateDBCapacityReservation|status=200->400|keys=status
  Consequence in the same replay: 4 steps blocked, because the reservation the fixture's later error probes address was never created.

Cause 2 — fixture rds-db-capacity-reservation-errors, step task#3, operation DescribeDBCapacityReservations:
  expected status 404, actual status 404 (error code DBCapacityReservationNotFound matches)
  $.ErrorResponse.Error.Message: value_mismatch
    expected: "The specified DBCapacityReservation 'sim2real-probe-rds-crmiss-a5403560' does not exist."
    actual:   "DBCapacityReservation sim2real-probe-rds-crmiss-a5403560 not found."
  divergence_key: service=rds|DescribeDBCapacityReservations|status=404->404|keys=$.ErrorResponse.Error.Message
  Fixed text required: "The specified DBCapacityReservation '" + <DBCapacityReservationIdentifier from the request> + "' does not exist."; 'sim2real-probe-rds-crmiss-a5403560' is this run's identifier only.
  Source of the diverging wording: services/service-com-amazonaws-rds-amazonrdsv19/src/state/capacity_reservations.rs:126-128 — `format!("DBCapacityReservation {named} not found.")`.

## Steps to Reproduce
All calls against the emulator's rds front door (service token 'rds', crate services/service-com-amazonaws-rds-amazonrdsv19), signed for us-west-2, using the ec2/query (awsQuery) protocol as the fixture does.

Cause 1: replay fixture rds-db-capacity-reservation-errors and watch step setup#1 — CreateDBCapacityReservation with a fresh DBCapacityReservationName, a reserve-able Engine, a DB instance class of the modeled pattern, CapacityPerAvailabilityZone and one or more AvailabilityZones of the signed region. AWS answers 200 with the DBCapacityReservation (State 'pending' or 'active', DBCapacityReservationIdentifier prefixed 'dbcr-', DBCapacityReservationArn, ReservedCapacityPerAvailabilityZone echoing the request); the emulator answers 400. Inspect the returned Error/Code to see which gate fired, then make that configuration succeed.

Cause 2: DescribeDBCapacityReservations(DBCapacityReservationIdentifier='<some-identifier-that-was-never-created>'). Expect HTTP 404, Error.Code DBCapacityReservationNotFound and Error.Message exactly "The specified DBCapacityReservation '<that value>' does not exist.". The emulator returns the right status and code with "DBCapacityReservation <that value> not found.". Re-replay rds-db-capacity-reservation-errors#task#3 to confirm.
