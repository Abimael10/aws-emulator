Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__fis__iam__s3__sts.experiment-template-invalid-stop-condition-source-rejected cloudwatch__fis__iam__s3__sts.update-experiment-template-rejects-invalid-pre-experiment-duration
Issue-Key: cloudwatch__fis__iam__s3__sts.experiment-template-invalid-stop-condition-source-rejected|expect_error#17|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Key: cloudwatch__fis__iam__s3__sts.update-experiment-template-rejects-invalid-pre-experiment-duration|expect_error#10|AssertionError|raise AssertionError(f"expected {code!r} ({message!r}), but the call succeeded and returned {result!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# fis: 2 net-new causes — the invalid stop-condition source refusal is an emulator-authored sentence, and UpdateExperimentTemplate accepts an invalid experimentReportConfiguration.preExperimentDuration

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateExperimentTemplate

## Description
This issue bundles 2 net-new root causes in the fis (Fault Injection Simulator) emulator. Each is numbered with its own failing check id; both must be closed before submitting.

Cause 1 of 2: CreateExperimentTemplate's refusal of an unknown stopConditions source uses an emulator-authored message.
AWS: Smithy (fis 2020-12-01) models CreateExperimentTemplate with stopConditions required and declares ValidationException (errorKind client, httpError 400). The only legal sources are 'none' and 'aws:cloudwatch:alarm'; any other value is refused with HTTP 400, Error.Code 'ValidationException' and the message 'Invalid source for Stop Condition: [<source>].' — the fixed text is 'Invalid source for Stop Condition: [' and '].', and the single variable part is the source string taken from the request.
Emulator: the status and Error.Code are correct, but the message is 'stopConditions[0].source must be none or aws:cloudwatch:alarm, got "<source>"' — an emulator-authored sentence that reports the input path and the allowed set instead of AWS's wording.
Why: the message is a requirement of the error class. A client matching FIS's wording (or surfacing it to a user) diverges, and the bracketed-source form AWS uses is what identifies the offending value in logs.
Fix: CreateExperimentTemplate's stopConditions validation message construction (also UpdateExperimentTemplate, which shares the rule).
Failing check: cloudwatch__fis__iam__s3__sts.experiment-template-invalid-stop-condition-source-rejected.

Cause 2 of 2: UpdateExperimentTemplate accepts an experimentReportConfiguration.preExperimentDuration that is not a duration, and stores it.
AWS: UpdateExperimentTemplate (PATCH /experimentTemplates/{id}) takes experimentReportConfiguration -> UpdateExperimentTemplateReportConfigurationInput and declares ValidationException (client, httpError 400). preExperimentDuration must be an ISO-8601 duration (e.g. 'PT1M'); a non-duration string is refused with HTTP 400, Error.Code 'ValidationException' and the fixed message 'The preExperimentDuration parameter value for the experimentReportConfiguration is invalid. Specify a valid duration value and try again.' (no variable part), leaving the template unchanged.
Emulator: the call SUCCEEDS with HTTP 200 and the response's experimentTemplate.experimentReportConfiguration carries preExperimentDuration: 'not-a-duration' verbatim, with lastUpdateTime advanced — the garbage value is persisted.
Why: the template is stored with an unparseable report window, so the refusal a client relies on to catch a bad duration never comes and the error surfaces (if at all) when an experiment runs and the report cannot be generated. Any client validating against FIS's refusal silently ships a broken template.
Fix: UpdateExperimentTemplate's experimentReportConfiguration validation (the preExperimentDuration/postExperimentDuration duration parse), ahead of the state write; CreateExperimentTemplate's equivalent branch should be checked with it.
Failing check: cloudwatch__fis__iam__s3__sts.update-experiment-template-rejects-invalid-pre-experiment-duration.

## Evidence
Cause 1 — Expected: ('ValidationException', 'Invalid source for Stop Condition: [sim2real-not-a-stop-condition-source].'). Actual message: 'stopConditions[0].source must be none or aws:cloudwatch:alarm, got "sim2real-not-a-stop-condition-source"' (the source value is per-run; the divergence is the sentence).

Cause 2 — Expected: ValidationException 'The preExperimentDuration parameter value for the experimentReportConfiguration is invalid. Specify a valid duration value and try again.' Actual: 'AssertionError: expected \'ValidationException\' (...), but the call succeeded and returned {"ResponseMetadata": {"HTTPStatusCode": 200, ...}, "experimentTemplate": {... "experimentReportConfiguration": {"outputs": {"s3Configuration": {...}}, "dataSources": {"cloudWatchDashboards": [...]}, "preExperimentDuration": "not-a-duration"}}}' — ids, ARNs and timestamps are per-run; the divergence is the 200 and the stored value.

## Steps to Reproduce
Cause 1: create an IAM role FIS can assume, then CreateExperimentTemplate(clientToken, description, roleArn, actions={'wait': {'actionId':'aws:fis:wait','parameters':{'duration':'PT1M'}}}, stopConditions=[{'source':'sim2real-not-a-stop-condition-source'}]) and read (Error.Code, Error.Message).
Cause 2: CreateExperimentTemplate with stopConditions=[{'source':'none'}] and a valid experimentReportConfiguration (an S3 output bucket/prefix and a cloudWatchDashboards data source); then UpdateExperimentTemplate(id, experimentReportConfiguration={..., 'preExperimentDuration': 'not-a-duration'}) and observe HTTP 200 with the value stored instead of a 400 ValidationException.
