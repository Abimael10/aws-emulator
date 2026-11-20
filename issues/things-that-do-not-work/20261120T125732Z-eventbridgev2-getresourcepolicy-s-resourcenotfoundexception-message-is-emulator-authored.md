Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2.resource-policy-put-get-list-round-trip
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2.resource-policy-put-get-list-round-trip|run#21|AssertionError|assert message == "No 'default' resource policy is attached to this event bus", (
Issue-Commit: 233e85f7211280754246a7d02478fa1f321c9471

# eventbridgev2 GetResourcePolicy's ResourceNotFoundException message is emulator-authored and interpolates the event bus name instead of AWS's fixed "No '<PolicyName>' resource policy is attached to this event bus"

> _Note: this issue was identified against the code base at commit `233e85f7211280754246a7d02478fa1f321c9471`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourcePolicy

## Description
One net-new root cause found by replaying behavioral suite simparity-behavioral-aws-v1 against the emulator built from this checkout. The service is eventbridgev2 (sdkId EventBridgeV2, AWSEventsV2, protocol rpcv2Cbor, model services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json), which get_bootstrap_status reports as bootstrapped (pending is empty).

Related OPEN issue that does NOT carry this check's replay key and therefore does not cover it: 20261117T141912Z-eventbridgev2-6-net-new-wire-causes-event-bus.md. Its Cause 3 describes the SAME GetResourcePolicy not-found wording defect, but it was raised from the WIRE corpus (replay keys of the form eventbridgev2|GetResourcePolicy|4xx->4xx|$.Message, fixture eventbridgev2-resource-policy-public-access-and-reserved-names#task#4) and it bundles five other causes; this behavioral check id is not in its replay pointer, so re-replaying that issue's traces would not exercise this check. A fixer closing that issue's Cause 3 should expect this check to go green too, and should re-replay the check id listed below to confirm.

=== Cause 1 of 1: GetResourcePolicy's not-found message is the handler's own sentence and leaks the event bus name ===

AWS: Smithy GetResourcePolicy (@readonly) takes ResourceArn (@required, target EventBusArn, @length 1-1011, pattern ^arn:aws...:event-busv2\/<name>\/<25-char id>$) and PolicyName (@length 1-128, @pattern ^[A-Za-z0-9_]+$), and declares ResourceNotFoundException (@error client, @httpError 404, doc "The resource does not exist."). The operation doc is explicit: "Returns the named resource policy attached to an event bus. Fails with ResourceNotFoundException when the event bus or the named policy does not exist." Recorded behavior: reading a policy name that is not attached to an existing bus answers HTTP 404 with error code ResourceNotFoundException and Message "No 'default' resource policy is attached to this event bus". The message template is AWS's FIXED text "No '<PolicyName>' resource policy is attached to this event bus"; its ONLY variable part is the requested PolicyName, single-quoted, defaulting to 'default' when the request omits the member. AWS does NOT name the event bus, its ARN, or the account in this message.

Emulator: the HTTP status (404) and the error code (ResourceNotFoundException) are correct, but the Message is a handler-authored sentence of the shape "Resource policy not found: <PolicyName> on event bus <event bus name>" — the replay recorded 'Resource policy not found: default on event bus sim2real-behav-ebv2-bus-71e22970683f'. Note that the bus name in that value is this run's generated name filling the emulator's own template: the defect is the wording and the extra bus-name interpolation, not that particular name. Everything else in the check passed — PutResourcePolicy returned PolicyName 'default' with a RevisionId, GetResourcePolicy round-tripped the document byte-for-byte at the same revision, ListResourcePolicies returned exactly one summary at that revision, and DeleteResourcePolicy removed it — so only the refusal wording on the post-delete read diverges.

Why: an error message is part of the contract. A client, test or runbook that asserts on, logs, greps or parses AWS's sentence reads different text from the emulator and misclassifies the refusal; conversely, code written against the emulator learns to pull an event bus name out of this message and has nothing to pull against AWS, where no bus name is present. Because PolicyName is the only thing AWS varies, a client cannot even recover which policy was missing from the emulator's wording in a stable way.

