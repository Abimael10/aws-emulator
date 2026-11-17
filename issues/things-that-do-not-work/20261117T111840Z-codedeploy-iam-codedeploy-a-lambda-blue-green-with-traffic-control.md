Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy__iam.deployment-group-lambda-default-config-get-batchget-agree
Issue-Key: codedeploy__iam.deployment-group-lambda-default-config-get-batchget-agree|run#15|AssertionError|assert get_info["deploymentConfigName"] == "CodeDeployDefault.LambdaCanary10Percent5Minutes", (
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# CodeDeploy: a Lambda BLUE_GREEN/WITH_TRAFFIC_CONTROL deployment group created with no deploymentConfigName defaults to LambdaAllAtOnce instead of LambdaCanary10Percent5Minutes

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeploymentGroup

## Description
Cause 1 of 1: the default `deploymentConfigName` the emulator assigns to a Lambda deployment group created with deploymentStyle BLUE_GREEN/WITH_TRAFFIC_CONTROL is the wrong predefined configuration.
AWS: recorded behavior only — the Smithy model declares `deploymentConfigName` optional on CreateDeploymentGroup and its documentation only states the Server-platform fallback ("CodeDeployDefault.OneAtATime is the default deployment configuration"); it states no Lambda-platform default, and GetDeploymentGroupOutput does not model one. The recording shows that for an application with computePlatform Lambda and a group created with deploymentStyle {deploymentType: BLUE_GREEN, deploymentOption: WITH_TRAFFIC_CONTROL} and `deploymentConfigName` omitted, the created group's deploymentConfigName is the traffic-shifting predefined config `CodeDeployDefault.LambdaCanary10Percent5Minutes`, and GetDeploymentGroup and BatchGetDeploymentGroups return the identical record.
Emulator: the same request yields deploymentConfigName `CodeDeployDefault.LambdaAllAtOnce` — the all-at-once config, which performs no traffic shifting at all. Every other asserted member of the record (applicationName, deploymentGroupName, computePlatform=Lambda, serviceRoleArn, deploymentStyle) matched; only the defaulted config name diverges. The handler appears to pick one fixed Lambda default without regard to the requested deploymentStyle.
Why: a client that omits deploymentConfigName and relies on AWS's default gets a group whose deployments shift 100% of traffic immediately instead of a 10% canary held for 5 minutes. The divergence is invisible at create time and only shows up as wrong deployment pacing, wrong lifecycle events and a wrong rollback window — and any client that reads the group back to decide whether to override the config draws the opposite conclusion from the same request. The check also never reaches its GetDeploymentGroup/BatchGetDeploymentGroups agreement and ListDeploymentGroups assertions, so agreement between those three views for a defaulted Lambda group is unverified.
Fix: CreateDeploymentGroup's defaulting of `deploymentConfigName` in the operation's handler/state — select the Lambda-platform default from the requested deploymentStyle (traffic-controlled blue/green -> CodeDeployDefault.LambdaCanary10Percent5Minutes) rather than a single constant, and make GetDeploymentGroup and BatchGetDeploymentGroups serve that same stored value.
Traces: codedeploy__iam.deployment-group-lambda-default-config-get-batchget-agree.

## Evidence
Replayed behavioral suite, service heading `### codedeploy__iam` (1/2 passed).

Check codedeploy__iam.deployment-group-lambda-default-config-get-batchget-agree [mismatch]:
  Setup: CreateApplication(computePlatform="Lambda"); an IAM role assumable by codedeploy.amazonaws.com.
  Request: CreateDeploymentGroup(applicationName=<app>, deploymentGroupName=<dg>, serviceRoleArn=<role>, deploymentStyle={"deploymentType":"BLUE_GREEN","deploymentOption":"WITH_TRAFFIC_CONTROL"}) with deploymentConfigName OMITTED.
  Expected (recorded AWS): GetDeploymentGroup(...)["deploymentGroupInfo"]["deploymentConfigName"] == "CodeDeployDefault.LambdaCanary10Percent5Minutes"; BatchGetDeploymentGroups for the same name returns exactly one record equal to the GetDeploymentGroup record; ListDeploymentGroups returns applicationName=<app> and deploymentGroups=[<dg>].
  Actual (emulator): deploymentConfigName == "CodeDeployDefault.LambdaAllAtOnce" — `AssertionError: a Lambda BLUE_GREEN deployment group created with no deploymentConfigName must default to CodeDeployDefault.LambdaCanary10Percent5Minutes, got 'CodeDeployDefault.LambdaAllAtOnce'`. The preceding assertions on applicationName, deploymentGroupName, computePlatform and serviceRoleArn passed, so the divergence is confined to the defaulted config name.
  Application name, group name and role ARN are generated per run and are not part of the requirement; the requirement is which predefined configuration a defaulted Lambda traffic-controlled group resolves to.

Smithy: CreateDeploymentGroupInput.deploymentConfigName is optional (DeploymentConfigName, @length 1-100) and its documentation names only CodeDeployDefault.OneAtATime as a fallback; no Lambda default is modeled, hence "recorded behavior only".

## Steps to Reproduce
1. Boot the emulator.
2. CreateApplication(applicationName=<app>, computePlatform="Lambda").
3. Create an IAM role whose trust policy allows codedeploy.amazonaws.com to sts:AssumeRole.
4. CreateDeploymentGroup(applicationName=<app>, deploymentGroupName=<dg>, serviceRoleArn=<role arn>, deploymentStyle={"deploymentType":"BLUE_GREEN","deploymentOption":"WITH_TRAFFIC_CONTROL"}) — do NOT pass deploymentConfigName.
5. GetDeploymentGroup(applicationName=<app>, deploymentGroupName=<dg>) -> expect deploymentGroupInfo.deploymentConfigName == "CodeDeployDefault.LambdaCanary10Percent5Minutes"; observe "CodeDeployDefault.LambdaAllAtOnce".
6. BatchGetDeploymentGroups(applicationName=<app>, deploymentGroupNames=[<dg>]) -> expect exactly one deploymentGroupsInfo entry byte-identical to the GetDeploymentGroup record.
7. ListDeploymentGroups(applicationName=<app>) -> expect applicationName=<app> and deploymentGroups=[<dg>].
