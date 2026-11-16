Issue-Source: trace-sweep-divergence
Issue-Trace: appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names
Issue-Shape-Id: com.amazonaws.appconfigdata#AppConfigData
Issue-Key: appconfigdata|StartConfigurationSession|4xx->4xx|$
Issue-Commit: ce79cfcc2d5bdf31295b2bed10a3822c9ac41993

# appconfigdata StartConfigurationSession serves a populated BadRequestException payload where AWS's 400 body is empty

> _Note: this issue was identified against the code base at commit `ce79cfcc2d5bdf31295b2bed10a3822c9ac41993`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartConfigurationSession

## Description
This issue carries 1 cause, in appconfigdata.

Cause 1 of 1: StartConfigurationSession refuses an identifier whose form the operation cannot read with HTTP 400 — the right status class — but puts a fully populated BadRequestException document on the wire where the recorded AWS response body for that same refusal is an empty JSON object.

  AWS: recorded behavior only for the BODY. Smithy grounding: com.amazonaws.appconfigdata#StartConfigurationSession is POST /configurationsessions, success code 201, and models BadRequestException (httpError 400), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429). ApplicationIdentifier / EnvironmentIdentifier / ConfigurationProfileIdentifier all target Identifier, a plain string constrained only by @length 1..128 — the model places no pattern on them and documents each as "The application ID or the application name" (resp. environment / configuration profile), so the model says nothing about which error an identifier in some other form earns. BadRequestException's optional members are Message (String), Reason (BadRequestReason, sole value 'InvalidParameters') and Details (BadRequestDetails union with InvalidParameters -> map<String, InvalidParameterDetail{Problem: InvalidParameterProblem in [Corrupted, Expired, PollIntervalNotSatisfied]}>). The recording for this step shows AWS answering HTTP 400 with the body `{}`: no __type discriminator, no Message, no Reason and no Details — i.e. for this refusal AWS commits only to the 400 status (with the error type carried out of band in the x-amzn-errortype header) and asserts nothing in the payload about WHICH request parameter it judged unreadable. In the response body there is no fixed text and no variable part at all: the whole document is the empty object.

  Emulator: the same step answers 400 (status agrees) with the body {"__type": "BadRequestException", "Message": "The input fails to satisfy the constraints specified by the service.", "Reason": "InvalidParameters", "Details": {"InvalidParameters": {"ApplicationIdentifier": {"Problem": "Corrupted"}}}} — four members where the recording has none. The Message is verbatim the Smithy @documentation sentence of BadRequestException reused as a runtime message; Reason and Details are synthesized attribution naming the member the handler decided was at fault and classifying it as InvalidParameterProblem 'Corrupted'; `__type` is an in-body error discriminator the recorded restJson1 response does not use.

  Why: a client that reads the error document, not just the status, is told something AWS never told it. Reason='InvalidParameters' plus Details.InvalidParameters.ApplicationIdentifier.Problem='Corrupted' is a positive, machine-readable assertion that the application identifier specifically was unparseable; callers and test suites that branch on BadRequestDetails (to re-prompt for one field, to retry the other two tiers, or to distinguish a malformed parameter from an expired token, since 'Expired' and 'PollIntervalNotSatisfied' share this same union) take a path that never exists against AWS, and any assertion that the 400 body is empty fails. The extra in-body `__type` also makes the emulator's error framing differ from the recorded wire form even for generic error-shape checks. The outcome class is right, so this will not break a caller that only inspects the status — which is why it is MEDIUM rather than HIGH — but every consumer of the payload sees fabricated detail.

  Fix: the error-construction path of this operation's handler and the error serializer it feeds — services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/start_configuration_session.rs builds the refusal in malformed_identifier_refusal() (filling message/reason/details from local constants) and the handler returns it before any control-plane resolution. The refusal must serialize to the body AWS records for this class — the empty JSON object, with the error type conveyed by the response's error-type header rather than an in-body __type — rather than populating Message/Reason/Details from the model's documentation string and the handler's own per-member attribution. Whether the handler should instead treat an unreadable identifier as an unresolvable one (ResourceNotFoundException, 404) is NOT indicated here: the recording agrees with the emulator on 400, so only the payload is at fault.

  Traces: appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names#task#10 (cluster affected_count 1, stub_path empty, owned_by empty).

