Issue-Source: trace-sweep-divergence
Issue-Trace: personalize-event-tracker-lifecycle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: personalize-event-tracker-lifecycle
Issue-Shape-Id: com.amazonaws.personalize#AmazonPersonalize
Issue-Key: personalize|CreateEventTracker|4xx->2xx|$.__type
Issue-Key: personalize|CreateEventTracker|4xx->2xx|$.message
Issue-Key: personalize|CreateEventTracker|4xx->2xx|status
Issue-Key: personalize|CreateEventTracker|2xx->4xx|$.eventTrackerArn
Issue-Key: personalize|CreateEventTracker|2xx->4xx|$.trackingId
Issue-Key: personalize|CreateEventTracker|2xx->4xx|status
Issue-Commit: 334bc8277a596300b2e484d224a654f1308ad2e0

# CreateEventTracker ignores the INTERACTIONS-dataset precondition on its dataset group (2 causes)

> _Note: this issue was identified against the code base at commit `334bc8277a596300b2e484d224a654f1308ad2e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventTracker

## Description
`CreateEventTracker` on `personalize` never checks that the dataset group it is asked to attach to already holds an INTERACTIONS (Item interactions) dataset. The event-tracker lifecycle fixture therefore diverges twice, in opposite directions: the create that AWS refuses is admitted, and the create that AWS accepts is then refused. Both causes live in the same handler and the same precondition, so they are filed together; 2 causes must be closed.

Cause 1 of 2: CreateEventTracker succeeds on a dataset group that has no INTERACTIONS dataset (fixture `personalize-event-tracker-lifecycle`, step `task#3`).
  AWS: an event tracker appends event data to the Item interactions dataset of the dataset group it names (Smithy: `CreateEventTracker` docs — "Amazon Personalize then appends the event data to the Item interactions dataset of the dataset group you specify in your event tracker"; input member `datasetGroupArn` is @required; modeled errors include `ResourceNotFoundException`). Until that dataset exists the group cannot receive event data, so the create is refused. The recorded response is HTTP 400 with `__type` `ResourceNotFoundException` and `message` `Missing INTERACTIONS dataset with datasetgroup: [<datasetGroupArn>]`. In that message the fixed text is `Missing INTERACTIONS dataset with datasetgroup: [` ... `]`; the only variable part is the dataset-group ARN taken verbatim from the request's `datasetGroupArn`. (Note: the model declares `ResourceNotFoundException` with httpError 404, but the recorded wire status for this refusal is 400 — the recording is the wire truth here.)
  Emulator: answers HTTP 200 and mints `eventTrackerArn` + `trackingId`, with no `__type` and no `message`. `services/service-com-amazonaws-personalize-amazonpersonalize/src/ops/create_event_tracker.rs` only looks the dataset group up in `state.dataset_groups` for its region, then checks tracker-per-group and name collisions and the stored-tracker limit; nothing inspects the group's datasets.
  Why: a client that relies on AWS rejecting this create believes it has a usable tracker and a usable `trackingId`, and only discovers the dataset group cannot accept interactions later, when ingestion through `personalize-events` silently has nowhere to land. Error-path tests that assert the refusal see a success instead.
  Fix: the `CreateEventTracker` handler's precondition/validation step — after resolving `datasetGroupArn`, require the group to hold a dataset of type INTERACTIONS and otherwise return the modeled `ResourceNotFoundException` at the recorded status with the message template above.
  Traces: `personalize-event-tracker-lifecycle#task#3`.

