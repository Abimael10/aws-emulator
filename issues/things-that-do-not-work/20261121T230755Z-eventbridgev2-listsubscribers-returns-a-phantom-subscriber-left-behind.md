Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-create-subscriber-filter-and-target-validation
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-create-subscriber-filter-and-target-validation eventbridgev2-create-subscriber-starting-position-transformer-and-universal-target-validation
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|ListSubscribers|2xx->2xx|$.Subscribers[*]
Issue-Commit: c988d8ae2952379498078b24cd167acc3f62c9d8

# eventbridgev2 ListSubscribers returns a phantom subscriber left behind by a refused CreateSubscriber

> _Note: this issue was identified against the code base at commit `c988d8ae2952379498078b24cd167acc3f62c9d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListSubscribers

## Description
One net-new wire cause in the eventbridgev2 subscriber state: a ListSubscribers read of an event bus whose every CreateSubscriber attempt was refused still enumerates a subscriber, because the emulator persists subscriber state on a create it should have rejected.

=== Cause 1 of 1: ListSubscribers enumerates a subscriber that no accepted create ever produced ===
AWS: Smithy - ListSubscribers (readonly, paginated: inputToken NextToken / outputToken NextToken / pageSize MaxResults / items Subscribers) takes EventBusArn plus optional NamePrefix/MaxResults/NextToken and answers 200 with Subscribers: SubscriberSummaryList; its modelled errors are ResourceNotFoundException (404), AccessDeniedException (403), InvalidInputException (400), ThrottlingException (429), InternalException (500). The list reflects only subscribers a CreateSubscriber call actually accepted: CreateSubscriber models InvalidInputException (400) and AccessDeniedException (403), and a request refused with either of those creates nothing, so it contributes no element to the list. In the recording every CreateSubscriber against this bus is refused (400 InvalidInputException for a target/parameter-block mismatch, for a cross-account target and for an empty filter pattern; 403 AccessDeniedException for the permission probe), so the recorded ListSubscribers answers 200 with an empty Subscribers list - there is no $.Subscribers[0] at all.
Emulator: 200 with one element present at $.Subscribers[0] - a full SubscriberSummary (SubscriberArn, Name, EventBusArn, TargetArn, Type UNORDERED, State RUNNING, CreationTime, LastModifiedTime, SubscriberAccountId) for the subscriber name the refused creates used. The refused create path mutates subscriber state before (or regardless of) validation, and the list read then serves that residue.
Why: a client enumerating the subscribers of a bus is shown a subscriber that does not exist in AWS and that can never deliver, yet reports State RUNNING. Any logic driven by the listing diverges: a reconciler believes the subscriber is already present and skips creating the real one, a name-uniqueness pre-check believes the name is taken, and teardown code tries to delete an ARN AWS never minted. Pagination counts (MaxResults/NextToken) are inflated by the same phantom.
Fix: the eventbridgev2 CreateSubscriber handler's state write must be gated behind input validation and authorization so a refused create leaves no subscriber record, and the eventbridgev2 subscriber state that ListSubscribers reads must contain only committed subscribers. This is the read-side consequence of the refusal-path ordering already filed in 20261113T174325Z-eventbridgev2-subscriber-error-paths-answer-the-wrong-status.md (which does NOT carry this step's replay key), and it is not the stub reported in 20261114T215754Z-eventbridgev2-listsubscribers-stub.md: ListSubscribers answers 200 with a serialized body here.
Traces: eventbridgev2-create-subscriber-filter-and-target-validation#task#14, and the same cluster's eventbridgev2-create-subscriber-starting-position-transformer-and-universal-target-validation#task#16.

## Evidence
Cause 1 - ListSubscribers, expected status 200, actual status 200; body diverges. Expected: Subscribers empty (no $.Subscribers[0]). Actual: $.Subscribers[0] extra - {'SubscriberArn': 'arn:aws:events:<region>:<account>:subscriber/<name>/<25-char suffix>', 'Name': '<the subscriber name the refused creates used>', 'EventBusArn': 'arn:aws:events:<region>:<account>:event-busv2/<bus name>/<25-char suffix>', 'TargetArn': 'arn:aws:sqs:<region>:<account>:<queue>', 'Type': 'UNORDERED', 'State': 'RUNNING', 'CreationTime': <epoch>, 'LastModifiedTime': <epoch>, 'SubscriberAccountId': '<account>'}. The ARNs, names, ids and timestamps are per-run values; the requirement is that the element is absent entirely. Cluster covers 2 replayed steps across 2 fixtures (affected_count 2).

## Steps to Reproduce
1. Build and boot the emulator from this checkout, then replay the wire corpus (trace set simparity-wire-aws-v1).
2. Fixture eventbridgev2-create-subscriber-filter-and-target-validation: CreateEventBus, then issue the fixture's CreateSubscriber probes (steps task#3..task#13) - every one of them is a request AWS refuses with 400 InvalidInputException or 403 AccessDeniedException.
3. Step task#14: call ListSubscribers with the bus's EventBusArn. AWS answers 200 with an empty Subscribers list; the emulator answers 200 with one SubscriberSummary for the refused subscriber name.
4. The same sequence reproduces at eventbridgev2-create-subscriber-starting-position-transformer-and-universal-target-validation#task#16.
