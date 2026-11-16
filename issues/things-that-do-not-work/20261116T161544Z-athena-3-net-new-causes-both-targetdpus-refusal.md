Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.athena-capacity-reservation-below-minimum-dpus-rejected athena.capacity-reservation-target-dpus-below-minimum-rejected athena.capacity-reservation-update-below-minimum-rejected athena.capacity-reservation-negative-dpu-target-rejected athena.update-capacity-reservation-negative-target-dpus-rejected athena.list-query-executions-out-of-range-max-results-rejected
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.athena-capacity-reservation-below-minimum-dpus-rejected|run#6|AssertionError|assert message == "Capacity must be requested in increments of 4.", repr(message)
Issue-Key: athena.capacity-reservation-target-dpus-below-minimum-rejected|run#8|AssertionError|assert message == "Capacity must be requested in increments of 4.", repr(message)
Issue-Key: athena.capacity-reservation-update-below-minimum-rejected|run#13|AssertionError|assert message == "Capacity must be requested in increments of 4.", repr(message)
Issue-Key: athena|CreateCapacityReservation|InvalidRequestException
Issue-Key: athena.update-capacity-reservation-negative-target-dpus-rejected|run#12|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: athena.list-query-executions-out-of-range-max-results-rejected|run#6|AssertionError|assert "QueryExecutionIds" in resp, (
Issue-Commit: 7f3aa4ec0dd216b0c43fd64d8ea5fd6d8762b08f

# athena: 3 net-new causes — both TargetDpus refusal messages on Create/UpdateCapacityReservation are the generic schema-range text, and ListQueryExecutions omits QueryExecutionIds for an empty page

> _Note: this issue was identified against the code base at commit `7f3aa4ec0dd216b0c43fd64d8ea5fd6d8762b08f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityReservation

## Description
Three net-new root causes on Athena's capacity-reservation control plane and on ListQueryExecutions, all reached through the behavioral suite (report heading `### athena`). This bundle carries 3 causes; ALL THREE must be closed before submitting.

Related already-open issues, named here so the fixer can join them up (neither carries these replay keys, and the suite did not mark these checks OWNED): 20261104T163600Z-athena-constraint-refusals-raise-an-unmodeled-validationexception-instead.md (its causes 1, 2 and 6 cover the ERROR CODE of exactly these calls — the emulator now answers InvalidRequestException, so that part has landed; what is left, and what this issue is about, is the MESSAGE text and the ListQueryExecutions output shape) and 20261111T163757Z-athena-capacity-reservation-lifecycle-refusals-diverge-delete-state.md cause 3 (same TargetDpus rule, UpdateCapacityReservation side, also only up to the error code). This is NOT a reopen: the recorded AWS behavior for these calls is itself a 400 refusal and the emulator does refuse with the modeled code; only the message/payload diverges.

