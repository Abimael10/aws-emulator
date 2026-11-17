Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy__ec2__ecs__elbv2__iam.create-ecs-deployment-group-rejects-service-without-codedeploy-controller
Issue-Key: codedeploy__ec2__ecs__elbv2__iam.create-ecs-deployment-group-rejects-service-without-codedeploy-controller|_error_pair#14|AssertionError|raise AssertionError(f"expected the call to be refused, but it SUCCEEDED and returned {result!r}")
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# CodeDeploy CreateDeploymentGroup does not resolve the ECS service's deploymentController, so a BLUE_GREEN group over an ECS-controlled service is created instead of refused InvalidECSServiceException

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeploymentGroup

## Description
Cause 1 of 1: CreateDeploymentGroup accepts an `ecsServices` entry naming an Amazon ECS service that was not created with the CODE_DEPLOY deployment controller.
AWS: Smithy CreateDeploymentGroup declares `ecsServices` (ECSServiceList of {serviceName, clusterName}) and models InvalidECSServiceException — "The Amazon ECS service identifier is not valid." — as a client error, HTTP 400. CodeDeploy resolves the named service in the named cluster at create time and requires its `deploymentController.type` to be CODE_DEPLOY; a service created with the default ECS (rolling-update) controller is refused with InvalidECSServiceException and the message `Deployment group's ECS service must be configured for a CODE_DEPLOY deployment controller.` (entirely fixed text for this error class), and no deployment group is created — ListDeploymentGroups for that application still returns []. The identical request naming a service created with deploymentController type CODE_DEPLOY succeeds and GetDeploymentGroup echoes `ecsServices` as [{serviceName, clusterName}].
Emulator: the call returns HTTP 200 and creates the group, returning the deployment group name/id (replay: "expected the call to be refused, but it SUCCEEDED and returned 'sim2real-xsvc-dg-<id>'"). The handler stores `ecsServices` as opaque input without resolving the service against the ECS service state, so the controller precondition is never evaluated.
Why: the single most common ECS blue/green misconfiguration — pointing CodeDeploy at a rolling-update service — is undetectable. The client believes it has a working blue/green group; every later deployment against it either behaves unlike AWS or fails far from the request that caused it, instead of failing at create time with a modeled error the client already handles. This is the same class as the already-open issue 20261022T171541Z-codedeploy-createdeploymentgroup-and-createdeployment-accept-input-real-aws.md (CreateDeploymentGroup storing input as an opaque document), but that issue's five causes do not include the ECS deploymentController precondition, so this check is not covered by it.
Fix: CreateDeploymentGroup's cross-service validation in the operation's handler — resolve each `ecsServices` entry against the ECS service state and refuse unless its deploymentController type is CODE_DEPLOY, before any group is persisted.
Traces: codedeploy__ec2__ecs__elbv2__iam.create-ecs-deployment-group-rejects-service-without-codedeploy-controller.

## Evidence
Replayed behavioral suite, service heading `### codedeploy__ec2__ecs__elbv2__iam` (0/1 passed).

Check codedeploy__ec2__ecs__elbv2__iam.create-ecs-deployment-group-rejects-service-without-codedeploy-controller [mismatch]:
  Setup: VPC/subnets/SG, ECS cluster, idle task definition, two ELBv2 target groups + listener, a CodeDeploy service role, and an ECS service created with deploymentController type "ECS" (not CODE_DEPLOY).
  Request: CreateDeploymentGroup(applicationName=<ECS app>, deploymentGroupName=<new>, serviceRoleArn=<role>, deploymentStyle={BLUE_GREEN, WITH_TRAFFIC_CONTROL}, ecsServices=[{serviceName: <ECS-controller service>, clusterName: <cluster>}], loadBalancerInfo with both target groups and the listener).
  Expected (AWS): HTTP 400, Error.Code = "InvalidECSServiceException", Error.Message = "Deployment group's ECS service must be configured for a CODE_DEPLOY deployment controller." (fixed text, no request-derived values); ListDeploymentGroups(applicationName=<app>) == [].
  Actual (emulator): HTTP 200, group created — `AssertionError: expected the call to be refused, but it SUCCEEDED and returned 'sim2real-xsvc-dg-04b9e0238ce6'` (the generated group name is environment-specific; the requirement is the refusal, its code and its message).
  The check's positive control — the identical request naming an ECS service created with deploymentController CODE_DEPLOY, then GetDeploymentGroup echoing ecsServices — was not reached.

Smithy: CreateDeploymentGroup errors include InvalidECSServiceException (client, HTTP 400); input member `ecsServices` targets ECSServiceList.

## Steps to Reproduce
1. Boot the emulator. Create a VPC with two subnets and a security group, an ECS cluster, and register an idle task definition.
2. Create two ELBv2 target groups in that VPC and a load balancer with a listener forwarding to the first.
3. Create an IAM role assumable by codedeploy.amazonaws.com.
4. Create an ECS service in the cluster with deploymentController={"type": "ECS"} attached to the first target group.
5. CreateApplication(computePlatform="ECS").
6. CreateDeploymentGroup(applicationName=<that app>, deploymentGroupName=<new>, serviceRoleArn=<role>, deploymentStyle={"deploymentType":"BLUE_GREEN","deploymentOption":"WITH_TRAFFIC_CONTROL"}, ecsServices=[{"serviceName": <step 4 service>, "clusterName": <cluster>}], loadBalancerInfo=<both target groups + listener>) -> expect HTTP 400 InvalidECSServiceException with message "Deployment group's ECS service must be configured for a CODE_DEPLOY deployment controller."; observe HTTP 200 and a created group.
7. ListDeploymentGroups(applicationName=<that app>) -> expect [].
8. Positive control: create a second ECS service with deploymentController={"type":"CODE_DEPLOY"} and repeat step 6 against a fresh application -> expect success, and GetDeploymentGroup to echo ecsServices=[{serviceName, clusterName}].
