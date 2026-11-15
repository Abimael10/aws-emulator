Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-update-event-bus-description-and-retention
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-update-event-bus-description-and-retention
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|DescribeEventBus|2xx->2xx|$.Description
Issue-Related: 20261111T025659Z-eventbridgev2-event-bus-and-event-source-updates-are.md
Issue-Commit: eedcbd539e91aa3bbb4add86fe8b39999130d759

# EventBridgeV2 UpdateEventBus mutations never reach the stored bus, so DescribeEventBus reports pre-update state

> _Note: this issue was identified against the code base at commit `eedcbd539e91aa3bbb4add86fe8b39999130d759`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEventBus

## Description
ONE net-new cause on the bootstrapped eventbridgev2 service (sdkId EventBridgeV2, rpcv2Cbor). An accepted UpdateEventBus never mutates the stored event bus, so every later read of that bus reports it exactly as it was created. The read handler itself is faithful - it reports the record it was given; the write is the part that never happens.

Cause 1 of 1: DescribeEventBus reports the pre-update Description and StorageConfiguration.RetentionPeriodInDays after a successful UpdateEventBus (fixture eventbridgev2-update-event-bus-description-and-retention, step task#4; the same cluster also covers task#6 and task#8, the two later reads in the same flow).
AWS: Smithy (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json): UpdateEventBus input {EventBusArn @required @length 1..1011 @pattern ^arn:aws...:event-busv2/..., Description @length 0..512, EncryptionConfiguration, StorageConfiguration}, output {EventBusArn, Name, Description, EncryptionConfiguration, StorageConfiguration->StorageConfigurationOutput, State (BusState incl. UPDATING), StateReason, LastModifiedTime}, errors ConcurrentModificationException 409 / InvalidStateException 409 / ResourceNotFoundException 404 / AccessDeniedException 403 / InvalidInputException 400 / ThrottlingException 429 / InternalException 500; operation doc: "Updates an event bus. The update is asynchronous: the bus moves to UPDATING and returns to ACTIVE when the change is applied. Fields omitted from the request are left unchanged." DescribeEventBus (@readonly) output carries Description and StorageConfiguration (StorageConfigurationOutput). So after an UpdateEventBus that supplies a new Description and a new StorageConfiguration.RetentionPeriodInDays, AWS settles the bus back to ACTIVE and every subsequent DescribeEventBus reports the NEW values, leaving members absent from the update request unchanged.
Emulator: all three reads in the flow answer 200 with the creation-time values - $.Description value_mismatch (expected the updated description, got the description the bus was created with) and $.StorageConfiguration.RetentionPeriodInDays value_mismatch (expected the updated retention period, got the created one). The UpdateEventBus call earlier in the flow is still answered by the scaffold: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/update_event_bus.rs still carries the "SCAFFOLDED, NOT IMPLEMENTED" marker and its handle body returns crate::scaffolded::Scaffolded::not_implemented("UpdateEventBus"), so no write ever reaches the stored bus record. (The 400 ValidationException the same operation produces for an out-of-range RetentionPeriodInDays comes from the centralized pre-handler constraint pass, not from this handler, and is filed separately.)
Why: a client that updates a bus and reads it back - to confirm the change, to drive a waiter on State/Description, or to recompute a retention window from RetentionPeriodInDays - sees its update silently discarded while the call it made was accepted. From that point on the emulator's bus state diverges from AWS for every later step of the flow, and any logic keyed on the updated description or retention period never fires.
Fix: the UpdateEventBus handler and the event-bus record in this service's state - apply Description / EncryptionConfiguration / StorageConfiguration to the stored bus, move it through UPDATING back to ACTIVE, leave omitted members unchanged, and return the post-update projection; the read path then needs no change. The auto-filed scaffold-stub issue for UpdateEventBus covers the 501 ANSWER only, not this state consequence, so this read divergence must be re-replayed against the step above to confirm the fix.
Traces: eventbridgev2-update-event-bus-description-and-retention#task#4 (representative), #task#6, #task#8. Code: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/update_event_bus.rs:3-12 (SCAFFOLDED marker) and :179-197 (handle returns not_implemented); src/ops/describe_event_bus.rs and the stored bus record in src/state.rs.

Related open issue that does NOT own this replay key: 20261111T025659Z-eventbridgev2-event-bus-and-event-source-updates-are.md describes the same family of un-applied updates, but the replay reports this cluster as unowned (owned_by empty) at this commit - its recorded keys no longer match this cluster's key eventbridgev2|DescribeEventBus|status=200->200|keys=$.Description,$.StorageConfiguration.RetentionPeriodInDays - so this step is filed here to keep it gated.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 200 steps total, 78 passed, 120 mismatched, 2 blocked, 28 divergence clusters, score 39.0%. This is the ONLY cluster in the whole replay with an empty owned_by and an empty stub_path; every other divergence is owned by an open issue or blocked by an unimplemented operation.

Cluster: service=eventbridgev2, operation DescribeEventBus, verdict mismatch, expected_status 200, actual_status 200, fixture_id eventbridgev2-update-event-bus-description-and-retention, step_key task#4, affected_count 3 (task#4, task#6, task#8), divergence_key service=eventbridgev2|DescribeEventBus|status=200->200|keys=$.Description,$.StorageConfiguration.RetentionPeriodInDays.

detail, verbatim:
  $.Description: value_mismatch (expected 'updated description', got 'initial description')
  $.StorageConfiguration.RetentionPeriodInDays: value_mismatch (expected 7, got 3)
('updated description'/'initial description' and 7/3 are this recording's values for the flow's new and created values; the requirement is that the read reports whatever the preceding UpdateEventBus supplied, not those literals.)

Emulator source at this commit: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/update_event_bus.rs line 4 carries '//! SCAFFOLDED, NOT IMPLEMENTED' and line 196 is Err(crate::scaffolded::Scaffolded::not_implemented("UpdateEventBus")), so the handler performs no mutation.

Model: smithy_query(operation="UpdateEventBus,DescribeEventBus") on services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json - UpdateEventBus is @idempotent, input {EventBusArn @required, Description @length 0..512, EncryptionConfiguration, StorageConfiguration}; DescribeEventBus is @readonly with output members Description and StorageConfiguration (StorageConfigurationOutput).

## Steps to Reproduce
1. git_clone the repo, boot the emulator built from it, and replay trace set simparity-wire-aws-v1.
2. Fixture eventbridgev2-update-event-bus-description-and-retention: CreateEventBus with a Description and StorageConfiguration.RetentionPeriodInDays (the recording uses 'initial description' and 3) and wait for State ACTIVE.
3. Call UpdateEventBus(EventBusArn=<arn>, Description=<new description>, StorageConfiguration={RetentionPeriodInDays: <new value>}) - the emulator answers 501 NotImplemented from the scaffold instead of accepting the update.
4. Call DescribeEventBus on the same ARN (step task#4, repeated at task#6 and task#8): the emulator answers 200 still carrying the creation-time Description and RetentionPeriodInDays, where AWS reports the values supplied in step 3 once the bus has returned to ACTIVE.