Cause 1 of 3: a positive TargetDpus below 4 is refused with the framework's generic range message instead of AWS's increments-of-4 message.
  AWS: Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): CreateCapacityReservation.TargetDpus and UpdateCapacityReservation.TargetDpus are @required, target TargetDpusInteger with @range min 4; both operations bind only InvalidRequestException (client, HTTP 400) and InternalServerException (500). Recorded behavior: a TargetDpus that is positive but below the minimum (the checks exercise 1) is refused 400 InvalidRequestException with Message "Capacity must be requested in increments of 4." — entirely FIXED text, with no request value, member name or limit interpolated — and nothing is created (Create) / TargetDpus is left unchanged (Update).
  Emulator: 400 InvalidRequestException with Message "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 4" — the centralized deserialize→validate step's rendering of the schema's @range trait, which fires before the handler runs. Same status, same code, different message class.
  Why: InvalidRequestException is the ONLY client error these operations model, so the Message is the sole discriminator between the DPU-granularity rule and any other bad input. A client that reads it to tell the user "request capacity in increments of 4" (or that keys retry/round-up logic on AWS's fixed sentence) never matches, and the granularity rule itself is invisible — the generic text claims the only problem is a minimum.
  Fix: TargetDpus validation in the CreateCapacityReservation and UpdateCapacityReservation handlers (or a shared Athena capacity-reservation validator they both call), applied BEFORE the schema-driven @range refusal can render its own text, so the operation's modeled InvalidRequestException carries AWS's wording.
  Traces: athena.athena-capacity-reservation-below-minimum-dpus-rejected (Create, TargetDpus=1), athena.capacity-reservation-target-dpus-below-minimum-rejected (Create, TargetDpus=1), athena.capacity-reservation-update-below-minimum-rejected (Update on an ACTIVE reservation, TargetDpus=1). Code: services/service-com-amazonaws-athena-amazonathena/src/ops/create_capacity_reservation.rs lines 132-193 — the handler validates tags, duplicate name and the reservation count, and never inspects input.target_dpus; services/service-com-amazonaws-athena-amazonathena/src/ops/update_capacity_reservation.rs, same gap.

Cause 2 of 3: a zero or negative TargetDpus is refused with the modeled minimum (4) where AWS's frontend reports the minimum as 1.
  AWS: Smithy: as cause 1 (TargetDpusInteger @range min 4, errors InvalidRequestException/InternalServerException only). Recorded behavior: TargetDpus of 0 or a negative value is refused 400 InvalidRequestException with Message "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 1". Fixed text is "1 validation error detected: Value at '" … "' failed to satisfy constraint: Member must have value greater than or equal to " …; the variable parts are the member name as the wire names it ('targetDpus') and the limit as AWS's frontend states it for this class, 1 — NOT the modeled 4, and with no echo of the submitted value. The rejected create leaves no reservation under that Name and the rejected update leaves TargetDpus unchanged.
  Emulator: the same envelope but "… greater than or equal to 4", i.e. the schema's @range minimum. So AWS answers two DIFFERENT messages for TargetDpus<=0 and 1<=TargetDpus<4, and the emulator answers one message for both.
  Why: the two refusals are distinct classes for a caller — an unallocatable (non-positive) capacity request versus a request that must be rounded up to a multiple of 4 — and a client that reads the limit out of the message to repair its request is told the wrong bound. Anything matching AWS's text for the non-positive case fails to match at all.
  Fix: same place as cause 1 — the Create/UpdateCapacityReservation TargetDpus validation must branch on the value: non-positive gets the "greater than or equal to 1" validation-envelope message, positive-but-below-4 gets the increments-of-4 message, and neither may be left to the generic schema @range rendering.
  Traces: athena.capacity-reservation-negative-dpu-target-rejected (Create TargetDpus=0 and Update TargetDpus=-5), athena.update-capacity-reservation-negative-target-dpus-rejected (Update TargetDpus=0 on an ACTIVE reservation).

Cause 3 of 3: ListQueryExecutions omits the QueryExecutionIds member entirely when the page carries no ids.
  AWS: Smithy: ListQueryExecutions output QueryExecutionIds (QueryExecutionIdList) and NextToken; input MaxResults (MaxQueryExecutionsCount, @range 0..50); errors InvalidRequestException (400) / InternalServerException (500). Recorded behavior: MaxResults=50 — the documented boundary — is ACCEPTED and the 200 response carries the QueryExecutionIds member; the out-of-range case (MaxResults=51) is refused 400 InvalidRequestException with "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 50" (that half already matches). Recorded behavior only for the empty case: the member is present in the response body rather than suppressed.
  Emulator: the accepted MaxResults=50 call returns 200 with a body carrying NO QueryExecutionIds member at all (the check sees keys=['ResponseMetadata'] only). The handler suppresses it deliberately: `query_execution_ids: (!page.is_empty()).then_some(page)`, justified by the list's @length min 1.
  Why: a client that reads resp["QueryExecutionIds"] — the normal way to page a query history, including the SDK paginators — raises KeyError/NPE on an empty history instead of seeing an empty page, so "no queries yet" is indistinguishable from a malformed response and listing code has to special-case the emulator.
  Fix: the ListQueryExecutions serializer/handler output construction — emit the QueryExecutionIds member for an accepted page even when it holds no ids, rather than keying its presence on the list's non-emptiness.
  Traces: athena.list-query-executions-out-of-range-max-results-rejected (the positive control after the MaxResults=51 refusal). Code: services/service-com-amazonaws-athena-amazonathena/src/ops/list_query_executions.rs lines 216-221.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, all six failing checks listed under the report heading `### athena`, none marked ALREADY FILED and none marked OWNED.

Cause 1 — status 400 and code InvalidRequestException agree on both sides; the Message diverges:
  expected Message: 'Capacity must be requested in increments of 4.'
       got Message: "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 4"
  athena.athena-capacity-reservation-below-minimum-dpus-rejected — AssertionError at check line 31 on `assert message == "Capacity must be requested in increments of 4."` for CreateCapacityReservation(Name=<name>, TargetDpus=1).
  athena.capacity-reservation-target-dpus-below-minimum-rejected — AssertionError at check line 34, same assertion, same actual message.
  athena.capacity-reservation-update-below-minimum-rejected — AssertionError at check line 61 for UpdateCapacityReservation(Name=<ACTIVE reservation>, TargetDpus=1) after the post-ACTIVE update cooldown had passed, same actual message.

Cause 2 — status 400 and code InvalidRequestException agree; the limit named in the Message diverges:
  athena.update-capacity-reservation-negative-target-dpus-rejected — AssertionError at check line 66: expected message "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 1", got "… greater than or equal to 4" (UpdateCapacityReservation TargetDpus=0).
  athena.capacity-reservation-negative-dpu-target-rejected — AssertionError at check line 76 (helper _assert_rejected_unallocatable) asserting the same "… greater than or equal to 1" text, got "… greater than or equal to 4" (CreateCapacityReservation TargetDpus=0).
  All of these calls were issued through a botocore client with parameter_validation=False, so the refusal graded is the emulator's and not the SDK's.

Cause 3 — athena.list-query-executions-out-of-range-max-results-rejected: the MaxResults=51 refusal matched (400 InvalidRequestException with AWS's maxResults constraint message), then AssertionError at check line 33: "expected QueryExecutionIds in the response for MaxResults=50 (the documented boundary), got keys=['ResponseMetadata']". The reservation names and ids in these runs are per-run unique_name values and are not part of any requirement; only the message templates and the member's presence are.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, replay suite simparity-behavioral-aws-v1 and inspect the six checks named in trace_ids.

By hand against the emulator's Athena endpoint (any region/credentials). Use a botocore client with Config(parameter_validation=False) for every TargetDpus and MaxResults call below: botocore's generated model carries the same @range traits and would otherwise refuse locally, so the refusal observed must be the emulator's.

A. Cause 1 (create) — CreateCapacityReservation(Name=<name matching ^[a-zA-Z0-9._-]+$>, TargetDpus=1). Observe 400 InvalidRequestException with "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 4"; AWS answers 400 InvalidRequestException "Capacity must be requested in increments of 4." Then GetCapacityReservation(Name=<name>) must report the name unknown (nothing was created).

B. Cause 1 (update) — CreateCapacityReservation(Name=<name2>, TargetDpus=4); poll GetCapacityReservation until Status=ACTIVE; retry UpdateCapacityReservation(Name=<name2>, TargetDpus=1) until the post-ACTIVE cooldown message stops coming back. Observe the same generic range message; AWS answers "Capacity must be requested in increments of 4." and leaves TargetDpus=4.

C. Cause 2 — CreateCapacityReservation(Name=<name3>, TargetDpus=0) and, on an ACTIVE reservation, UpdateCapacityReservation(TargetDpus=0) and UpdateCapacityReservation(TargetDpus=-5). Each returns "… greater than or equal to 4"; AWS returns "1 validation error detected: Value at 'targetDpus' failed to satisfy constraint: Member must have value greater than or equal to 1" for all three, creates nothing and changes nothing.

D. Cause 3 — ListQueryExecutions(MaxResults=51): 400 InvalidRequestException with AWS's maxResults constraint message (already correct). Then ListQueryExecutions(MaxResults=50) against a workgroup with no query history (e.g. the primary workgroup of a freshly booted emulator): observe a 200 body with no QueryExecutionIds member; AWS's response carries the member. Run a query in that workgroup first and the member appears, which confirms the presence is keyed on the page being non-empty.
