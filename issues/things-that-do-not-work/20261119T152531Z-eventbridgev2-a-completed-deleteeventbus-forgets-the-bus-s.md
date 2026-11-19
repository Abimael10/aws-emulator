Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2.revoke-resource-marks-subscriber-terminal-and-outlives-bus-deletion
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|DescribeSubscriber|ResourceNotFoundException
Issue-Commit: 95422a199f71de611c9b4568260a34e03a6bfa1c

# eventbridgev2: a completed DeleteEventBus forgets the bus's revoked subscribers, so DescribeSubscriber answers 404 on a subscriber AWS keeps readable

> _Note: this issue was identified against the code base at commit `95422a199f71de611c9b4568260a34e03a6bfa1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSubscriber

## Description
Cause 1 of 1: when a bus's asynchronous deletion completes, the emulator deletes every subscriber of that bus from its store, including a REVOKED one, so a later DescribeSubscriber on that subscriber ARN is refused instead of answered.

AWS: Smithy — RevokeResource is documented as "Revokes a subscriber or an EventSource... Revocation is terminal: there is no operation that clears it. A revoked resource refuses mutating operations with InvalidStateException; delete stays available for cleanup." DeleteEventBus is documented as "Deletion is asynchronous: the bus moves to DELETING and disappears when complete... A bus with subscribers or event sources cannot be deleted" — a revoked subscriber no longer holds its bus, so the delete succeeds with it still present. DescribeSubscriber is @readonly and models ResourceNotFoundException (HTTP 404) only for a subscriber that does not exist. Recorded behavior: after RevokeResource on a subscriber, DeleteEventBus on its bus succeeds and the bus disappears, and DescribeSubscriber on the revoked subscriber still answers HTTP 200 with SubscriberArn equal to the ARN asked for (the subscriber record outlives the bus); the revoked subscriber is reachable for the record until it is explicitly deleted.

Emulator: DescribeSubscriber on the revoked subscriber, issued after the bus's DELETING span has elapsed, answers HTTP 404 ResourceNotFoundException with message "subscriber <SubscriberArn> does not exist" (the fixed text is "subscriber " + " does not exist"; the variable part is the SubscriberArn taken from the request). The earlier steps of the same check pass — RevokeResource sets Revoked true, DescribeSubscriber reports Revoked=true, and UpdateSubscriber on it is refused InvalidStateException "Subscriber is revoked and cannot be updated" — so only the survival of the record past the bus's deletion diverges.

Why: a client that revokes a misbehaving subscriber, tears the bus down, and then reads the subscriber back (audit, cleanup bookkeeping, or a waiter that distinguishes "revoked" from "gone") gets a 404 and concludes the subscriber was never there or was deleted; a cleanup path that treats 404 as "already deleted" and anything else as a failure diverges from AWS, and the terminal revoked state becomes unobservable the moment the bus is removed.

Fix: the bus-lifecycle settlement in this service's state — the completed-deletion branch that cascades subscriber removal (see Traces for the file and lines); a revoked subscriber must stay in the registry (and keep being projected by DescribeSubscriber) when its bus's deletion completes, rather than being forgotten with the bus.

Traces: eventbridgev2.revoke-resource-marks-subscriber-terminal-and-outlives-bus-deletion. Source citations: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/state.rs lines 511-521 (`settle_lifecycles` completed-deletion loop calls `forget_subscribers_of_bus(&arn)` unconditionally); services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/delete_event_bus.rs lines 198-224 (the delete only gates on un-revoked subscribers and schedules the DELETING transition). Related open issues that do NOT cover this cause: 20261113T174325Z-eventbridgev2-subscriber-reads-omit-stored-configuration-drop-updates.md (DeleteSubscriber leaves a subscriber readable) and 20261110T210822Z-eventbridgev2-3-net-new-behavioral-causes-deleteeventbus-is.md (DeleteEventBus gating on event sources).

## Evidence
Replayed check eventbridgev2.revoke-resource-marks-subscriber-terminal-and-outlives-bus-deletion:

botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the DescribeSubscriber operation: subscriber arn:aws:events:us-west-2:000000000000:subscriber/<subscriber-name>/<25-char id> does not exist

Expected (AWS): HTTP 200 DescribeSubscriberResponse whose SubscriberArn equals the ARN requested, with Revoked=true, after the bus's deletion has completed.
Actual (emulator): HTTP 404 ResourceNotFoundException; the subscriber record was removed together with the bus.
All preceding steps of the check matched AWS: fresh DescribeSubscriber had no Revoked flag, RevokeResource succeeded, DescribeSubscriber then reported Revoked=true, UpdateSubscriber was refused with InvalidStateException / "Subscriber is revoked and cannot be updated", and DeleteEventBus succeeded with the revoked subscriber present.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateEventBus(Name=<name>, ClientToken=<tok>) and wait until DescribeEventBus reports State=ACTIVE.
3. CreateSubscriber(Name=<name>, EventBusArn=<bus arn>, ClientToken=<tok>, InvokeConfiguration={RoleArn: <any role arn>, TargetArn: <any sqs queue arn>}).
4. RevokeResource(Arn=<subscriber arn>) — succeeds; DescribeSubscriber reports Revoked=true and UpdateSubscriber is refused InvalidStateException.
5. DeleteEventBus(EventBusArn=<bus arn>) and poll DescribeEventBus until it answers ResourceNotFoundException (the bus is gone).
6. DescribeSubscriber(SubscriberArn=<subscriber arn>): the emulator answers 404 ResourceNotFoundException "subscriber <arn> does not exist"; AWS answers 200 with the revoked subscriber.
