Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sesv2.sesv2-eventbridge-send-event-lambda-invocation-positive lambda__sesv2.sesv2-eventbridge-bounce-event-data-format
Issue-Key: lambda__sesv2.sesv2-eventbridge-send-event-lambda-invocation-positive|_drain_relay_events#12|Timeout|return collected
Issue-Key: lambda__sesv2.sesv2-eventbridge-bounce-event-data-format|probe#11|Timeout|return len(collected)
Issue-Commit: 6533a5364fe9b92c8a3d23732cb80de0d1d9d8ab

# lambda__sesv2: SEND and BOUNCE email events never reach a configuration set's EventBridge event destination, so the rule's Lambda target is never invoked

> _Note: this issue was identified against the code base at commit `6533a5364fe9b92c8a3d23732cb80de0d1d9d8ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfigurationSetEventDestination

## Description
Cause 1 of 1: a SESv2 configuration set with an enabled EventBridgeDestination publishes no event for the SEND or BOUNCE event types, so an EventBridge rule on {"source":["aws.ses"],"detail-type":["Email Sent"]} / ["Email Bounced"] never matches and the rule's Lambda target is never invoked. Both checks run out their budget waiting for a relayed event (Error: Check timed out after 60.0s), so no verdict on the event payload is ever reachable.

AWS: Smithy: sesv2 CreateConfigurationSetEventDestination takes EventDestinationDefinition{Enabled, MatchingEventTypes: EventTypes, EventBridgeDestination{EventBusArn (required, default bus only)}}; EventType enumerates SEND, REJECT, BOUNCE, COMPLAINT, DELIVERY, OPEN, CLICK, RENDERING_FAILURE, DELIVERY_DELAY, SUBSCRIPTION. The model documents that when the destination is Enabled "the specified event types are sent to the destinations in this EventDestinationDefinition"; the document's own shape is not modeled, so recorded behavior only for its contents. On AWS, sending through that configuration set publishes to the default bus an event with source "aws.ses" and detail-type "Email Sent" (for SEND) and, for a send to the mailbox simulator address bounce@simulator.amazonses.com, "Email Bounced" whose detail carries eventType "Bounce", mail.destination containing the recipient, and a bounce sub-object with bounceType "Permanent", bounceSubType "General", a timestamp, a feedbackId, and exactly one bouncedRecipients entry with emailAddress = the recipient, action "failed", status "5.1.1" and a diagnosticCode containing "550".

Emulator: after CreateConfigurationSetEventDestination with Enabled=true and MatchingEventTypes ["SEND"] / ["BOUNCE"] on the default bus, a rule matching aws.ses / "Email Sent" / "Email Bounced" with a Lambda target that relays to SQS, and a SendEmail through that configuration set (repeated for several rounds in the SEND check to allow for rule propagation), no event is ever relayed: both checks are killed at the 60 s budget with no event in the relay queue. Because nothing arrives, the detail-type, eventType and the whole bounce sub-object are unobservable rather than wrong.

Why: a client that wires bounce and send telemetry through EventBridge — suppression-list maintenance, per-recipient delivery accounting, alarming on bounce rate — receives nothing. Every consumer silently sees zero events, which is indistinguishable from "nothing was sent", so a sender keeps mailing an address AWS has already told it is permanently undeliverable. It also makes the documented SES event schema untestable: no payload exists to validate field by field.

Fix: the SESv2 send pipeline's event-destination fan-out — a send through a configuration set must, for each enabled EventDestination whose MatchingEventTypes include the event, publish the SES email-event document to the named destination, and for EventBridgeDestination that means a PutEvents onto the named (default) bus with source "aws.ses" and the documented detail-type per event type, including the mailbox-simulator-driven BOUNCE path that must synthesize the Permanent/General 550 5.1.1 bounce detail. Sibling open issues cover only two of the other event types and carry neither of these checks: 20261102T110414Z-lambda-sesv2-a-sesv2-delivery-event-never-reaches.md (DELIVERY) and 20261109T160343Z-lambda-sesv2-a-sesv2-complaint-event-never-reaches.md (COMPLAINT); 20261107T212447Z-lambda-sesv2-2-net-new-causes-the-lambda-runtime-s.md mentions SESv2 events not reaching EventBridge but likewise does not carry these two replay keys.

Traces: lambda__sesv2.sesv2-eventbridge-send-event-lambda-invocation-positive, lambda__sesv2.sesv2-eventbridge-bounce-event-data-format.

## Evidence
Check lambda__sesv2.sesv2-eventbridge-send-event-lambda-invocation-positive [mismatch]:
Error: Check timed out after 60.0s
Expected: after CreateConfigurationSetEventDestination(EventDestination={Enabled: true, MatchingEventTypes: ["SEND"], EventBridgeDestination: {EventBusArn: arn:aws:events:<region>:<account>:event-bus/default}}) and a rule {"source":["aws.ses"],"detail-type":["Email Sent"]} with a Lambda target, each SendEmail through the configuration set causes the function to be invoked with the aws.ses event (the check re-sends up to 4 times to allow for rule propagation).
Actual: no event is relayed in any round; the check is killed at the harness budget with nothing in the relay queue.

Check lambda__sesv2.sesv2-eventbridge-bounce-event-data-format [mismatch]:
Error: Check timed out after 60.0s
Expected: a send to bounce@simulator.amazonses.com through a configuration set whose EventBridge destination matches ["BOUNCE"] relays an event with detail-type "Email Bounced" whose detail has eventType "Bounce", mail.destination containing the recipient, and bounce = {bounceType: "Permanent", bounceSubType: "General", timestamp, feedbackId, bouncedRecipients: [{emailAddress: <recipient>, action: "failed", status: "5.1.1", diagnosticCode containing "550"}]}.
Actual: no "Email Bounced" event is relayed at all (the check's wait for a relayed event never completes), so none of the bounce fields can be compared.

## Steps to Reproduce
1. Resolve the account's verified sending domain and create an SQS relay queue, a role that may send to it, and a Lambda relay function that forwards its event to the queue.
2. sesv2 CreateConfigurationSet(<cs>) then CreateConfigurationSetEventDestination(ConfigurationSetName=<cs>, EventDestinationName=<d>, EventDestination={"Enabled": true, "MatchingEventTypes": ["SEND"] (and, for the second check, ["BOUNCE"]), "EventBridgeDestination": {"EventBusArn": "arn:aws:events:<region>:<account>:event-bus/default"}}).
3. events PutRule with EventPattern {"source":["aws.ses"],"detail-type":["Email Sent"]} (resp. ["Email Bounced"]), AddPermission so EventBridge may invoke the relay function, and PutTargets pointing the rule at it.
4. sesv2 SendEmail through <cs> from the verified domain to success@simulator.amazonses.com (resp. bounce@simulator.amazonses.com).
5. Poll the relay queue for the delivered event. On AWS an aws.ses event with the documented detail-type and detail arrives; against the emulator nothing ever arrives and the scenario runs out its budget.
