Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.create-deployment-group-ecs-without-load-balancer-info-rejected
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.create-deployment-group-ecs-without-load-balancer-info-rejected|run#6|AssertionError|assert code == "InvalidLoadBalancerInfoException", (
Issue-Commit: 69b5bb01d5a9bd6e71aad0c8b3d3a2a49866ff19

# CodeDeploy CreateDeploymentGroup has no loadBalancerInfo precondition for ECS compute platform: an ECS group with no loadBalancerInfo is refused InvalidECSServiceException instead of InvalidLoadBalancerInfoException

> _Note: this issue was identified against the code base at commit `69b5bb01d5a9bd6e71aad0c8b3d3a2a49866ff19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeploymentGroup

## Description
One cause.

Cause 1 of 1: CreateDeploymentGroup never enforces the "an ECS deployment group must carry loadBalancerInfo" precondition, so a request that omits it is instead refused by the unrelated ecsServices-resolution gate, with the wrong error code and the wrong message.

AWS: Smithy CreateDeploymentGroup declares `ecsServices` (ECSServiceList), `loadBalancerInfo` (LoadBalancerInfo) and `deploymentStyle` (DeploymentStyle), and models BOTH InvalidLoadBalancerInfoException (HTTP 400, "An invalid load balancer name, or no load balancer name, was specified") and InvalidECSServiceException (HTTP 400, "The Amazon ECS service identifier is not valid"). For an application whose computePlatform is ECS, a CreateDeploymentGroup with deploymentStyle BLUE_GREEN/WITH_TRAFFIC_CONTROL and `ecsServices` set but `loadBalancerInfo` omitted is refused HTTP 400 InvalidLoadBalancerInfoException with the message "For ECS deployment group, loadBalancerInfo must be specified" (recorded behavior; the message is entirely fixed text — it carries no request- or state-derived value), and no deployment group is created (ListDeploymentGroups does not list the name). This precondition is evaluated BEFORE the ecsServices cluster/service pair is resolved: in the recording the named cluster and service do not exist in the account at all, and AWS still answers InvalidLoadBalancerInfoException, not an ECS-service refusal. The same omission on a Server-platform IN_PLACE/WITHOUT_TRAFFIC_CONTROL group is legitimate and succeeds — the rule is about the ECS/traffic-control combination, not about loadBalancerInfo being required in general.

Emulator: the request is refused, but with error code 'InvalidECSServiceException' (and that gate's relayed "could not load the ECS service" message) rather than 'InvalidLoadBalancerInfoException' / "For ECS deployment group, loadBalancerInfo must be specified". services/service-com-amazonaws-codedeploy-codedeploy-20141006/src/ops/create_deployment_group.rs resolves `ecsServices` against ECS (lines ~411-428, verdict applied at ~484-492) and stores `input.load_balancer_info` unexamined (line ~615); there is no computePlatform-keyed loadBalancerInfo check anywhere in the handler, so for an ECS group the only refusal that can fire is the ECS-pair one, and a request that omits loadBalancerInfo while naming an EXISTING ECS service would still be accepted with HTTP 200.

Why: a client that branches on the modeled error code (and the operator reading the message) is told the ECS cluster/service identifier is wrong when the actual defect in its request is the missing load-balancer configuration, so it "fixes" the wrong field; and an ECS blue/green group that names a real service but no load balancer — one that can never shift traffic — is still created with HTTP 200, surfacing the failure only at deploy time or never.

Fix: CreateDeploymentGroup's validation in services/service-com-amazonaws-codedeploy-codedeploy-20141006/src/ops/create_deployment_group.rs — add the loadBalancerInfo precondition keyed on the resolved application's computePlatform, and order it ahead of the ecsServices resolution gate so it is the refusal served when both would apply.

Related already-open issue: 20261022T171541Z-codedeploy-createdeploymentgroup-and-createdeployment-accept-input-real-aws.md describes this rule as its Cause 3, but it describes the emulator as answering HTTP 200; the emulator now REFUSES this request with a different modeled error, which is the divergence replayed here, so neither its description nor its replay key covers this failure. The ECS-pair gate that now intercepts the call was added for the fixed issue 20261104T035630Z-codedeploy-ecs-elbv2-codedeploy-createdeploymentgroup-does-not-resolve-ecsservices-against-ecs.md; that gate itself is correct — it is only firing in place of a precondition that is still missing.

Traces: codedeploy.create-deployment-group-ecs-without-load-balancer-info-rejected.

## Evidence
Replayed behavioral suite, service codedeploy, check codedeploy.create-deployment-group-ecs-without-load-balancer-info-rejected (mismatch):

  File "check_codedeploy_create_deployment_group_ecs_without_load_balancer_info_rejected.py", line 32, in run
    assert code == "InvalidLoadBalancerInfoException", (
  AssertionError: expected an ECS deployment group with no loadBalancerInfo to be rejected InvalidLoadBalancerInfoException, got 'InvalidECSServiceException'

Expected (AWS): HTTP 400, code InvalidLoadBalancerInfoException, message "For ECS deployment group, loadBalancerInfo must be specified" (all fixed text), and ListDeploymentGroups(applicationName=<ecs app>) must not list the refused group name.
Actual (emulator): HTTP 400 with code InvalidECSServiceException from the ecsServices resolution gate.

The check's positive control — the same loadBalancerInfo omission on a Server-platform IN_PLACE/WITHOUT_TRAFFIC_CONTROL group — is expected to succeed, so the requirement is the ECS-platform refusal specifically, not a blanket rejection.

Application names, deployment group names, cluster/service names and ids in the run are generated per run and are not part of the requirement; the requirement is the status, the error code and the fixed message text.

Smithy (services/service-com-amazonaws-codedeploy-codedeploy-20141006/smithy-model/com-amazonaws-codedeploy-codedeploy-20141006.json): CreateDeploymentGroup errors include InvalidLoadBalancerInfoException (HTTP 400) and InvalidECSServiceException (HTTP 400); input members ecsServices, loadBalancerInfo, deploymentStyle.

## Steps to Reproduce
1. Boot the emulator. Create an IAM role assumable by codedeploy.amazonaws.com and get its ARN.
2. CreateApplication(applicationName=<app>, computePlatform="ECS").
3. CreateDeploymentGroup(applicationName=<app>, deploymentGroupName=<dg>, serviceRoleArn=<role arn>, deploymentStyle={deploymentType:"BLUE_GREEN", deploymentOption:"WITH_TRAFFIC_CONTROL"}, ecsServices=[{serviceName:<any>, clusterName:<any>}]) with loadBalancerInfo OMITTED.
   Expect: HTTP 400 InvalidLoadBalancerInfoException, message "For ECS deployment group, loadBalancerInfo must be specified" — regardless of whether the named cluster/service exist.
   Observe: HTTP 400 InvalidECSServiceException.
4. ListDeploymentGroups(applicationName=<app>) -> <dg> must not be listed.
5. Repeat step 3 against an ECS cluster/service that DOES exist in the emulator's ECS state, still omitting loadBalancerInfo: expect the same InvalidLoadBalancerInfoException refusal; observe HTTP 200 with a deploymentGroupId.
6. Positive control: CreateApplication(computePlatform="Server") then CreateDeploymentGroup(deploymentStyle={IN_PLACE, WITHOUT_TRAFFIC_CONTROL}, ec2TagFilters=[...]) with no loadBalancerInfo -> must still succeed and GetDeploymentGroup must return it.
