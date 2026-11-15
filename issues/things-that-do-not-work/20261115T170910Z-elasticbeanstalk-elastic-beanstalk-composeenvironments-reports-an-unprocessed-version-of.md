Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.compose-environments-mismatched-application-must-fail
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.compose-environments-mismatched-application-must-fail|run#20|AssertionError|assert code2 == "ConfigurationValidationException", f"unexpected error code: {code2}"
Issue-Commit: 2a513259435d6dcec5aa5a741411e3a8a3522aaf

# Elastic Beanstalk: ComposeEnvironments reports an UNPROCESSED version of its own application as MissingParameter instead of ConfigurationValidationException

> _Note: this issue was identified against the code base at commit `2a513259435d6dcec5aa5a741411e3a8a3522aaf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ComposeEnvironments

## Description
ONE net-new root cause in the elasticbeanstalk emulator, grounded in a failing behavioral check of this suite. The adjacent OPEN issue 20261107T192913Z-elasticbeanstalk-elastic-beanstalk-composeenvironments-refuses-manifest-bearing-bundles-with.md describes neighbouring ground (ComposeEnvironments refusing the recorded SUCCESS path), but it does not carry this check — the suite did not mark this check OWNED, and the divergence here is in how ComposeEnvironments CLASSIFIES a version that does belong to the named application but is not yet processed.

Cause 1 of 1: ComposeEnvironments does not distinguish 'this version label does not belong to the named application' from 'this version belongs to the application but has not been successfully processed'. Failing check: elasticbeanstalk.compose-environments-mismatched-application-must-fail.
AWS: Smithy (com.amazonaws.elasticbeanstalk#ComposeEnvironments): input ApplicationName (@length 1..100), GroupName (@length 1..19), VersionLabels; output EnvironmentDescriptionsMessage{Environments, NextToken}; modeled errors are only InsufficientPrivilegesException (403) and TooManyEnvironmentsException (400) — neither MissingParameter nor ConfigurationValidationException is modeled, so both refusals are recorded behavior only. Recorded behavior: ComposeEnvironments first scopes VersionLabels to the versions of ApplicationName; when that leaves none, the call is refused with error code MissingParameter and the message "No valid application version found." (entirely fixed text, no variable part). When a supplied label DOES name a version of ApplicationName but that version is not in a processed state, the call is refused instead with error code ConfigurationValidationException and the message "ApplicationVersion <VersionLabel> has not been successfully processed. Current status: <Status>" — the fixed text is "ApplicationVersion ", " has not been successfully processed. Current status: "; the variable parts are the VersionLabel taken from the request and the ApplicationVersionDescription Status held in the emulator's state (UNPROCESSED for a version created by CreateApplicationVersion with Process unset/false). In both cases no environment is created under the application.
Emulator: the cross-application case is correct (MissingParameter / "No valid application version found."), but the own-application UNPROCESSED case is answered with the SAME refusal: error code MissingParameter instead of ConfigurationValidationException, so the status-specific message is never produced — the check fails with "AssertionError: unexpected error code: MissingParameter" on the second ComposeEnvironments call. Hypothesis: the handler's version lookup keeps only versions it considers usable (processed) and then reports the empty result as MissingParameter, collapsing the two distinct refusals into one.
Why: a client cannot tell a wrong/foreign version label (a request-construction bug it must fix by naming the right label) from a version of its own application that merely needs to finish processing (a transient state it should wait on and retry). Retry-on-ConfigurationValidationException logic never fires, and callers see a 'missing parameter' for a parameter they supplied correctly; the per-version Status that AWS names in the message is lost entirely.
Fix: the ComposeEnvironments handler's version resolution and refusal classification — resolve each requested label against the named application's versions first, refuse with MissingParameter only when NO requested label belongs to the application, and otherwise refuse a resolved-but-unprocessed version with ConfigurationValidationException carrying that version's label and its stored Status (services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice/src/ops/compose_environments.rs and the application-version state module it reads).
Traces: elasticbeanstalk.compose-environments-mismatched-application-must-fail.

## Evidence
Check elasticbeanstalk.compose-environments-mismatched-application-must-fail, second ComposeEnvironments call (check line 56):
  assert code2 == "ConfigurationValidationException", f"unexpected error code: {code2}"
  AssertionError: unexpected error code: MissingParameter

Expected (recorded AWS): HTTP 400, Code=ConfigurationValidationException, Message="ApplicationVersion <version_a> has not been successfully processed. Current status: UNPROCESSED" (fixed text as quoted; <version_a> is the VersionLabel from the request and UNPROCESSED is the version's stored Status).
Actual (emulator): HTTP 400, Code=MissingParameter, Message="No valid application version found." — the same refusal the emulator (correctly) returns for the preceding call, which referenced another application's version label and which the check passed.
Effect assertions are unaffected: DescribeEnvironments(ApplicationName=app_a) stays empty after both refusals.

## Steps to Reproduce
1. Boot the emulator and point an AWS client at it (region us-west-2).
2. eb.create_application(ApplicationName=app_a); eb.create_application(ApplicationName=app_b).
3. eb.create_application_version(ApplicationName=app_b, VersionLabel=version_b) and eb.create_application_version(ApplicationName=app_a, VersionLabel=version_a), both WITHOUT SourceBundle/SourceBuildInformation and without Process=True, so each version sits at Status='UNPROCESSED'.
4. eb.compose_environments(ApplicationName=app_a, VersionLabels=[version_b]) -> correctly refused with MissingParameter / "No valid application version found.".
5. eb.compose_environments(ApplicationName=app_a, VersionLabels=[version_a]) -> emulator refuses with MissingParameter / "No valid application version found."; AWS refuses with ConfigurationValidationException / "ApplicationVersion <version_a> has not been successfully processed. Current status: UNPROCESSED".
6. Re-replay the check listed in trace_ids.
