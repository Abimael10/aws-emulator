Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sesv2.configuration-set-event-destination-custom-event-bus-rejected
Issue-Key: events__sesv2.configuration-set-event-destination-custom-event-bus-rejected|_error#18|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# events__sesv2: CreateConfigurationSetEventDestination and UpdateConfigurationSetEventDestination accept an EventBridgeDestination naming a non-default event bus instead of refusing it with BadRequestException

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfigurationSetEventDestination

## Description
Cause 1 of 1 — the EventBridgeDestination's event-bus restriction is not enforced.
AWS: SESv2 only publishes configuration-set events to the account's DEFAULT event bus. CreateConfigurationSetEventDestination (Smithy: POST /v2/email/configuration-sets/{ConfigurationSetName}/event-destinations, success code 200, models BadRequestException errorKind client httpError 400) and UpdateConfigurationSetEventDestination (PUT .../event-destinations/{EventDestinationName}, same BadRequestException) both refuse an EventDestination whose EventBridgeDestination.EventBusArn names any bus other than the default one. The refusal is HTTP 400 BadRequestException with the fixed message `Invalid Event Bridge destination, must be default bus.` (no variable part). A refused create must add no destination, and a refused update must leave the existing destination on the default bus with its other fields (Enabled, MatchingEventTypes) untouched. The sibling call naming `arn:<partition>:events:<region>:<account>:event-bus/default` succeeds and is readable through GetConfigurationSetEventDestinations.
Emulator: the create naming a custom bus returns HTTP 200 with an empty body (observed: `{'ResponseMetadata': {'HTTPStatusCode': 200, ...}}`), i.e. the destination is accepted; no BadRequestException is raised on either the create or the update path.
Why: a client configuring SES event publishing against a custom bus gets a green response and a destination that cannot work — events never arrive on that bus — and the accepted destination also diverges from AWS when the configuration set is read back. An error-handling path that depends on the BadRequestException never runs.
Fix: the EventBridgeDestination validation in the SESv2 CreateConfigurationSetEventDestination and UpdateConfigurationSetEventDestination handlers (both must reject a non-default EventBusArn before any state is written).
Traces: events__sesv2.configuration-set-event-destination-custom-event-bus-rejected.

## Evidence
Expected: HTTP 400 BadRequestException with Message `Invalid Event Bridge destination, must be default bus.` from both CreateConfigurationSetEventDestination and UpdateConfigurationSetEventDestination when EventBridgeDestination.EventBusArn names a custom bus.
Actual: `AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '2', ...}}}` — the create was accepted, so the update leg was never reached.

## Steps to Reproduce
1. `events create-event-bus --name <custom>`.
2. `sesv2 create-configuration-set --configuration-set-name <cs>`.
3. Create a control destination with EventBridgeDestination.EventBusArn = `arn:aws:events:<region>:<account>:event-bus/default` and confirm it reads back.
4. `sesv2 create-configuration-set-event-destination --configuration-set-name <cs> --event-destination-name ed-eb-custom --event-destination '{"Enabled":true,"MatchingEventTypes":["SEND"],"EventBridgeDestination":{"EventBusArn":"arn:aws:events:<region>:<account>:event-bus/<custom>"}}'` — must be refused with BadRequestException.
5. Repeat against the control destination with update-configuration-set-event-destination — must be refused and leave the destination on the default bus.
