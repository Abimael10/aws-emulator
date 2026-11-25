Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-test-configuration-errors
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-test-configuration-errors
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|CreateTest|2xx->4xx|$.test
Issue-Key: resiliencehubv2|CreateTest|2xx->4xx|status
Issue-Key: resiliencehubv2|CreateTest|4xx->4xx|$.message
Issue-Key: resiliencehubv2|CreateTest|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: resiliencehubv2|CreateTest|4xx->4xx|status
Issue-Commit: 8bf2bd5c40cd5968f6fa936e1c0608724e59a2f4

# resiliencehubv2 CreateTest refuses with a blanket multi-AZ RTO ValidationException instead of validating parameters, reporting conflict, or creating the test

> _Note: this issue was identified against the code base at commit `8bf2bd5c40cd5968f6fa936e1c0608724e59a2f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTest

## Description
CreateTest (POST /v2/create-test) in the resiliencehubv2 emulator has one dominant refusal path: whenever the service's associated resilience policy does not define a multi-AZ RTO it answers 400 ValidationException with the message "Cannot create test for service '<serviceArn>'. Ensure the resilience policy associated with the service defines a multi-AZ RTO." That check runs ahead of (and in place of) required-parameter validation, the one-test-per-template conflict check, and successful creation, so three different modelled outcomes collapse into the same 400. A fourth, independent cause is the wording of the test-template not-found refusal. This bundle contains 4 causes; all four must be closed.

No existing open resiliencehubv2 issue covers CreateTest (the open ones cover TagResource/UntagResource Content-Length, effectivePolicyValues, user-journey/system responses, and the DeleteUserJourney stub).

Cause 1 of 4 — test-template not-found message wording (fixture resiliencehubv2-test-configuration-errors, step task#3).
AWS: Smithy: CreateTest.testTemplateArn is a required ServiceOwnedArn; CreateTest declares ResourceNotFoundException (httpError 404, doc "Resource not found."). The recording shows AWS answering 404 with the message "The specified test template does not exist. Verify the test template ARN and try again." — that message is wholly fixed text for this error class; it does not interpolate the supplied ARN or template id.
Emulator: answers 404 (status correct) but with the message "TestTemplate not found: <template-id>" — an internal "<Type> not found: <id>" template that substitutes the request's template identifier.
Why: clients and tests that match on the documented sentence (or surface it to operators) see a foreign string; the message is a contract surface for this error class, so error-message assertions and user-facing copy diverge from AWS.
Fix: the CreateTest handler's test-template lookup/refusal path — emit the fixed AWS sentence for an unknown test template instead of the generic internal not-found formatter.
Traces: resiliencehubv2-test-configuration-errors#task#3.

Cause 2 of 4 — required test parameter is never validated; the multi-AZ RTO refusal fires instead (fixture resiliencehubv2-test-configuration-errors, step task#4).
AWS: Smithy: CreateTest.parameters is a TestParameters map (max 20 entries, key ParameterKey, value StringList); CreateTest declares ValidationException (httpError 400, members message/reason/fieldList). The recording shows AWS answering 400 with the message "The 'region' parameter is required." when a test template whose parameter set includes region is created without it. The fixed text is "The '<parameterKey>' parameter is required."; the only variable part is the missing parameter key, taken from the request's template parameter schema versus the submitted parameters map.
Emulator: answers 400 but with the blanket multi-AZ RTO message quoted above, so a missing-required-parameter input error is reported as a policy precondition failure.
Why: a client that omits a required template parameter is told to change its resilience policy rather than to supply the parameter; the message names neither the offending member nor the real cause, so remediation and fieldList-driven client handling are wrong.
Fix: CreateTest's input validation — validate the selected test template's required parameters against the request's parameters map and raise ValidationException naming the missing key, before any policy/RTO precondition is evaluated.
Traces: resiliencehubv2-test-configuration-errors#task#4.

Cause 3 of 4 — a valid CreateTest is refused outright (fixture resiliencehubv2-test-configuration-errors, step task#5).
AWS: Smithy: CreateTest http POST /v2/create-test, success code 200, output CreateTestResponse with required member test (shape Test). Doc: "Creates a test for a service by configuring a test template. Each service has one test per template." The recording shows AWS answering 200 with the test object populated from the template (actions list, name, parameters, roleName, serviceArn, testTemplateArn, successfulTestRuns 0, totalTestRuns 0, plus generated testId/creationTime).
Emulator: answers 400 ValidationException with the multi-AZ RTO message and no $.test member at all, i.e. the precondition is applied to templates it does not govern (a dependency-validation template needs no multi-AZ RTO) so creation never happens.
Why: the operation cannot succeed for these templates, so every downstream flow (test listing, test runs, test deletion) is unreachable — the emulator has no state to drive them.
Fix: CreateTest's handler/state — scope the multi-AZ RTO precondition to the templates that actually require it and otherwise mint the Test from the template and persist it, returning 200 with the required test member.
Traces: resiliencehubv2-test-configuration-errors#task#5.

Cause 4 of 4 — duplicate test per template is not reported as a conflict (fixture resiliencehubv2-test-configuration-errors, step task#6).
AWS: Smithy: CreateTest declares ConflictException (httpError 409, doc "Conflict — resource already exists.", required member message); the operation doc states "Each service has one test per template." The recording shows AWS answering 409 with the message "A test already exists for this test template on this service. Only one test can be created per template on a service. Update the existing test, delete it, or use a different template." — fixed text for this error class, with no interpolated values; the response's x-amzn-errortype must name ConflictException.
Emulator: answers 400 with x-amzn-errortype ValidationException and the multi-AZ RTO message, so the per-template uniqueness rule is not enforced or reported at all (and, given cause 3, the first create never landed either).
Why: a client cannot distinguish "already exists" from an input error — retry/adopt-existing logic keyed on ConflictException/409 never triggers, and idempotent provisioning loops break.
Fix: CreateTest's handler and state — after input validation, check for an existing test for the (serviceArn, testTemplateArn) pair and raise ConflictException (409, the fixed AWS sentence, x-amzn-errortype ConflictException).
Traces: resiliencehubv2-test-configuration-errors#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (fixture resiliencehubv2-test-configuration-errors), 4 net-new divergence clusters, all CreateTest:

1) step task#3 — status 404 expected, 404 actual. $.message: expected 'The specified test template does not exist. Verify the test template ARN and try again.', actual 'TestTemplate not found: aws-zonal-autoshift:rtzz999'.

2) step task#4 — status 400 expected, 400 actual. $.message: expected "The 'region' parameter is required.", actual "Cannot create test for service 'arn:aws:resiliencehub:<region>:<account>:service/<id>'. Ensure the resilience policy associated with the service defines a multi-AZ RTO."

3) step task#5 — status: expected 200, got 400. $.test: missing (expected the created Test object with actions [aws:ssm:send-command, aws:ecs:task-network-packet-loss, aws:eks:pod-network-packet-loss], name, parameters {duration, region}, roleName, serviceArn, testTemplateArn .../test-template/aws-dependency-validation:<id>, successfulTestRuns 0, totalTestRuns 0, generated testId/creationTime), got None. Body carried the multi-AZ RTO ValidationException message instead.

4) step task#6 — status: expected 409, got 400. $.message: expected 'A test already exists for this test template on this service. Only one test can be created per template on a service. Update the existing test, delete it, or use a different template.', actual the multi-AZ RTO message. $headers.x-amzn-errortype: expected the ConflictException shape, got 'ValidationException'.

Model evidence: Smithy CreateTest — POST /v2/create-test, success 200, output required member test:Test; errors AccessDeniedException(403), ConflictException(409), InternalServerException(500), ResourceNotFoundException(404), ValidationException(400); input serviceArn (required), testTemplateArn (required), parameters:TestParameters(map, max 20), roleName, stopConditions, loggingConfiguration. ConflictException doc "Conflict — resource already exists."; ResourceNotFoundException doc "Resource not found." with optional resourceId/resourceType; ValidationException carries message/reason/fieldList.

## Steps to Reproduce
Replay fixture resiliencehubv2-test-configuration-errors against the emulator (steps task#3, task#4, task#5, task#6):
1. Create a system and a service, attach a resilience policy that does NOT define a multi-AZ RTO.
2. POST /v2/create-test with a testTemplateArn that does not exist (e.g. a bogus zonal-autoshift template id) -> emulator returns 404 with 'TestTemplate not found: <id>' instead of AWS's fixed sentence 'The specified test template does not exist. Verify the test template ARN and try again.' (task#3).
3. POST /v2/create-test for a real template omitting its required 'region' parameter -> emulator returns the multi-AZ RTO ValidationException instead of "The 'region' parameter is required." (task#4).
4. POST /v2/create-test for the aws-dependency-validation test template with full, valid parameters (duration, region), a roleName and the service ARN -> emulator returns 400 with the multi-AZ RTO message and no $.test; AWS returns 200 with the created Test (task#5).
5. Repeat the same valid POST /v2/create-test for the same (serviceArn, testTemplateArn) pair -> emulator returns 400 ValidationException (multi-AZ RTO) ; AWS returns 409 ConflictException with 'A test already exists for this test template on this service...' and x-amzn-errortype ConflictException (task#6).
