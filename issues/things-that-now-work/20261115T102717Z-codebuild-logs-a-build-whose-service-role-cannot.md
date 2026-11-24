Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__logs.codebuild-log-delivery-denied-without-putlogevents-permission
Issue-Key: codebuild__logs.codebuild-log-delivery-denied-without-putlogevents-permission|run#30|AssertionError|assert bad["buildStatus"] == "FAILED", (
Issue-Commit: e4ca4939f46b803ca65adff0207938919af7628e

# codebuild__logs: a build whose service role cannot logs:PutLogEvents to its configured CloudWatch log group still reports buildStatus SUCCEEDED

> _Note: this issue was identified against the code base at commit `e4ca4939f46b803ca65adff0207938919af7628e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBuild

## Description
Cause 1 of 1: the build's CloudWatch Logs delivery is never authorized against the build's service role, so a build that cannot write to the log group its project configures succeeds anyway.
AWS: with ProjectLogsConfig.cloudWatchLogs = {status ENABLED, groupName G, streamName S} and a service role whose only logs grant (logs:CreateLogStream, logs:PutLogEvents) names a DIFFERENT log group, the build does not succeed — CodeBuild cannot deliver its log stream and ends the build buildStatus FAILED, creates no stream under "S/" in G and delivers no event into G; granting logs:CreateLogStream/logs:PutLogEvents on G to that same role makes the identical project's build SUCCEED and create exactly "S/<build-uuid>" in G. Smithy codebuild: Build.buildStatus -> StatusType = SUCCEEDED|FAILED|FAULT|TIMED_OUT|IN_PROGRESS|STOPPED; BuildPhase.contexts -> PhaseContext{statusCode,message} carries the troubleshooting context of the failing phase; ProjectLogsConfig/CloudWatchLogsConfig{status,groupName,streamName}. The log-delivery refusal itself is recorded behavior only — the model states no error for it — so the requirement is the FAILED outcome plus a PhaseContext naming the refused delivery, not an API error code.
Emulator: the build with the mis-scoped role reports buildStatus 'SUCCEEDED' with every phase SUCCEEDED — phases [('SUBMITTED','SUCCEEDED'), ('QUEUED','SUCCEEDED'), ('PROVISIONING','SUCCEEDED'), ('DOWNLOAD_SOURCE','SUCCEEDED'), ('BUILD','SUCCEEDED'), ('UPLOAD_ARTIFACTS','SUCCEEDED'), ('FINALIZING','SUCCEEDED'), ('COMPLETED', None)] — and contexts None on all eight phases. The missing grant has no effect whatsoever on the build's outcome, while the permitted control build of the same shape also succeeds, so the two are indistinguishable.
Why: a client cannot detect a mis-scoped service role from the build result. The build reports success while its logs were never delivered, so CI gates, log-based assertions and permission-boundary tests all pass on a build whose output was silently dropped; the direction of the defect is the dangerous one (false success).
Fix: the build runner's CloudWatch Logs delivery path in the codebuild crate — authorize CreateLogStream/PutLogEvents for the resolved group and stream against the build's serviceRole before/while delivering, and fail the build (buildStatus FAILED, with a PhaseContext naming the refused log delivery) when that delivery is refused.
Related already-open issue, which does NOT cover this: 20261109T143515Z-codebuild-logs-codebuild-reports-the-configured-streamname-verbatim-as-the.md lists this check id among its trace ids, but its cause 2 describes the check never settling ("Check timed out after 180.0s") and asserts that in AWS such a build "still runs but delivers no log events". The check now settles, and the divergence it reports is the opposite requirement: the build must end FAILED. That issue's cause 1 (streamName reported verbatim instead of "<streamName>/<buildId>") is a different defect and is not re-filed here.
Traces: codebuild__logs.codebuild-log-delivery-denied-without-putlogevents-permission.

## Evidence
codebuild__logs.codebuild-log-delivery-denied-without-putlogevents-permission:
"AssertionError: a build that cannot write to its configured log group must FAIL, got 'SUCCEEDED'; phases=[('SUBMITTED', 'SUCCEEDED'), ('QUEUED', 'SUCCEEDED'), ('PROVISIONING', 'SUCCEEDED'), ('DOWNLOAD_SOURCE', 'SUCCEEDED'), ('BUILD', 'SUCCEEDED'), ('UPLOAD_ARTIFACTS', 'SUCCEEDED'), ('FINALIZING', 'SUCCEEDED'), ('COMPLETED', None)]; contexts=[None, None, None, None, None, None, None, None]"
Expected: buildStatus 'FAILED', with a PhaseContext on the failing phase naming the refused CloudWatch Logs delivery, no log stream created under '<streamName>/' in the configured group, and no build event delivered into that group.
Actual: buildStatus 'SUCCEEDED', all phases SUCCEEDED, no PhaseContext on any phase.

## Steps to Reproduce
1. CreateLogGroup G (and note a decoy group name D that is never created).
2. Mint two CodeBuild-assumable IAM roles; put an inline policy on the first allowing logs:CreateLogStream+logs:PutLogEvents on G, and on the second the same actions but scoped to D.
3. CreateProject twice with source NO_SOURCE + inline buildspec that echoes a marker, artifacts NO_ARTIFACTS, environment LINUX_CONTAINER/aws/codebuild/standard:7.0, and logsConfig cloudWatchLogs {status ENABLED, groupName G, streamName <distinct prefix per project>}; the first uses role 1, the second role 2.
4. StartBuild on the permitted project and confirm SUCCEEDED and the marker in G (barrier holds).
5. StartBuild on the project whose role only names D and poll BatchGetBuilds to terminal. Observe buildStatus 'SUCCEEDED' instead of 'FAILED'.
