Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2__iam__sqs.subscriber-on-failure-configuration-writes-dead-letter-record-to-sqs
Issue-Key: eventbridgev2__iam__sqs.subscriber-on-failure-configuration-writes-dead-letter-record-to-sqs|run#24|KeyError|assert record["busArn"] == bus_arn, f"record busArn {record['busArn']!r} != {bus_arn!r}"
Issue-Commit: e85b3d4e306a1fc3f303977481cd6410fa541f01

# eventbridgev2__iam__sqs: the subscriber on-failure dead-letter record omits busArn (and the rest of its failure-describing fields are unverifiable)

> _Note: this issue was identified against the code base at commit `e85b3d4e306a1fc3f303977481cd6410fa541f01`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSubscriber

## Description
Cause 1 of 1: the dead-letter record EventBridge v2 writes to a subscriber's OnFailureConfiguration queue is missing the `busArn` field.

Note on related open issues: 20261113T094431Z-eventbridgev2-iam-sqs-9-net-new-causes-putevents.md (Cause 8 of 9) and 20261113T094431Z-eventbridgev2-iam-sns-sqs-an-eventbridge-v2-subscriber.md both describe the case where NO dead-letter record is written at all, and the replay key they carry for this check is the old timeout signature (`_poll_dlq_for_failure_record#23|Timeout`). That is no longer what happens: a dead-letter record now DOES arrive on the OnFailureConfiguration queue and is matched by its `failedMessages[].eventId`, so the suite no longer treats this check as owned. The surviving divergence is the record's field set, which neither of those issues covers.

