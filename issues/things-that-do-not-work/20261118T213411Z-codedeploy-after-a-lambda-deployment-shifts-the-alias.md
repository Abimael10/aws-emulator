Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback|run#26|AssertionError|assert str(live_at_shift) == v3, (
Issue-Commit: 9cd823483c6bf9bc21049a15587772110eae227a

# codedeploy: after a Lambda deployment shifts the alias, an alias-qualified Invoke still executes the previous version's code — GetAlias and the version invocations reach disagree (1 cause)

> _Note: this issue was identified against the code base at commit `9cd823483c6bf9bc21049a15587772110eae227a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause in the codedeploy surface, grounded in a failing behavioral check the replay report did NOT mark as already filed or owned.

Cause 1 of 1: once a CodeDeploy Lambda deployment has moved an alias to the AppSpec TargetVersion, the control plane and the data plane disagree — lambda GetAlias reports FunctionVersion = TargetVersion, but a synchronous Invoke of the SAME alias-qualified name (FunctionName="<fn>:<alias>") runs the code of the version the alias pointed at BEFORE the shift (one published version behind). Failing check ids: codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice, com.amazonaws.lambda) — Invoke(FunctionName: NamespacedFunctionName @httpLabel, whose pattern admits the ...(function:)?<name>(:(\$LATEST(\.PUBLISHED)?|[a-zA-Z0-9-_]+))? alias/qualifier suffix; Qualifier: NumericLatestPublishedOrAliasQualifier) -> {StatusCode @httpResponseCode, Payload @httpPayload, ExecutedVersion @httpHeader X-Amz-Executed-Version targeting Version with pattern ^(\$LATEST|[0-9]+)$}; GetAlias -> AliasConfiguration{FunctionVersion: Version}; PublishVersion -> FunctionConfiguration{Version: Version}. The model therefore states that an alias is a qualifier Invoke must resolve, and that the response names the version actually executed. The rest is recorded behavior only: in the recording an alias names exactly one immutable published version (no AdditionalVersionWeights here, the deployment config is CodeDeployDefault.LambdaAllAtOnce), each PublishVersion freezes the code that was in $LATEST at the moment it was published, and so an Invoke of "<fn>:<alias>" executes that frozen code — the payload a handler published as version N returns is the payload the alias serves while GetAlias reports N. AllAtOnce performs the whole shift before the AfterAllowTraffic lifecycle event runs, so from the instant GetAlias reports the TargetVersion, live invokes of the alias reach it. No HTTP status or error code is involved: the invoke is a 200 with no FunctionError; the executed code is simply the wrong version's.

Emulator: the shift itself now lands (GetAlias reports the TargetVersion, and the check's earlier assertion on that passed), but the very next Invoke of "<fn>:<alias>" returns the body produced by the version published immediately before the target — the replay reports: "a live invoke of 'live' returned version 2 while the alias reads TargetVersion '3'; GetAlias and the version invocations actually reach must agree". The version numbers 2/3 are this run's published versions and are not part of the requirement; the requirement is that the code executed for an alias-qualified Invoke is the code of the version GetAlias reports for that alias. Hypothesis (the recording cannot distinguish these two, and neither is stated by the model): either the Invoke path resolves the alias from a stale/cached copy of the alias record that the shift did not update (the value observed is the alias' value before the shift), or PublishVersion snapshots the function code off by one update, so version N carries the code that was current when version N-1 was published; both produce exactly the observed one-version-behind result.

Why: a client cannot trust a completed Lambda rollout. CodeDeploy reports the shift, GetAlias confirms the new version, and every production caller of the alias keeps executing the old code — a smoke test or an AfterAllowTraffic validation hook that invokes the alias validates the previous deployment, a canary measures the wrong code, and a rollback appears to restore a version that was never actually serving. Downstream, the whole AfterAllowTraffic contract this check exists to pin (hook reports Failed -> deployment Failed/HOOK_EXECUTION_FAILURE -> DEPLOYMENT_FAILURE auto-rollback spawning a codeDeployRollback deployment with rollbackInfo.rollbackTriggeringDeploymentId and the "Deployment <id> terminated. Automatic rollback is triggered with a DeploymentId <id>." rollbackMessage) is never reached, because the check aborts on this data-plane disagreement first.

Fix: the lambda service's Invoke data plane — the qualifier resolution step that maps an alias-qualified FunctionName (and Qualifier) to the version whose code is executed, together with the published-version code snapshot taken by PublishVersion that it reads. Whichever of the two holds, the invariant to restore is that the code run for "<fn>:<alias>" is the code of the version GetAlias reports (and that X-Amz-Executed-Version names it), re-resolved on every invocation rather than cached across an alias update such as the one a CodeDeploy traffic shift performs. No change is needed in the codedeploy deployment state machine for this cause — it already performs the shift the alias read reflects.

Related open issues that do NOT cover this: 20261107T224322Z-codedeploy-lambda-deployments-reach-succeeded-without-shifting-the.md and 20261107T212447Z-cloudwatch-codedeploy-lambda-codedeploy-lambda-deployment-never-shifts-the-alias-to.md describe the alias NOT being shifted at all (control plane); here the shift is visible in GetAlias and only the invoke path disagrees. 20261111T154845Z-codedeploy-stopdeployment-s-deploymentalreadycompletedexception-message-is-re-authored.md Cause 2 and 20261112T102720Z-lambda-invoking-a-weighted-alias-never-completes-the.md concern alias-invoke THROUGHPUT and weighted routing, not which version a single unweighted alias invoke reaches. 20261108T101433Z-lambda-2-net-new-causes-an-invoke-inside.md concerns $LATEST code visibility during an update window, not an alias qualifier. None of them carries this check's replay key, and the suite did not mark this failure as owned.

Traces: codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback — fails at check line 116 (assert str(live_at_shift) == v3) immediately after the alias read at line 108 confirmed FunctionVersion == TargetVersion.

## Evidence
Suite: simparity-behavioral-aws-v1. Total 125/155 passed (80.6%); codedeploy 120/144 (83%), 24 failed — 23 of them marked ALREADY FILED/OWNED, this one net-new.

[mismatch] codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback
  check_codedeploy_after_allow_traffic_hook_failure_triggers_deployment_failure_rollback.py line 116:
    assert str(live_at_shift) == v3
  AssertionError: a live invoke of 'live' returned version 2 while the alias reads TargetVersion '3'; GetAlias and the version invocations actually reach must agree

Setup performed by the check (all of it passed): a python3.12 function is created whose handler returns {'v': 1}; UpdateFunctionCode then PublishVersion is repeated so that version N's handler returns {'v': N} for N = 1, 2, 3; an alias 'live' is created on version 1. A Lambda-platform application and deployment group (CodeDeployDefault.LambdaAllAtOnce, autoRollbackConfiguration {enabled: true, events: ['DEPLOYMENT_FAILURE']}) are created. A control deployment (AppSpec CurrentVersion 1 -> TargetVersion 2, AfterAllowTraffic hook reporting Succeeded) reached Succeeded and left GetAlias at version 2 with no AdditionalVersionWeights. A second deployment (CurrentVersion 2 -> TargetVersion 3, hook reporting Failed) was then created.

Expected (AWS): once GetAlias(FunctionName=<fn>, Name='live').FunctionVersion == '3', lambda.Invoke(FunctionName='<fn>:live', Payload=b'{}') returns StatusCode 200, no FunctionError, X-Amz-Executed-Version '3' and the payload {'v': 3} — i.e. the code frozen by PublishVersion for version 3.
Observed (emulator): GetAlias reports FunctionVersion '3' (that assertion passed), but the immediately following Invoke of '<fn>:live' returns 200 with no FunctionError and payload {'v': 2} — the code of the version the alias held before the shift. The literal 2/3 are this run's published version numbers; the requirement is agreement between the alias' FunctionVersion and the code the alias-qualified invoke executes.

Because the check aborts here, everything after it in the same check is unverified: deployment status Failed with errorInformation.code HOOK_EXECUTION_FAILURE, the AfterAllowTraffic lifecycle event reading Failed, rollbackInfo.rollbackDeploymentId, the rollbackMessage template "Deployment <breachDeploymentId> terminated. Automatic rollback is triggered with a DeploymentId <rollbackDeploymentId>.", the rollback deployment's creator 'codeDeployRollback' and its rollbackInfo.rollbackTriggeringDeploymentId, and the restoration of the alias (and of live invokes) to the pre-shift version.

## Steps to Reproduce
1. lambda: CreateFunction (runtime python3.12, handler returning {'v': 1}); wait for State=Active.
2. lambda: UpdateFunctionCode with a handler returning {'v': 2}, wait Active, PublishVersion -> version V2. Repeat with {'v': 3} -> version V3. (V1 was published from the original code.)
3. lambda: CreateAlias(Name='live', FunctionVersion=V1).
4. codedeploy: CreateApplication(computePlatform='Lambda'); CreateDeploymentGroup(deploymentConfigName='CodeDeployDefault.LambdaAllAtOnce', autoRollbackConfiguration={enabled: true, events: ['DEPLOYMENT_FAILURE']}).
5. codedeploy: CreateDeployment with an inline AppSpecContent naming the function, alias 'live', CurrentVersion V1, TargetVersion V2; poll GetDeployment to Succeeded.
6. codedeploy: CreateDeployment with AppSpec CurrentVersion V2, TargetVersion V3; poll lambda GetAlias(FunctionName=<fn>, Name='live') until FunctionVersion == V3.
7. lambda: Invoke(FunctionName='<fn>:live', Payload=b'{}') and read the payload. AWS: {'v': 3} with X-Amz-Executed-Version V3. Emulator: {'v': 2} — the pre-shift version's code — with StatusCode 200 and no FunctionError.
8. Equivalently, invoke the versions directly: Invoke(FunctionName='<fn>:V3') and Invoke(FunctionName='<fn>:V2') and compare their payloads with the code each version was published from, to tell stale alias resolution apart from an off-by-one version code snapshot.
9. Replay codedeploy.after-allow-traffic-hook-failure-triggers-deployment-failure-rollback.
