Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode
Issue-Key: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode|expect_error#11|AssertionError|raise AssertionError(f"expected {(code, message)!r}, but the call succeeded: {result!r}"[:900])
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# CreateApplication accepts the deprecated ZEPPELIN-FLINK-1_0 runtime instead of refusing it

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApplication

## Description
Cause 1 of 1: a deprecated RuntimeEnvironment is accepted and an application is created.

AWS: Smithy (kinesisanalyticsv2 CreateApplication) declares InvalidArgumentException; the recording shows RuntimeEnvironment `ZEPPELIN-FLINK-1_0` refused with code `InvalidArgumentException` and message `Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment` — fixed text apart from the runtime identifier, which is taken from the request. The refusal holds whether or not ApplicationMode is INTERACTIVE, and nothing is created either time.

Emulator: the call succeeds. It returns an ApplicationDetail with `RuntimeEnvironment: 'ZEPPELIN-FLINK-1_0'`, `ApplicationStatus: 'READY'`, ApplicationVersionId 1 and a real ARN: `AssertionError: expected ('InvalidArgumentException', 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. ...'), but the call succeeded: {'ApplicationDetail': {...}}`.

Why: a client building on a runtime AWS no longer accepts gets a green light and an application that cannot exist on real AWS; the deprecation is discovered only on the real account, and the emulator's state now holds an application in an impossible configuration.

Fix: CreateApplication's RuntimeEnvironment validation — refuse deprecated runtime values with InvalidArgumentException and that message, independent of ApplicationMode, creating nothing.

Evidence of the accepted values is in the Evidence field; the currently supported runtime must continue to be accepted.

Traces: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode.

## Evidence
Expected: `('InvalidArgumentException', 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment')`, nothing created.
Actual: 200 with `{'ApplicationDetail': {'ApplicationARN': 'arn:aws:kinesisanalytics:us-west-2:000000000000:application/<name>', 'RuntimeEnvironment': 'ZEPPELIN-FLINK-1_0', 'ApplicationStatus': 'READY', 'ApplicationVersionId': 1, ...}}`

## Steps to Reproduce
1. Create a service execution role.
2. CreateApplication(ApplicationName=..., RuntimeEnvironment='ZEPPELIN-FLINK-1_0', ServiceExecutionRole=<role>) — once without ApplicationMode and once with ApplicationMode='INTERACTIVE'.
Observed: both succeed with ApplicationStatus READY. Expected: both refused with InvalidArgumentException and the deprecation message; no application created.