Cause 2 of 2: the legitimate CreateEventTracker, issued once the dataset group does hold its INTERACTIONS dataset, is refused (fixture `personalize-event-tracker-lifecycle`, step `task#6`).
  AWS: with the INTERACTIONS dataset in place, `CreateEventTracker` succeeds — HTTP 200 carrying `eventTrackerArn` (Arn, pattern `^arn:([a-z\d-]+):personalize:.*:.*:.+$`) and `trackingId` (Smithy: `CreateEventTracker` output members `eventTrackerArn`, `trackingId`; the operation is @idempotent and the tracker starts in CREATE PENDING, not ACTIVE).
  Emulator: answers HTTP 400 with no `eventTrackerArn` and no `trackingId`, so the lifecycle never obtains a tracking id and every later step that needs one cannot run. Hypothesis: because cause 1 admitted a tracker for this same dataset group at the earlier step, the tracker-per-group collision check in `create_event_tracker.rs` ("Only one event tracker can be associated with a dataset group") or the input-validation stage now rejects the correct create; the replay output reports only the status and the absent output members, not the emulator's error code.
  Why: the supported path is unreachable. A client doing the documented sequence — create dataset group, create the INTERACTIONS dataset, then create the event tracker — gets an error where AWS gives it a tracker, so no PutEvents ingestion can be addressed at all.
  Fix: the `CreateEventTracker` handler — once the precondition from cause 1 is enforced no tracker exists for the group at this point, so the create must mint and return `eventTrackerArn`/`trackingId` at 200; whichever refusal path is taken here must not fire for a group whose only prior create was correctly rejected.
  Traces: `personalize-event-tracker-lifecycle#task#6`.

Related open issues, neither of which covers these causes: `20261022T000000Z-personalize-control-plane-does-not-register-event-trackers-or-datasets-for-ingestion.md` (wiring the minted tracking id into the `personalize-events` registry — that wiring is present in this handler and is not what diverges here) and `20261108T163508Z-personalize-create-operation-invalidinputexception-messages-do-not-match.md` (InvalidInputException wording on other create operations).

## Evidence
Replay of trace set `simparity-wire-aws-v1` against the emulator built from this checkout:

Cause 1 — fixture `personalize-event-tracker-lifecycle`, step `task#3`, operation CreateEventTracker:
  expected status 400, actual status 200
  $.__type: missing (expected 'ResourceNotFoundException', got None)
  $.message: missing (expected 'Missing INTERACTIONS dataset with datasetgroup: [arn:aws:personalize:us-west-2:000000000000:dataset-group/sim2real-probe-personalize-dg-2ff36b09]', got None)
  (the ARN shown is this run's generated dataset group; the requirement is the template, with the request's own datasetGroupArn inside the brackets)

Cause 2 — fixture `personalize-event-tracker-lifecycle`, step `task#6`, operation CreateEventTracker:
  expected status 200, actual status 400
  $.eventTrackerArn: missing (expected a shaped Arn, got None)
  $.trackingId: missing (expected a shaped tracking id, got None)

Code: `services/service-com-amazonaws-personalize-amazonpersonalize/src/ops/create_event_tracker.rs` lines 183-224 resolve the dataset group and run the collision/limit checks; there is no INTERACTIONS-dataset precondition anywhere in `handle`.

Smithy (`services/service-com-amazonaws-personalize-amazonpersonalize/smithy-model/com-amazonaws-personalize-amazonpersonalize.json`): CreateEventTracker input {name @required, datasetGroupArn @required}, output {eventTrackerArn, trackingId}, errors InvalidInputException/LimitExceededException/ResourceAlreadyExistsException/ResourceInUseException/ResourceNotFoundException/TooManyTagsException; operation docs state the tracker appends event data to the dataset group's Item interactions dataset and that only one tracker may be associated with a dataset group.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `personalize-event-tracker-lifecycle`.
2. Cause 1 — at step `task#3`, call CreateEventTracker with a `datasetGroupArn` naming a dataset group that exists but holds NO INTERACTIONS dataset. Observed: 200 with eventTrackerArn/trackingId. Expected: 400, `__type` `ResourceNotFoundException`, `message` `Missing INTERACTIONS dataset with datasetgroup: [<the datasetGroupArn sent>]`.
3. Create the group's INTERACTIONS dataset (CreateDataset with datasetType INTERACTIONS in that dataset group).
4. Cause 2 — at step `task#6`, call CreateEventTracker again for that dataset group. Observed: 400 with no eventTrackerArn and no trackingId. Expected: 200 carrying both `eventTrackerArn` and `trackingId`.
