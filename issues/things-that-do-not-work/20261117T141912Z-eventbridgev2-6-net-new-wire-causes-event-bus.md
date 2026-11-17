Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-create-subscriber-filter-and-target-validation
Issue-Step: teardown#15
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-create-subscriber-filter-and-target-validation eventbridgev2-create-subscriber-starting-position-transformer-and-universal-target-validation eventbridgev2-event-bus-retention-description-and-key-validation eventbridgev2-resource-policy-public-access-and-reserved-names eventbridgev2-subscriber-tagging-and-name-prefix-pagination eventbridgev2-update-event-source-configuration-rejections
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|DeleteEventBus|2xx->4xx|status
Issue-Key: eventbridgev2|DescribeEventBus|4xx->2xx|$
Issue-Key: eventbridgev2|DescribeEventBus|4xx->2xx|status
Issue-Key: eventbridgev2|UpdateEventBus|4xx->2xx|$.Message
Issue-Key: eventbridgev2|UpdateEventBus|4xx->2xx|$.__type
Issue-Key: eventbridgev2|UpdateEventBus|4xx->2xx|status
Issue-Key: eventbridgev2|GetResourcePolicy|4xx->4xx|$.Message
Issue-Key: eventbridgev2|GetResourcePolicy|4xx->2xx|$.Message
Issue-Key: eventbridgev2|GetResourcePolicy|4xx->2xx|$.__type
Issue-Key: eventbridgev2|GetResourcePolicy|4xx->2xx|status
Issue-Key: eventbridgev2|ListTagsForResource|2xx->2xx|$.Tags.stage
Issue-Key: eventbridgev2|ListTagsForResource|2xx->2xx|$.Tags.team
Issue-Key: eventbridgev2|UpdateEventSource|4xx->2xx|$.Message
Issue-Key: eventbridgev2|UpdateEventSource|4xx->2xx|$.__type
Issue-Key: eventbridgev2|UpdateEventSource|4xx->2xx|status
Issue-Key: eventbridgev2|DescribeEventSource|2xx->2xx|$.Configuration.AwsServiceEventsConfiguration.Pattern
Issue-Commit: eab59dd3d13731329d6515ebc512ab3051ad49e1

# eventbridgev2: 6 net-new wire causes — event bus never deletable, UpdateEventBus skips KMS-key validation, resource-policy reads, subscriber tags and event-source pattern validation