AWS: recorded behavior only for the dead-letter record body — the record is a JSON document written into SQS, so the Smithy model does not describe it. What the model does state is the surrounding contract: Smithy — OnFailureConfiguration.Arn, target DeadLetterQueueArn, @length 1-1600, @pattern `^arn:aws(-[a-z0-9]+)*:sqs:...` , doc "The ARN of the destination that receives events that could not be delivered. An Amazon SQS queue is the supported destination."; RetryPolicy.MaxRetryAttempts @range 0-185 (default 5). Recorded behavior: when a subscriber's one and only delivery attempt fails (here because the target queue was deleted before the event was published) and RetryPolicy.MaxRetryAttempts is exhausted, EventBridge writes exactly one JSON record to the OnFailureConfiguration.Arn queue that DESCRIBES the failure rather than replaying the event. That record carries, as top-level keys: `busArn` (the ARN of the event bus the event was published to, taken from the subscriber's EventBusArn), `subscriberArn` (the subscriber's ARN), `targetArn` (the ARN from InvokeConfiguration.TargetArn), `errorCode` (for a target queue that no longer exists, the fixed value `RESOURCE_NOT_FOUND`), `errorMessage` (a non-empty string describing the failure), `exhaustedRetryCondition` (the fixed value `MaximumRetryAttempts` when MaxRetryAttempts bounded the attempts), `retryAttempts` (the number of retries actually made — 0 when MaxRetryAttempts is 0), and `failedMessages`, a list whose entries carry `eventId` (the EventId the PutEvents result reported for that entry). The record must NOT carry the original event's envelope keys `source`, `detail-type`, `detail` or `account`. All of these field NAMES are the fixed part of the contract; the ARNs, the event id and the retry count are values taken from the request and from the subscriber's stored configuration.

Emulator: a record is written and does carry `failedMessages[0].eventId` equal to the EventId PutEvents returned (the receive loop matches on exactly that, and it matched), but the record has no `busArn` key at all — reading it raises `KeyError: 'busArn'`. Because that is the first field the check reads, the emulator's values for `subscriberArn`, `targetArn`, `errorCode`, `errorMessage`, `exhaustedRetryCondition` and `retryAttempts`, and the absence of the original-event keys, are all left unverified behind this one missing field; a fix should make the whole record match the field set above, not just add `busArn`.

Why: a client relying on AWS's behavior consumes the on-failure queue to find out which events were dropped and why. Without `busArn` it cannot attribute a dead-letter record to the bus the event was published to — the standard pattern of one shared dead-letter queue behind subscribers on several buses becomes unresolvable, and any consumer that indexes or routes records by bus ARN raises a key error on every record instead of handling the failure.

Fix: the eventbridgev2 subscriber delivery dispatcher's on-failure path — specifically the dead-letter record serializer that builds the JSON body sent to OnFailureConfiguration.Arn; it must populate the bus ARN from the subscriber's stored EventBusArn alongside the subscriber/target ARNs and the failure classification fields.

Traces: eventbridgev2__iam__sqs.subscriber-on-failure-configuration-writes-dead-letter-record-to-sqs

## Evidence
Failing check (verdict: error): eventbridgev2__iam__sqs.subscriber-on-failure-configuration-writes-dead-letter-record-to-sqs

  File "check_eventbridgev2__iam__sqs_subscriber_on_failure_configuration_writes_dead_letter_record_to_sqs.py", line 145, in run
    assert record["busArn"] == bus_arn, f"record busArn {record['busArn']!r} != {bus_arn!r}"
  KeyError: 'busArn'

The record was retrieved successfully from the OnFailureConfiguration queue and already matched on failedMessages[].eventId == the EventId PutEvents returned, so the emulator did write a dead-letter record; it simply has no `busArn` member.

Expected (AWS, recorded): one JSON record on the OnFailureConfiguration queue shaped
  {
    "busArn": <EventBusArn of the bus the event was published to>,
    "subscriberArn": <SubscriberArn>,
    "targetArn": <InvokeConfiguration.TargetArn>,
    "errorCode": "RESOURCE_NOT_FOUND",
    "errorMessage": <non-empty string describing the failure>,
    "exhaustedRetryCondition": "MaximumRetryAttempts",
    "retryAttempts": 0,
    "failedMessages": [{"eventId": <EventId from the PutEvents result>}]
  }
and carrying none of the original event's keys ("source", "detail-type", "detail", "account").
Fixed text: every field name above, plus the enum-like values "RESOURCE_NOT_FOUND" (target queue no longer exists) and "MaximumRetryAttempts" (retries bounded by RetryPolicy.MaxRetryAttempts). Variable: the three ARNs and the event id, taken from the request and the subscriber's stored configuration, and retryAttempts, taken from RetryPolicy.MaxRetryAttempts (0 here).

Actual (emulator): a record whose `failedMessages[0].eventId` is correct but which contains no `busArn` key; the remaining fields were never reached.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. CreateEventBus with a fresh Name and a ClientToken; poll DescribeEventBus until State == ACTIVE.
3. CreateQueue twice (a target queue and a dead-letter queue); read each QueueArn via GetQueueAttributes and SetQueueAttributes a Policy allowing sqs:SendMessage to Principal {"Service": "events.amazonaws.com"} on each.
4. CreateRole trusted by events.amazonaws.com and PutRolePolicy allowing sqs:SendMessage on both queue ARNs.
5. CreateSubscriber on the bus with InvokeConfiguration {RoleArn: <role>, TargetArn: <target queue arn>, SqsParameters: {}}, RetryPolicy {MaxRetryAttempts: 0} and OnFailureConfiguration {Arn: <dlq arn>}; assert the response State is RUNNING.
6. DeleteQueue the TARGET queue, so the subscriber's single delivery attempt fails deterministically.
7. PutEvents one entry on the bus (any Source/DetailType/Detail); keep the returned Entries[0].EventId.
8. ReceiveMessage from the dead-letter queue until a body arrives whose failedMessages[] names that EventId, then read the body's top-level keys.
9. Observe the record has no "busArn" key (KeyError). On AWS the record carries busArn = the bus's ARN, together with subscriberArn, targetArn, errorCode "RESOURCE_NOT_FOUND", a non-empty errorMessage, exhaustedRetryCondition "MaximumRetryAttempts", retryAttempts 0, and none of the original event's envelope keys.
