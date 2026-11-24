Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.data-plane-v2-logging-options-configured-vs-unset cloudformation__iot.cfn-iot-logging-and-resource-specific-logging-read-back-then-disabled-after-delete
Issue-Key: iot|GetV2LoggingOptions|NotConfiguredException
Issue-Commit: 05c7972ccec5de40063e90508b79406e82288e9e

# iot: GetV2LoggingOptions' NotConfiguredException message says "No logging levels have been set." instead of AWS's "No logging options have been set."

> _Note: this issue was identified against the code base at commit `05c7972ccec5de40063e90508b79406e82288e9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetV2LoggingOptions

## Description
One cause (Cause 1 of 1): the NotConfiguredException GetV2LoggingOptions raises for an account that never called SetV2LoggingOptions carries the wrong message text.

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) binds GET /v2LoggingOptions and models NotConfiguredException (client error, HTTP 404) for GetV2LoggingOptions; the model does not spell the message text, so the wording is recorded behavior only. The recording shows AWS answering 404 NotConfiguredException with the message `SetV2LoggingOptions was not previously called. No logging options have been set.` — the whole string is FIXED text for this error class: there are no variable parts (no account id, no name, no timestamp) interpolated into it. Clients discriminate the "v2 logging was never configured" state on that exact (code, message) pair, which is why both failing checks compare the message verbatim.

Emulator: the status (404) and the error code (NotConfiguredException) are right, but the message is `SetV2LoggingOptions was not previously called. No logging levels have been set.` — "logging levels" where AWS says "logging options". V2 logging *levels* are a different, per-target resource (SetV2LoggingLevel/ListV2LoggingLevels), so the wording also misnames the thing that is unset.

Why: a client that keys on the documented (code, message) pair to recognise "never configured" — exactly what both checks' probe helpers do — does not recognise the emulator's refusal and re-raises it as an unexpected error. In iot.data-plane-v2-logging-options-configured-vs-unset the assertion on the message fails outright. In cloudformation__iot.cfn-iot-logging-and-resource-specific-logging-read-back-then-disabled-after-delete the very first probe (`_prior_logging_options`, which swallows only the exact AWS pair) re-raises, so the check aborts before CloudFormation is ever exercised: botocore.errorfactory.NotConfiguredException escapes with the emulator's wording. Note this means the AWS::IoT::Logging / AWS::IoT::ResourceSpecificLogging legs of that check were never reached and may hide further, currently unobserved causes; they can only be judged once this message is corrected.

Fix: the message producer behind this operation's error path — `crate::state::missing_v2_logging_options_message()`, as called by the GetV2LoggingOptions handler when `v2_logging_options` is None. The status and error code need no change; only the fixed message text does.

Traces: iot.data-plane-v2-logging-options-configured-vs-unset (mismatch: expected message 'SetV2LoggingOptions was not previously called. No logging options have been set.', got 'SetV2LoggingOptions was not previously called. No logging levels have been set.'); cloudformation__iot.cfn-iot-logging-and-resource-specific-logging-read-back-then-disabled-after-delete (error: NotConfiguredException escapes the unset-logging probe with the same wording). Code citations: services/service-com-amazonaws-iot-awsiotservice/src/ops/get_v2_logging_options.rs lines 160-170 (the None branch calling the message helper).

## Evidence
Expected (AWS, and what both checks assert): HTTP 404, code NotConfiguredException, message exactly `SetV2LoggingOptions was not previously called. No logging options have been set.` (all fixed text, no request- or state-derived parts).
Actual (emulator): HTTP 404, code NotConfiguredException, message `SetV2LoggingOptions was not previously called. No logging levels have been set.`

Replay output:
- iot.data-plane-v2-logging-options-configured-vs-unset — AssertionError: expected message 'SetV2LoggingOptions was not previously called. No logging options have been set.', got 'SetV2LoggingOptions was not previously called. No logging levels have been set.'
- cloudformation__iot.cfn-iot-logging-and-resource-specific-logging-read-back-then-disabled-after-delete — botocore.errorfactory.NotConfiguredException: An error occurred (NotConfiguredException) when calling the GetV2LoggingOptions operation: SetV2LoggingOptions was not previously called. No logging levels have been set. (raised out of the check's precondition probe, which only tolerates AWS's exact wording)

## Steps to Reproduce
1. Start the emulator with a fresh IoT state (no SetV2LoggingOptions has ever been called for the account/region).
2. Call iot:GetV2LoggingOptions (GET /v2LoggingOptions).
3. Observe 404 NotConfiguredException whose message ends "No logging levels have been set."; AWS ends it "No logging options have been set."
4. Equivalently, replay iot.data-plane-v2-logging-options-configured-vs-unset (asserts the message directly) or cloudformation__iot.cfn-iot-logging-and-resource-specific-logging-read-back-then-disabled-after-delete (aborts in its unset-logging precondition probe).