> _Note: this issue was identified against the code base at commit `eab59dd3d13731329d6515ebc512ab3051ad49e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteEventBus

## Description
Six distinct, net-new root causes found by replaying wire corpus simparity-wire-aws-v1 against the emulator built from this checkout. The service is bootstrapped eventbridgev2 (sdkId EventBridgeV2, AWSEventsV2 v2025-05-15, protocol rpcv2Cbor, model services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json). The causes are independent and ALL SIX must be fixed and re-replayed before this issue is closed; every cause names its own fixture step, and the issue's replay pointer covers all of them via Issue-Trace-Ids.

Related OPEN issues that do NOT cover these fixture steps and do NOT carry their replay keys: 20261113T145420Z-eventbridgev2-4-net-new-behavioral-causes-createsubscriber-s.md (its Cause 3 is the ABSENCE of the DeleteEventBus subscriber gate at an older commit; Cause 1 below is the opposite failure — the gate now refuses forever), 20261110T210822Z-eventbridgev2-3-net-new-behavioral-causes-deleteeventbus-is.md (event-source half of the same gate, plus event-bus reads omitting StorageConfiguration), 20261113T174325Z-eventbridgev2-subscriber-reads-omit-stored-configuration-drop-updates.md (deleted subscribers stay readable — the likely upstream of Cause 1's phantom child), 20261115T124416Z-eventbridgev2-updateeventbus-mutations-never-reach-the-stored-bus.md (UpdateEventBus mutations not persisted — a different defect from Cause 2's missing key validation), 20261117T042927Z-eventbridgev2-deleteresourcepolicy-stub.md (DeleteResourcePolicy is an unimplemented stub — see the note in Cause 4), 20261113T145420Z-eventbridgev2-tagresource-stub.md (TagResource stub — see the note in Cause 5) and 20261117T042927Z-eventbridgev2-putresourcepolicy-conflictexception-on-a-stale-expectedrevisionid-carries.md (PutResourcePolicy revision-mismatch wording, not the GetResourcePolicy read path of Causes 3-4).

=== Cause 1 of 6: an event bus whose children have been removed can never be deleted — DeleteEventBus answers 409 and DescribeEventBus keeps answering 200 ACTIVE ===
AWS: Smithy: DeleteEventBus is @idempotent, takes only EventBusArn (@required), and declares ResourceInUseException (@error client, @httpError 409) documented "The resource is in use and cannot be deleted. For example, an event bus with subscribers or event sources cannot be deleted until they are deleted."; the operation doc adds "Deletion is asynchronous: the bus moves to DELETING and disappears when complete (see the EventBusDeleted waiter). A bus with subscribers or event sources cannot be deleted." DescribeEventBus declares ResourceNotFoundException (@error client, @httpError 404, "The resource does not exist."). So the in-use gate is scoped to children that STILL EXIST: once the bus's subscribers and event sources have been deleted, DeleteEventBus succeeds with HTTP 200 and an empty body, and a DescribeEventBus poll on that ARN ends in 404 ResourceNotFoundException.
Emulator: the teardown delete of a bus whose subscribers and event sources were just deleted is refused — "status: expected 200, got 409" — and the follow-up DescribeEventBus poll then answers HTTP 200 with the complete bus body (EventBusArn/Name/StorageConfiguration/CreationTime/LastModifiedTime and State ACTIVE) for all 37 attempts inside the 180 s cap, never reaching DELETING and never answering ResourceNotFoundException. The bus is therefore undeletable for the rest of the account's life. Hypothesis: the in-use gate counts children the emulator has not forgotten — deleted subscribers survive deletion in state (see the related open issue above), so a phantom child holds the gate shut permanently.
Why: a client that follows AWS's documented ordering (delete the subscribers and event sources, then the bus) gets a 409 it can never clear, and the EventBusDeleted waiter never completes. Teardown code loops until its budget expires, test accounts accumulate buses that cannot be removed, and the 404-after-delete branch of every read path is unreachable.
Fix: the DeleteEventBus handler's in-use gate and the bus/child relation it consults in service state — the gate must be evaluated against live, non-revoked subscribers and event sources only, and DeleteSubscriber/DeleteEventSource must remove the child from the bus's child set so the gate opens; the accepted delete must then move the bus to DELETING and remove it, so DescribeEventBus answers 404 ResourceNotFoundException.
Traces: eventbridgev2-create-subscriber-filter-and-target-validation#teardown#15 (the 409) and #teardown#16 (the 200 ACTIVE poll); the same pair repeats as eventbridgev2-create-subscriber-starting-position-transformer-and-universal-target-validation#teardown#17 and #teardown#18.

=== Cause 2 of 6: UpdateEventBus accepts an EncryptionConfiguration naming a KMS key that does not exist, instead of refusing with 403 AccessDeniedException ===
AWS: Smithy: UpdateEventBus takes EncryptionConfiguration (structure EncryptionConfiguration, member KmsKeyIdentifier, @length 0-2048, @pattern ^[a-zA-Z0-9_\\-/:]*$) and declares AccessDeniedException (@error client, @httpError 403) whose doc states "The caller does not have the permissions required to perform the operation. This error is also returned when the operation cannot use the AWS KMS key for the event bus." Recorded behavior: pointing an event bus at a KMS key that does not exist is refused with HTTP 403, __type com.amazon.nexus.v20250515.exceptions#AccessDeniedException and Message "KMS_KEY_NOT_FOUND: The event bus's KMS key does not exist." — that message is entirely FIXED text for this error class; it interpolates nothing from the request or from state (no key id, no bus name).
Emulator: the update is accepted: "status: expected 403, got 200", with no __type and no Message in the body at all. The handler performs no reachability check on EncryptionConfiguration.KmsKeyIdentifier.
Why: a client that handles AccessDeniedException/KMS_KEY_NOT_FOUND (prompting for a valid key, falling back to AWS-owned encryption) never sees the refusal, and believes a bus is encrypted under a key that cannot be resolved; against AWS the same call fails, so the behaviour it was written against is never exercised.
Fix: UpdateEventBus's input validation (and the same check in CreateEventBus's encryption path, which shares it) must resolve EncryptionConfiguration.KmsKeyIdentifier against KMS state and raise the modeled AccessDeniedException with AWS's fixed message when the key does not exist, leaving the stored bus unchanged.
Traces: eventbridgev2-event-bus-retention-description-and-key-validation#task#8.

=== Cause 3 of 6: GetResourcePolicy's ResourceNotFoundException message is emulator-authored and interpolates the bus name ===
AWS: Smithy: GetResourcePolicy (@readonly) takes ResourceArn (@required, an event-bus ARN) and PolicyName (@length 1-128, @pattern ^[A-Za-z0-9_]+$) and declares ResourceNotFoundException (@error client, @httpError 404); its doc states "Returns the named resource policy attached to an event bus. Fails with ResourceNotFoundException when the event bus or the named policy does not exist." Recorded behavior: reading a policy name that is not attached answers HTTP 404 with Message "No 'default' resource policy is attached to this event bus". The template is AWS's fixed text "No '<PolicyName>' resource policy is attached to this event bus", where the only variable part is the requested PolicyName (quoted in single quotes; 'default' when the request omits it). AWS does NOT name the event bus in this message.
Emulator: the status and error class are right, but the message is the handler's own sentence of the form "Resource policy not found: <PolicyName> on event bus <bus name>" — the replay shows 'Resource policy not found: default on event bus sim2real-probe-ebv2-bus-cd340de7'. The bus name in that value is this run's generated name filling the emulator's template; the defect is the wording and the extra interpolation, not that particular name.
Why: the error message is part of the contract. A client or test that asserts on, logs or parses AWS's sentence reads different text from the emulator, and code that learns to pull a bus name out of this message against the emulator has nothing to pull against AWS.
Fix: the GetResourcePolicy handler's not-found path and the ResourceNotFoundException construction it uses — emit AWS's fixed sentence with the requested PolicyName as its only variable part.
Traces: eventbridgev2-resource-policy-public-access-and-reserved-names#task#4; the same divergence repeats at #task#10.

=== Cause 4 of 6: GetResourcePolicy answers 200 with a policy that is no longer attached, instead of 404 ResourceNotFoundException ===
AWS: Smithy/recorded behavior as in Cause 3: once the named policy is no longer attached to the bus, GetResourcePolicy must fail with HTTP 404, __type com.amazon.nexus.v20250515.exceptions#ResourceNotFoundException and Message "No '<PolicyName>' resource policy is attached to this event bus" (fixed text, PolicyName from the request).
Emulator: the read still succeeds — "status: expected 404, got 200", with __type and Message absent — so the detached policy stays readable and the resource-policy lifecycle is never observable as a removal.
Why: a client that checks whether a bus still carries a policy (an idempotent "remove then verify" step, or an authorization audit) is told the policy is still attached; a policy AWS considers gone remains returned with its document and revision id, so remediation never terminates.
Fix: the GetResourcePolicy handler must read through to the bus's live policy set, and the policy-removal path in state must actually detach the named policy. Note for the fixer: DeleteResourcePolicy is an unimplemented stub in this build (open issue 20261117T042927Z-eventbridgev2-deleteresourcepolicy-stub.md), so implementing that write is a prerequisite for this read to be reachable — but the read's own 404/ResourceNotFoundException branch for a detached policy must exist and be exercised, which is what this fixture step replays.
Traces: eventbridgev2-resource-policy-public-access-and-reserved-names#task#15.

=== Cause 5 of 6: a subscriber's tags are never readable — ListTagsForResource answers 200 with the tag keys absent ===
AWS: Smithy: ListTagsForResource (@readonly) takes ResourceArn (@required, TaggableResourceArn — an event bus, subscriber or event source) and returns Tags (TagMap, @length max 200); its doc is "Lists the tags on an event bus, subscriber, or event source." CreateSubscriber accepts Tags (TagMap) at create time, and TagResource "Adds or replaces tags on an event bus, subscriber, or event source" while UntagResource removes named keys. Recorded behavior: after the fixture's tag writes the read returns every live key/value pair — first {team: orders, stage: test}, and after the key 'stage' is removed, exactly {team: orders}; a key that was written and not removed is always present with its written value.
Emulator: the read answers HTTP 200 but the tag entries are missing: "$.Tags.stage: missing (expected 'test', got None)" and "$.Tags.team: missing (expected 'orders', got None)" on the first read, and "$.Tags.team: missing (expected 'orders', got None)" on the read taken after the untag — i.e. the subscriber's tag map is empty on both reads, including the key that was never removed. The tag writes the fixture performs (CreateSubscriber's Tags member and/or TagResource) never reach the subscriber's stored tag map, or ListTagsForResource does not read it.
Why: tags are the handle for ownership, cost allocation and policy conditions. A client that tags a subscriber at create time and then reads the tags back sees none, so tag-driven selection, cleanup-by-tag and tag-condition authorization are all unobservable against the emulator while they work against AWS — and the removal of one key is indistinguishable from the loss of all of them.
Fix: the subscriber's tag state — CreateSubscriber must persist its Tags member onto the created subscriber, UntagResource must remove only the named keys, and the ListTagsForResource handler must serialize the stored TagMap. Note for the fixer: TagResource is an unimplemented stub in this build (open issue 20261113T145420Z-eventbridgev2-tagresource-stub.md); if the fixture's keys are written through TagResource, that stub must be implemented too, but the key that survives the untag is still absent, so the create-time/read-back path is independently broken.
Traces: eventbridgev2-subscriber-tagging-and-name-prefix-pagination#task#9 (both keys missing) and #task#12 (the surviving key missing).

=== Cause 6 of 6: UpdateEventSource never validates Configuration.AwsServiceEventsConfiguration.Pattern — reserved top-level keys and non-JSON patterns are accepted and persisted ===
AWS: Smithy: UpdateEventSource takes Configuration (union EventSourceConfiguration, "Discriminated EventSource configuration. Exactly one variant must be set.") whose AwsServiceEventsConfiguration variant is AwsServiceEventsSourceConfiguration{AwsService (@required, @pattern ^aws\\.[a-z0-9\\-]+$), Pattern (EventPattern, @length 0-3753, @sensitive), OnFailureConfiguration}. Pattern's doc is explicit: "A filter pattern, as a JSON string, that defines which of the service's events are forwarded to the event bus. Do not include source, account, or region as top-level fields. If no pattern is specified, all events from the service are forwarded." InvalidInputException (@error client, @httpError 400) is a service-common error on this operation. Recorded behavior: an update whose Pattern sets one of those reserved top-level fields is refused with HTTP 400, __type com.amazon.nexus.v20250515.exceptions#InvalidInputException and Message "Pattern must not set the key '<key>'; the forwarding rule sets it from the requested AwsService and the source account and region" — fixed text whose only variable part is the offending top-level key name ('source', 'account' or 'region') as it appears in the submitted pattern. A Pattern that is not valid JSON is refused the same way (400 InvalidInputException), and because a refused update changes nothing, DescribeEventSource must still report the last accepted pattern.
Emulator: the refusal is missing entirely — "status: expected 400, got 200", with __type and Message absent — for every reserved-key/invalid-pattern update in the fixture, and the bad value is then stored: the subsequent DescribeEventSource returns Configuration.AwsServiceEventsConfiguration.Pattern '{not json' where the last valid pattern '{"detail-type": ["Object Created"]}' must still be reported. So the handler neither parses the pattern as JSON nor rejects the reserved top-level keys, and it overwrites good stored configuration with the rejected input.
Why: a client gets a 200 for a forwarding rule AWS would have refused, and the event source is left holding a pattern that cannot match anything (or one whose 'source' key contradicts the AwsService-derived rule), so forwarding silently stops while the API reports success; the stored configuration a subsequent read returns is not even well-formed JSON, which breaks any consumer that parses it.
Fix: UpdateEventSource's input validation (shared with CreateEventSource's configuration path) must parse Pattern as JSON and refuse a pattern that sets the top-level keys source, account or region with the modeled InvalidInputException and AWS's message, and a refused update must leave the stored event source — and therefore DescribeEventSource's Pattern — untouched.
Traces: eventbridgev2-update-event-source-configuration-rejections#task#4 (the accepted reserved-key update; the same divergence repeats at #task#5, #task#6 and #task#7) and #task#12 (the stored '{not json' pattern read back).

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200): 17 fixtures replayed, 200 steps, 113 passed, 78 mismatched, 9 blocked, score 56.5%, 30 divergence clusters. 22 of those clusters are already filed (owned by open eventbridgev2/sqs issues or blocked by stub operations) and are not repeated here. The six causes below are the net-new, unowned ones; all are on service eventbridgev2, which get_bootstrap_status reports as bootstrapped (pending is empty).

Cause 1 — [mismatch] DeleteEventBus, fixture eventbridgev2-create-subscriber-filter-and-target-validation step teardown#15 (cluster of 2, also ...-starting-position-transformer-and-universal-target-validation#teardown#17):
  expected HTTP 200, empty body
  actual   HTTP 409 — "status: expected 200, got 409"
  and [mismatch] DescribeEventBus, same fixture step teardown#16 (cluster of 2, also ...#teardown#18):
  expected HTTP 404 (poll until error code ResourceNotFoundException)
  actual   HTTP 200 — "poll capped at 180s" / "poll: error code None not in ['ResourceNotFoundException'] after 37 attempt(s)", body {'EventBusArn': 'arn:aws:events:us-west-2:000000000000:event-busv2/<bus name>/<25-char id>', 'Name': '<bus name>', 'Description': '', 'StorageConfiguration': {...}, 'CreationTime': ..., 'LastModifiedTime': ..., 'State': 'ACTIVE'} (the ARN, name, ids and timestamps are this run's values, not the requirement).

Cause 2 — [mismatch] UpdateEventBus, fixture eventbridgev2-event-bus-retention-description-and-key-validation step task#8:
  expected HTTP 403, $.__type 'com.amazon.nexus.v20250515.exceptions#AccessDeniedException', $.Message "KMS_KEY_NOT_FOUND: The event bus's KMS key does not exist."
  actual   HTTP 200 — "$.__type: missing", "$.Message: missing"

Cause 3 — [mismatch] GetResourcePolicy, fixture eventbridgev2-resource-policy-public-access-and-reserved-names step task#4 (cluster of 2, also #task#10):
  expected HTTP 404, $.Message "No 'default' resource policy is attached to this event bus"
  actual   HTTP 404, $.Message 'Resource policy not found: default on event bus sim2real-probe-ebv2-bus-cd340de7' (status and error class match; only the wording and the interpolated bus name diverge)

Cause 4 — [mismatch] GetResourcePolicy, same fixture step task#15:
  expected HTTP 404, $.__type 'com.amazon.nexus.v20250515.exceptions#ResourceNotFoundException', $.Message "No 'default' resource policy is attached to this event bus"
  actual   HTTP 200 — "$.__type: missing", "$.Message: missing"

Cause 5 — [mismatch] ListTagsForResource, fixture eventbridgev2-subscriber-tagging-and-name-prefix-pagination step task#9:
  expected HTTP 200 with $.Tags.team 'orders' and $.Tags.stage 'test'
  actual   HTTP 200 — "$.Tags.stage: missing (expected 'test', got None)", "$.Tags.team: missing (expected 'orders', got None)"
  and step task#12 (after the 'stage' key is removed): expected $.Tags.team 'orders'; actual "$.Tags.team: missing (expected 'orders', got None)".

Cause 6 — [mismatch] UpdateEventSource, fixture eventbridgev2-update-event-source-configuration-rejections step task#4 (cluster of 4: task#4, task#5, task#6, task#7):
  expected HTTP 400, $.__type 'com.amazon.nexus.v20250515.exceptions#InvalidInputException', $.Message "Pattern must not set the key 'source'; the forwarding rule sets it from the requested AwsService and the source account and region"
  actual   HTTP 200 — "$.__type: missing", "$.Message: missing"
  and [mismatch] DescribeEventSource, same fixture step task#12:
  expected $.Configuration.AwsServiceEventsConfiguration.Pattern '{"detail-type": ["Object Created"]}' (the last accepted pattern)
  actual   '{not json' — the refused input was persisted.

Model references (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json): DeleteEventBus errors ResourceInUseException/409 ("an event bus with subscribers or event sources cannot be deleted until they are deleted"), doc "A bus with subscribers or event sources cannot be deleted"; DescribeEventBus errors ResourceNotFoundException/404 ("The resource does not exist"); AccessDeniedException/403 doc "...also returned when the operation cannot use the AWS KMS key for the event bus"; EncryptionConfiguration.KmsKeyIdentifier @length 0-2048 @pattern ^[a-zA-Z0-9_\\-/:]*$; GetResourcePolicy doc "Fails with ResourceNotFoundException when the event bus or the named policy does not exist", PolicyName @pattern ^[A-Za-z0-9_]+$; ListTagsForResource returns Tags (TagMap, max 200) for an event bus, subscriber or event source, and CreateSubscriber accepts Tags; AwsServiceEventsSourceConfiguration.Pattern doc "Do not include source, account, or region as top-level fields"; InvalidInputException/400 is a service-common error.

## Steps to Reproduce
0. git_clone the repo, then replay_wire_corpus (simparity-wire-aws-v1), which builds and boots the emulator from this checkout. Each cause below can also be driven by hand against the running emulator with an rpc-v2-cbor EventBridgeV2 client.

Cause 1 (fixture eventbridgev2-create-subscriber-filter-and-target-validation, steps teardown#15 and teardown#16):
  a. CreateEventBus and poll DescribeEventBus until State == ACTIVE; create a subscriber (and/or an event source) on that bus.
  b. Delete every child: DeleteSubscriber(SubscriberArn=...) / DeleteEventSource(EventSourceArn=...).
  c. DeleteEventBus(EventBusArn=<bus arn>): AWS answers HTTP 200 with an empty body; the emulator answers HTTP 409.
  d. Poll DescribeEventBus(EventBusArn=<bus arn>): AWS ends in HTTP 404 ResourceNotFoundException; the emulator answers 200 with State ACTIVE until the 180 s poll cap (37 attempts).

Cause 2 (fixture eventbridgev2-event-bus-retention-description-and-key-validation, step task#8):
  a. Create an ACTIVE event bus.
  b. UpdateEventBus(EventBusArn=<bus arn>, EncryptionConfiguration={KmsKeyIdentifier: <a KMS key id/ARN that does not exist>}).
  c. AWS refuses with HTTP 403, __type ...#AccessDeniedException and Message "KMS_KEY_NOT_FOUND: The event bus's KMS key does not exist."; the emulator answers HTTP 200 and stores the key.

Cause 3 (fixture eventbridgev2-resource-policy-public-access-and-reserved-names, steps task#4 and task#10):
  a. Create an ACTIVE event bus and attach NO resource policy.
  b. GetResourcePolicy(ResourceArn=<bus arn>) (PolicyName omitted, i.e. 'default').
  c. Both answer HTTP 404 ResourceNotFoundException, but AWS's Message is "No 'default' resource policy is attached to this event bus" while the emulator's is "Resource policy not found: default on event bus <bus name>".

Cause 4 (same fixture, step task#15):
  a. Create an ACTIVE event bus, PutResourcePolicy(ResourceArn=<bus arn>, PolicyName='default', Policy=<a valid, non-public policy>).
  b. Remove it (DeleteResourcePolicy(ResourceArn=<bus arn>, PolicyName='default') — currently an unimplemented stub).
  c. GetResourcePolicy(ResourceArn=<bus arn>, PolicyName='default'): AWS answers HTTP 404 ...#ResourceNotFoundException with "No 'default' resource policy is attached to this event bus"; the emulator answers HTTP 200 with the policy still attached.

Cause 5 (fixture eventbridgev2-subscriber-tagging-and-name-prefix-pagination, steps task#9 and task#12):
  a. Create an ACTIVE event bus and CreateSubscriber(..., Tags={team: 'orders', stage: 'test'}) (and/or TagResource those keys onto the subscriber ARN).
  b. ListTagsForResource(ResourceArn=<subscriber arn>): AWS returns both key/value pairs; the emulator returns 200 with no Tags entries.
  c. UntagResource(ResourceArn=<subscriber arn>, TagKeys=['stage']), then ListTagsForResource again: AWS returns exactly {team: 'orders'}; the emulator still returns no Tags entries.

Cause 6 (fixture eventbridgev2-update-event-source-configuration-rejections, steps task#4-task#7 and task#12):
  a. Create an ACTIVE event bus and CreateEventSource with Configuration={AwsServiceEventsConfiguration: {AwsService: 'aws.s3', Pattern: '{"detail-type": ["Object Created"]}'}}.
  b. UpdateEventSource(EventSourceArn=<arn>, Configuration={AwsServiceEventsConfiguration: {AwsService: 'aws.s3', Pattern: '{"source": ["aws.s3"]}'}}) — and likewise with a top-level 'account' key, a top-level 'region' key, and a Pattern that is not JSON ('{not json'). AWS refuses each with HTTP 400 ...#InvalidInputException and "Pattern must not set the key '<key>'; the forwarding rule sets it from the requested AwsService and the source account and region"; the emulator answers HTTP 200.
  c. DescribeEventSource(EventSourceArn=<arn>): AWS still reports the last accepted Pattern '{"detail-type": ["Object Created"]}'; the emulator reports the rejected '{not json'.