Related open issues that do NOT cover this cluster: the appconfig deployment-State casing bundle 20261116T014021Z-appconfig-serves-deployment-state-in-pascalcase-across-the.md owns the OTHER divergence of this replay (appconfig StartDeployment $.State), is filed against a different service and a different operation, and binds none of the StartConfigurationSession error-body steps filed here.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up true, /__admin/health 200): 8 fixtures replayed, 88 steps, 81 passed, 7 mismatched, 0 errored, 0 blocked, score 92.0%, divergence_count 2. One of the two clusters is reported owned_by 20261116T014021Z-appconfig-serves-deployment-state-in-pascalcase-across-the.md and is skipped; the cluster below is reported with stub_path empty and owned_by empty.

Cause 1 — fixture appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names, step task#10, operation StartConfigurationSession, service appconfigdata, verdict mismatch, divergence_key service=appconfigdata|StartConfigurationSession|status=400->400|keys=$.
  expected status 400, actual status 400 (status agrees).
  Body, at JSON path `$`: value_mismatch —
    expected: {}
    actual:   {"__type": "BadRequestException", "Message": "The input fails to satisfy the constraints specified by the service.", "Reason": "InvalidParameters", "Details": {"InvalidParameters": {"ApplicationIdentifier": {"Problem": "Corrupted"}}}}
  So the entire error document is net-new: four members are served where the recorded response carries none.

Model basis (services/service-com-amazonaws-appconfigdata-appconfigdata/smithy-model/com-amazonaws-appconfigdata-appconfigdata.json): StartConfigurationSession -> POST /configurationsessions, code 201, errors [BadRequestException 400, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429]; input members ApplicationIdentifier/EnvironmentIdentifier/ConfigurationProfileIdentifier are required Identifier (@length 1..128, no @pattern); BadRequestException members are Message, Reason (BadRequestReason: ['InvalidParameters']) and Details (BadRequestDetails union -> InvalidParameters map of InvalidParameterDetail{Problem: InvalidParameterProblem ['Corrupted','Expired','PollIntervalNotSatisfied']}). The model makes all three BadRequestException members optional and says nothing that requires them to be populated for a malformed identifier; the recording shows them absent.

Emulator source backing the Emulator line: services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/start_configuration_session.rs — PROBLEM_CORRUPTED (line 147), identifier_members() (lines 152-167), is_arn_form() (lines 175-177), malformed_identifier_refusal() (lines 194-236, which fills message: INPUT_FAILS_CONSTRAINTS_MESSAGE, reason: REASON_INVALID_PARAMETERS, details: BadRequestDetails::InvalidParameters), and the handler's early return at lines 307-309.

## Steps to Reproduce
1. Stand up the AppConfig control-plane state the fixture sets up (application, environment, configuration profile with a deployed version), as fixture appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names does in its setup phase.
2. Call appconfigdata StartConfigurationSession (POST /configurationsessions) with an ApplicationIdentifier that is neither an id nor a name for its tier (the fixture passes an ARN-form value), and valid identifiers for the other two tiers.
3. Observe the response status: both AWS and the emulator answer HTTP 400, so the status assertion passes.
4. Read the response body. AWS returns the empty JSON object `{}` (error type only in the x-amzn-errortype header). The emulator returns {"__type":"BadRequestException","Message":"The input fails to satisfy the constraints specified by the service.","Reason":"InvalidParameters","Details":{"InvalidParameters":{"ApplicationIdentifier":{"Problem":"Corrupted"}}}}.
5. Replay appconfigdata-lacs-traj-session-logical-ids-rejected-per-tier-then-names#task#10 to confirm: the step is reported as a mismatch at JSON path `$` with expected {} and the populated document as actual.