Fix: the GetResourcePolicy handler's not-found path in the eventbridgev2 service crate, and the ResourceNotFoundException construction it uses — emit AWS's fixed sentence with the requested PolicyName (defaulting to 'default') as its only interpolated part, and drop the event bus name from the message. The same wording must be used whether the bus has no policies at all or the named policy is simply not attached.

Traces: behavioral check eventbridgev2.resource-policy-put-get-list-round-trip (verdict mismatch) asserts the message after DeleteResourcePolicy; the wire corpus hits the same handler at eventbridgev2-resource-policy-public-access-and-reserved-names#task#4 and #task#10 (already described as Cause 3 of the related open issue above).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Overall 18/42 passed (42.9%); service eventbridgev2 9/17 (53%), 8 failed, of which 7 are already filed (OWNED by an open issue or blocked by an unimplemented stub) and are not repeated here. The one net-new, unowned failure:

### eventbridgev2
#### [mismatch] eventbridgev2.resource-policy-put-get-list-round-trip
  File "check_eventbridgev2_resource_policy_put_get_list_round_trip.py", line 99, in run
    assert message == "No 'default' resource policy is attached to this event bus", (
  AssertionError: unexpected message 'Resource policy not found: default on event bus sim2real-behav-ebv2-bus-71e22970683f'

Expected vs actual on GetResourcePolicy after the policy was deleted:
  expected  HTTP 404, Error.Code 'ResourceNotFoundException', Error.Message "No 'default' resource policy is attached to this event bus"
  actual    HTTP 404, Error.Code 'ResourceNotFoundException', Error.Message 'Resource policy not found: default on event bus sim2real-behav-ebv2-bus-71e22970683f'

Fixed vs variable parts of the required message: "No '" + <PolicyName from the request, 'default' when omitted> + "' resource policy is attached to this event bus" is AWS's text; the bus name 'sim2real-behav-ebv2-bus-71e22970683f' in the emulator's value is a per-run generated name produced by ctx.unique_name("sim2real-behav-ebv2-bus-") and is not part of any requirement.

The check reached line 99, so every earlier assertion held: PutResourcePolicy returned PolicyName 'default'; GetResourcePolicy returned the same RevisionId and a byte-identical PolicyDocument; ListResourcePolicies (paginated) returned exactly [{PolicyName: 'default', RevisionId: <put revision>}]; DeleteResourcePolicy succeeded.

Model references (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json): GetResourcePolicy @readonly, input ResourceArn (@required, EventBusArn) + PolicyName (@length 1-128, @pattern ^[A-Za-z0-9_]+$), output ResourceArn/PolicyDocument/PolicyName/RevisionId all @required, errors ResourceNotFoundException (client, 404), AccessDeniedException (403), InvalidInputException (400), ThrottlingException (429), InternalException (500); operation doc "Returns the named resource policy attached to an event bus. Fails with ResourceNotFoundException when the event bus or the named policy does not exist."; ResourceNotFoundException doc "The resource does not exist." with a single Message member.

## Steps to Reproduce
0. git_clone the repo, then run_behavioral_suite (simparity-behavioral-aws-v1), which builds and boots the emulator from this checkout and replays eventbridgev2.resource-policy-put-get-list-round-trip. The cause can also be driven by hand against the running emulator with an rpc-v2-cbor EventBridgeV2 client:

1. CreateEventBus(Name=<unique name>, ClientToken=<unique token>); poll DescribeEventBus(EventBusArn=<bus arn>) until State == ACTIVE.
2. PutResourcePolicy(ResourceArn=<bus arn>, PolicyDocument=<a valid policy allowing events:PutEvents to arn:aws:iam::<account>:root on that bus ARN>). It returns PolicyName 'default' and a RevisionId.
3. DeleteResourcePolicy(ResourceArn=<bus arn>).
4. GetResourcePolicy(ResourceArn=<bus arn>) with PolicyName omitted (i.e. 'default').
   AWS: HTTP 404, Error.Code ResourceNotFoundException, Error.Message "No 'default' resource policy is attached to this event bus".
   Emulator: HTTP 404, Error.Code ResourceNotFoundException, but Error.Message "Resource policy not found: default on event bus <bus name>".
5. The same wording divergence is observable without step 2/3 at all: GetResourcePolicy on a freshly created bus that never carried a policy must produce the identical fixed sentence.
