Issue-Source: trace-sweep-divergence
Issue-Trace: route53-traffic-policy-versioning
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-traffic-policy-versioning
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|GetTrafficPolicy|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 6ee685494fcb976e5e853eb07eb6238c0bcf2fe0

# Route 53 GetTrafficPolicy 404 NoSuchTrafficPolicy body carries an emulator-authored Message element

> _Note: this issue was identified against the code base at commit `6ee685494fcb976e5e853eb07eb6238c0bcf2fe0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTrafficPolicy

## Description
ONE cause: the emulator's NoSuchTrafficPolicy refusal body for GetTrafficPolicy serializes an Error.Message element that AWS does not serialize for this error.

Cause 1 of 1: GetTrafficPolicy on a traffic-policy id/version that does not exist answers 404 with an extra Error.Message (fixture route53-traffic-policy-versioning, step task#10).
AWS: GET /2013-04-01/trafficpolicy/{Id}/{Version} for a policy that does not exist answers HTTP 404 with the REST-XML ErrorResponse envelope whose Error carries the error code NoSuchTrafficPolicy, and the recorded body carries NO Message element inside Error at all - the error is identified by its code alone. Smithy: NoSuchTrafficPolicy is an @error("client") with @httpError(404) and a SINGLE optional member `message` (target ErrorMessage) whose documentation is empty; GetTrafficPolicy's modeled errors are InvalidInput (400) and NoSuchTrafficPolicy (404). Nothing in the model requires a message, and the recording shows AWS omitting the element - so "404, code NoSuchTrafficPolicy, no Message element" is recorded behavior only, and it is the behavior the fixture pins.
Emulator: answers 404 (status correct) but adds $.ErrorResponse.Error.Message with the emulator-authored fixed text "The policy does not exist." - text that appears in neither the model nor the recording. There are no variable parts in that string: it is wholly invented, so no part of it comes from the request (the policy id and version) or from emulator state.
Why: a client that branches on the presence of an error Message, logs it, or surfaces it to a user sees a message where AWS sends none, and any client or test asserting the recorded refusal body byte-for-byte (or comparing against AWS's shape of this error class) fails. An emulator-authored message also teaches callers to match on text AWS will never send, so error handling written against the emulator breaks against AWS.
Fix: the GetTrafficPolicy handler's not-found refusal construction / the REST-XML error serializer for NoSuchTrafficPolicy - emit the ErrorResponse envelope with the code only and leave `message` unset for this error class, rather than defaulting a hand-written string.
Related: the open issue 20261111T033513Z-cloudformation-route53-createtrafficpolicy-s-invalid-document-refusal-is-emulator-authored.md covers CreateTrafficPolicy's invalid-document refusal wording; it does not cover the GetTrafficPolicy not-found body filed here, and the replay reports this cluster as unowned.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator. One mismatch cluster (status matched; only the body diverged):

GetTrafficPolicy, fixture route53-traffic-policy-versioning#task#10, expected 404 / actual 404:
  $.ErrorResponse.Error.Message: unexpected_present (expected None, got 'The policy does not exist.')

Expected (AWS, recorded): 404, ErrorResponse.Error with code NoSuchTrafficPolicy and no Message element.
Actual (emulator): 404 with ErrorResponse.Error.Message = "The policy does not exist." (fixed emulator-authored text, no request- or state-derived parts).

Model evidence: smithy_query on services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json - GetTrafficPolicy errors = [InvalidInput 400, NoSuchTrafficPolicy 404]; NoSuchTrafficPolicy = structure, errorKind client, httpError 404, members {message: ErrorMessage} (optional, empty doc), shape doc "No traffic policy exists with the specified ID."

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Replay fixture route53-traffic-policy-versioning up to step task#10 - GET /2013-04-01/trafficpolicy/{Id}/{Version} for a traffic policy id/version that does not exist.
3. Read the 404 body: ErrorResponse.Error must carry the code NoSuchTrafficPolicy and must NOT contain a Message element.
4. The emulator instead returns Error.Message = "The policy does not exist."; the step passes only when that element is gone.
