Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode
Issue-Key: glue__iam__kinesisanalyticsv2|CreateApplication|InvalidArgumentException
Issue-Commit: bdb1a6ef7dc3116b91a3c1b139324c19f5e5e095

# CreateApplication refuses the deprecated ZEPPELIN-FLINK-1_0 runtime with the ApplicationMode-mismatch message instead of AWS's deprecation refusal

> _Note: this issue was identified against the code base at commit `bdb1a6ef7dc3116b91a3c1b139324c19f5e5e095`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateApplication

## Description
Cause 1 of 1: CreateApplication does not have a deprecated-RuntimeEnvironment check, so a request naming a withdrawn Studio runtime generation falls through to the ApplicationMode/runtime-compatibility validation and is refused with that validation's message instead of AWS's deprecation message. Failing check: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode.

Related but NOT covering this: open issue 20261117T135333Z-glue-iam-kinesisanalyticsv2-createapplication-accepts-the-deprecated-zeppelin-flink-1-0.md records the earlier state of this operation, where the same request SUCCEEDED and created a READY application. The emulator now refuses the request, but with the wrong error message, so the replay key of this failure differs from that issue's. The deprecation rule is still unenforced as a rule of its own; it is only being shadowed by an unrelated validation.

AWS: Smithy (kinesisanalyticsv2, CreateApplication) marks RuntimeEnvironment required and declares InvalidArgumentException (client, HTTP 400) among the operation's errors; it does not model which enum members are deprecated, so the deprecation rule is recorded behavior only. The recording shows RuntimeEnvironment 'ZEPPELIN-FLINK-1_0' refused with HTTP 400, code 'InvalidArgumentException' and message 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment'. In that message everything is fixed text except the runtime identifier after 'Runtime ', which is the RuntimeEnvironment value taken verbatim from the request. The refusal holds whether ApplicationMode is omitted or set to INTERACTIVE, and in both cases nothing is created (DescribeApplication for the name afterwards answers ResourceNotFoundException). The currently supported ZEPPELIN-FLINK-3_0 runtime with ApplicationMode INTERACTIVE must still be accepted and settle READY.

Emulator: the request is refused with the right code but the wrong message. For the mode-less create it answers InvalidArgumentException with message "Invalid configuration provided. ApplicationMode 'null' is not applicable to runtime environment : ZEPPELIN-FLINK-1_0" — i.e. the runtime/ApplicationMode-compatibility validation fires first and reports a missing ApplicationMode, never mentioning that the runtime generation itself is withdrawn. The check asserts on the (code, message) pair and fails at expect_error.

Why: a client cannot tell from the emulator's answer why its request was rejected. It reads 'ApplicationMode null is not applicable', so the obvious remedy is to supply ApplicationMode=INTERACTIVE and retry — on real AWS that retry is refused with exactly the same deprecation message, because the runtime generation, not the mode, is the problem. Error-handling code that branches on the message (or surfaces it to a user) is steered toward a fix that cannot work, and the withdrawal of a runtime generation is only discovered on a real account.

Fix: CreateApplication's RuntimeEnvironment validation in the kinesisanalyticsv2 handler — classify withdrawn runtime generations and refuse them with InvalidArgumentException and AWS's deprecation message, creating nothing. This check must run BEFORE the runtime/ApplicationMode-compatibility validation, so that a deprecated runtime draws the deprecation refusal regardless of whether ApplicationMode is absent or INTERACTIVE; supported runtimes must keep flowing through the existing mode-compatibility rules unchanged.

Traces: glue__iam__kinesisanalyticsv2.deprecated-zeppelin-runtime-rejected-regardless-of-application-mode (both the mode-less and the ApplicationMode=INTERACTIVE create, plus the follow-up DescribeApplication that must answer ResourceNotFoundException).

## Evidence
Expected (HTTP 400): ('InvalidArgumentException', 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment') — fixed text apart from the runtime identifier, which is the request's RuntimeEnvironment value.
Actual: ('InvalidArgumentException', "Invalid configuration provided. ApplicationMode 'null' is not applicable to runtime environment : ZEPPELIN-FLINK-1_0").
Assertion: AssertionError: expected ('InvalidArgumentException', 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment'), got ('InvalidArgumentException', "Invalid configuration provided. ApplicationMode 'null' is not applicable to runtime environment : ZEPPELIN-FLINK-1_0") — raised at check_glue__iam__kinesisanalyticsv2_deprecated_zeppelin_runtime_rejected_regardless_of_application_mode.py line 52 (expect_error, line 131).
Smithy: kinesisanalyticsv2 CreateApplication input RuntimeEnvironment required (enum), errors include InvalidArgumentException (client, httpError 400). The deprecation of a runtime generation is not modeled — recorded behavior only.

## Steps to Reproduce
1. Create an IAM role trusted by kinesisanalytics.amazonaws.com and a Glue database (positive control).
2. CreateApplication(ApplicationName=<a>, RuntimeEnvironment='ZEPPELIN-FLINK-3_0', ApplicationMode='INTERACTIVE', ServiceExecutionRole=<role>, ApplicationConfiguration.ZeppelinApplicationConfiguration.CatalogConfiguration.GlueDataCatalogConfiguration.DatabaseARN=<db arn>) and wait for READY — this still passes.
3. CreateApplication(ApplicationName=<b>, RuntimeEnvironment='ZEPPELIN-FLINK-1_0', ServiceExecutionRole=<role>) with NO ApplicationMode.
   Observed: InvalidArgumentException "Invalid configuration provided. ApplicationMode 'null' is not applicable to runtime environment : ZEPPELIN-FLINK-1_0".
   Expected: InvalidArgumentException 'Runtime ZEPPELIN-FLINK-1_0 is deprecated. Please choose a valid version from: https://docs.aws.amazon.com/managed-flink/latest/apiv2/API_CreateApplication.html#APIReference-CreateApplication-request-RuntimeEnvironment'.
4. Repeat step 3 with ApplicationMode='INTERACTIVE' — the same deprecation refusal is expected.
5. DescribeApplication(ApplicationName=<b>) after each refusal must answer ResourceNotFoundException "Application '<b>' does not exist." (nothing created).
