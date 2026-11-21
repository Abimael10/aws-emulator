Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy__ecs__elbv2.create-ecs-deployment-group-without-traffic-control-rejected
Issue-Key: codedeploy__ecs__elbv2.create-ecs-deployment-group-without-traffic-control-rejected|_error#9|AssertionError|raise AssertionError(f"expected a ClientError, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# CodeDeploy CreateDeploymentGroup accepts WITHOUT_TRAFFIC_CONTROL for an ECS application instead of refusing InvalidDeploymentStyleException

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeploymentGroup

## Description
Cause 1 of 1: CreateDeploymentGroup does not enforce the ECS compute platform's deploymentStyle precondition.
AWS: Smithy CreateDeploymentGroup declares `deploymentStyle` (DeploymentStyle = {deploymentType, deploymentOption}) and models InvalidDeploymentStyleException — "An invalid deployment style was specified. Valid deployment types include \"IN_PLACE\" and \"BLUE_GREEN.\" Valid deployment options include \"WITH_TRAFFIC_CONTROL\" and \"WITHOUT_TRAFFIC_CONTROL.\"" — as a client error, HTTP 400. For an application whose computePlatform is ECS, the only admissible style is deploymentType BLUE_GREEN with deploymentOption WITH_TRAFFIC_CONTROL; any other combination (here BLUE_GREEN + WITHOUT_TRAFFIC_CONTROL) is refused with InvalidDeploymentStyleException and the message `For ECS deployment, the deployment type must be BLUE_GREEN, and deployment option must be WITH_TRAFFIC_CONTROL.` (entirely fixed text for this error class), and no group is created — ListDeploymentGroups for that application still returns []. The identical request with WITH_TRAFFIC_CONTROL succeeds and GetDeploymentGroup echoes deploymentStyle = {deploymentType: BLUE_GREEN, deploymentOption: WITH_TRAFFIC_CONTROL}.
Emulator: the call returns HTTP 200 and creates the group, returning its name/id (replay: "expected a ClientError, but the call SUCCEEDED and returned 'sim2real-xsvc-dg-<id>'"). `deploymentStyle` is stored as supplied without being checked against the application's computePlatform.
Why: a client gets an ECS deployment group in a state AWS can never produce — an ECS group with no traffic control — so its stored configuration, and the behavior of every deployment launched against it, diverge from AWS with no error at the point the mistake was made. Templates and IaC that depend on this create-time refusal to catch a wrong deploymentOption converge "successfully" on a broken group. Same class as the already-open 20261022T171541Z-codedeploy-createdeploymentgroup-and-createdeployment-accept-input-real-aws.md and 20261116T172520Z-codedeploy-createdeploymentgroup-has-no-loadbalancerinfo-precondition-for-ecs.md, but neither covers the deploymentStyle/computePlatform precondition this check exercises.
Fix: CreateDeploymentGroup's cross-member validation in the operation's handler — key the admissible `deploymentStyle` on the referenced application's computePlatform and refuse an ECS group that is not BLUE_GREEN + WITH_TRAFFIC_CONTROL before persisting anything.
Traces: codedeploy__ecs__elbv2.create-ecs-deployment-group-without-traffic-control-rejected.

## Evidence
Replayed behavioral suite, service heading `### codedeploy__ecs__elbv2` (1/4 passed).

Check codedeploy__ecs__elbv2.create-ecs-deployment-group-without-traffic-control-rejected [mismatch]:
  Setup: VPC/subnets/SG, ALB with blue and green target groups and a listener, ECS cluster, idle task definition and service, a CodeDeploy service role, and an ECS-platform CodeDeploy application.
  Request: CreateDeploymentGroup(applicationName=<ECS app>, deploymentGroupName=<new>, serviceRoleArn=<role>, deploymentStyle={deploymentType: BLUE_GREEN, deploymentOption: WITHOUT_TRAFFIC_CONTROL}, ecsServices=[{serviceName, clusterName}], loadBalancerInfo=<blue/green target groups + listener>).
  Expected (AWS): HTTP 400, Error.Code = "InvalidDeploymentStyleException", Error.Message = "For ECS deployment, the deployment type must be BLUE_GREEN, and deployment option must be WITH_TRAFFIC_CONTROL." (fixed text, no request-derived values); ListDeploymentGroups(applicationName=<app>) == [].
  Actual (emulator): HTTP 200, group created — `AssertionError: expected a ClientError, but the call SUCCEEDED and returned 'sim2real-xsvc-dg-4ecd1315d234'` (generated group name is environment-specific; the requirement is the refusal, its code and its message).
  The check's positive control — the identical request with WITH_TRAFFIC_CONTROL, then GetDeploymentGroup echoing deploymentStyle — was not reached.

Smithy: CreateDeploymentGroup errors include InvalidDeploymentStyleException (client, HTTP 400); the shape's own documentation enumerates the valid deploymentType/deploymentOption values.

## Steps to Reproduce
1. Boot the emulator. Create a VPC with subnets and a security group; create an ALB with two target groups (blue, green) and a listener forwarding to blue.
2. Create an ECS cluster, register an idle task definition, and create an ECS service attached to the blue target group.
3. Create an IAM role assumable by codedeploy.amazonaws.com.
4. CreateApplication(computePlatform="ECS").
5. CreateDeploymentGroup(applicationName=<that app>, deploymentGroupName=<new>, serviceRoleArn=<role>, deploymentStyle={"deploymentType":"BLUE_GREEN","deploymentOption":"WITHOUT_TRAFFIC_CONTROL"}, ecsServices=[{serviceName, clusterName}], loadBalancerInfo=<blue/green target groups + listener>) -> expect HTTP 400 InvalidDeploymentStyleException with message "For ECS deployment, the deployment type must be BLUE_GREEN, and deployment option must be WITH_TRAFFIC_CONTROL."; observe HTTP 200 and a created group.
6. ListDeploymentGroups(applicationName=<that app>) -> expect [].
7. Positive control: repeat step 5 with deploymentOption "WITH_TRAFFIC_CONTROL" -> expect success, and GetDeploymentGroup to report deploymentStyle {"deploymentType":"BLUE_GREEN","deploymentOption":"WITH_TRAFFIC_CONTROL"}.
