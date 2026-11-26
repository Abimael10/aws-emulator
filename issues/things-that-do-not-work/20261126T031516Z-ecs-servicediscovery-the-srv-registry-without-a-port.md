Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected
Issue-Key: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# ecs__servicediscovery: the SRV-registry-without-a-port refusal check reaches no verdict — the scenario never converges inside the replay budget

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
1 net-new cause, failing one check of this service group that no open issue's replay keys carry.

Cause 1 of 1: the ECS + Cloud Map SRV scenario never converges inside the replay budget — the check is killed at the harness 60 s ceiling, so neither the refusal contract nor its positive control is ever graded.

AWS: Smithy: ecs CreateService.serviceRegistries with error InvalidParameterException (httpError 400). AWS refuses an SRV-typed Cloud Map registry that carries neither 'port' nor the 'containerName'+'containerPort' pair with InvalidParameterException and the message: "When specifying a value for serviceRegistries that is configured to use a type 'SRV' DNS record, you must also enter a value for either 'port' or the 'containerName' and 'containerPort' combination, but not both. Registry: {registryArn}" — the sentence is fixed text and only {registryArn} is a value from the request. The refused call must leave no service behind and register nothing in Cloud Map; the same call with containerName/containerPort added must succeed, place a task that reaches RUNNING, and register that task's ENI address with AWS_INSTANCE_IPV4 = the task's privateIPv4Address and AWS_INSTANCE_PORT = the container port.

Emulator: 'Check timed out after 60.0s' — the check produced no assertion failure, so it was cut inside one of its waits rather than at the refusal comparison (which raises an AssertionError when the code/message diverge). Hypothesis: it was cut in the positive control's convergence poll for 'control service has 1 RUNNING task' (bounded at 600 s) — consistent with the same group's services never reaching a RUNNING task — or, earlier, in the Cloud Map private-DNS-namespace creation wait. Either way nothing after the setup is observable, so the SRV refusal (code, message and no-side-effect), the task's ENI address and the SRV registration's port are all unverified.

Why: a client gets no answer from the emulator about the one validation that makes SRV service discovery usable — an SRV registry with no port is either silently accepted (leaving a service and a registration AWS would never have created) or refused with different wording, and the registered instance's address/port cannot be checked at all.

Fix: the ECS service scheduler / task lifecycle so a FARGATE service converges to a RUNNING task inside the budget (and the servicediscovery namespace-creation operation settles), then the CreateService serviceRegistries validation for SRV registries and the Cloud Map instance registration that follows. Related open issue, not marked as owning this check: 20261125T221450Z-ecs-the-service-scheduler-never-converges-5-net.md.

Traces: ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected.

## Evidence
Expected (AWS): CreateService with serviceRegistries=[{registryArn: <SRV service>}] and no port is refused 400 InvalidParameterException with the message above (only Registry: <arn> varies), leaves no service in the cluster and no Cloud Map instance; the same call with containerName='app', containerPort=80 succeeds, its task reaches RUNNING, and the Cloud Map instance carries AWS_INSTANCE_IPV4 equal to the task's privateIPv4Address and AWS_INSTANCE_PORT '80'.

Actual (emulator): check ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected fails with 'Error: Check timed out after 60.0s' and no assertion is reached; its only unbounded waits are the namespace-creation setup and the control service's RUNNING-task / Cloud Map discovery polls. Namespace, service and cluster names differ per run and are not the requirement.

## Steps to Reproduce
1. CreatePrivateDnsNamespace in the shared VPC and create a Cloud Map service with an SRV DNS record; CreateCluster and RegisterTaskDefinition (FARGATE, awsvpc, container 'app' with containerPort 80).
2. CreateService with serviceRegistries=[{registryArn: <SRV service ARN>}] and no port/containerName: expect 400 InvalidParameterException with AWS's SRV sentence, and verify ListServices/DiscoverInstances show nothing was created.
3. Repeat with serviceRegistries=[{registryArn, containerName: 'app', containerPort: 80}] and poll for one RUNNING task, then DiscoverInstances for the registered instance's AWS_INSTANCE_IPV4/AWS_INSTANCE_PORT.
4. Observe the scenario never gets past its waits within the replay budget.
5. Re-replay ecs__servicediscovery.ecs-srv-service-registry-without-a-port-rejected to confirm the fix.
