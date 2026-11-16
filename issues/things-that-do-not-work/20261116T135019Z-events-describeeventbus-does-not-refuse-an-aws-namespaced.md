Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.create-event-bus-reserved-and-malformed-names-rejected
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.create-event-bus-reserved-and-malformed-names-rejected|run#13|AssertionError|assert (aws_code, aws_message) == (
Issue-Commit: 366da4842fbf9ceddd9497ec8aca896c38ee89fa

# events: DescribeEventBus does not refuse an 'aws.'-namespaced bus name — answers ResourceNotFoundException instead of ValidationException

> _Note: this issue was identified against the code base at commit `366da4842fbf9ceddd9497ec8aca896c38ee89fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEventBus

## Description
1 net-new cause.

Cause 1 of 1: DescribeEventBus applies no event-bus name-namespace validation before the store lookup, so a name in the reserved 'aws.' namespace is reported as a missing resource rather than as an invalid name.

AWS: EventBridge reserves the 'aws.' prefix for AWS-owned and partner buses; CreateEventBus refuses such a name with HTTP 400 ValidationException and the message "EventBus name starting with 'aws.' is not valid." (the emulator already does this correctly — those assertions in the same check pass). Because a customer bus in that namespace can never exist, DescribeEventBus refuses the same name the same way, BEFORE any lookup: HTTP 400, error code ValidationException, message "EventBus name starting with 'aws.' is not valid." — that message is entirely fixed text, it interpolates no request or state value. Smithy: DescribeEventBus.Name targets EventBusNameOrArn (len 1..1600, pattern ^(arn:aws[\w-]*:events:...:event-bus\/)?[/\.\-_A-Za-z0-9]+$), which permits '.' and therefore does not itself reject the name; the model declares only InternalException and ResourceNotFoundException for the operation, so the ValidationException refusal is recorded behavior only (it is a protocol-level validation error, as it is for CreateEventBus). The recorded response for a name starting with 'aws.' is HTTP 400 / ValidationException with that message.

Emulator: DescribeEventBus(Name='aws.<name>') returns HTTP 400 with code 'ResourceNotFoundException' and message 'Event bus aws.<name> does not exist.' — the handler resolves the effective bus name and goes straight to the store, so the reserved-namespace rule is never consulted and the name is treated as merely absent.

Why: a client cannot distinguish "you asked for a name that is structurally impossible" from "this bus has not been created yet". Code that retries/creates on ResourceNotFoundException will loop or try to create an 'aws.'-prefixed bus (which CreateEventBus then refuses with a different error class), and code that branches on ValidationException for input repair never sees it. The HTTP body also carries the wrong __type, so SDK error-type matching (ValidationException vs ResourceNotFoundException) fails.

Fix: add the shared event-bus name validation that CreateEventBus already performs to the DescribeEventBus handler's input validation, running it before the store lookup, so a name in the reserved 'aws.' namespace is refused as a ValidationException with AWS's wording rather than falling through to the not-found path. The same reserved-namespace check should be shared by every operation that resolves a bus by name.

Traces: events.create-event-bus-reserved-and-malformed-names-rejected (the check's CreateEventBus refusals for 'aws.' prefix, '/' in the name and the >256-char name all pass; it fails on the final DescribeEventBus('aws.<token>') assertion).

## Evidence
Check events.create-event-bus-reserved-and-malformed-names-rejected, final assertion:

  aws_code, aws_message = _error(lambda: events.describe_event_bus(Name=f"aws.{token}"))
  assert (aws_code, aws_message) == ("ValidationException", "EventBus name starting with 'aws.' is not valid.")

Expected (AWS): HTTP 400, Error.Code = 'ValidationException', Error.Message = "EventBus name starting with 'aws.' is not valid."
Actual (emulator): HTTP 400, Error.Code = 'ResourceNotFoundException', Error.Message = 'Event bus aws.sim2real-behav-bus-<id> does not exist.' (the bus-name part is a per-run value; the defect is the error code/class and the message template).

AssertionError reported by the suite:
  "a custom bus in the 'aws.' namespace can never exist, so DescribeEventBus must refuse the name the same way; got 'ResourceNotFoundException' 'Event bus aws.sim2real-behav-bus-28a4ea335cd6 does not exist.'"

Code path: services/service-com-amazonaws-eventbridge-awsevents/src/ops/describe_event_bus.rs lines 150-165 — the handler computes crate::state::effective_bus_name(...) and immediately does store.bus(&region, &bus_name), returning ResourceNotFoundException(crate::state::no_such_event_bus(&bus_name)) when absent; there is no name-namespace/format validation step.

## Steps to Reproduce
1. Boot the emulator and point an EventBridge client at it.
2. CreateEventBus(Name='<token>-ok') to confirm the service is serving (succeeds).
3. Call CreateEventBus(Name='aws.<token>') — observe the correct refusal: 400 ValidationException "EventBus name starting with 'aws.' is not valid."
4. Call DescribeEventBus(Name='aws.<token>').
   Expected: 400 ValidationException with message "EventBus name starting with 'aws.' is not valid."
   Observed: 400 ResourceNotFoundException with message 'Event bus aws.<token> does not exist.'
5. Re-replay check events.create-event-bus-reserved-and-malformed-names-rejected to confirm the fix.
