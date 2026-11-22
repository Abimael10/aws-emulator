Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events ecs-ecsnrt-traj-zero-capacity-provider-strategy-repair ecs-fargate-run-task-reports-container-exit-code ecs-daemon-and-express-gateway-error-paths ecs-ecsdep-traj-update-service-revision-mistakes
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|UpdateService|2xx->2xx|$.service.availabilityZoneRebalancing
Issue-Key: ecs|DescribeServices|2xx->2xx|$.services[*].availabilityZoneRebalancing
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].containers[*].imageDigest
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.__type
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.message
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|status
Issue-Key: ecs|UpdateService|4xx->4xx|$.message
Issue-Related: 20261103T150504Z-ecs-error-paths-answer-with-the-wrong-exception.md
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# ECS service, task and daemon reads return wrong field values and skip two request validations

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Five independent divergences on the service/task/daemon read and validation paths: a service's availabilityZoneRebalancing is reported ENABLED where AWS reports DISABLED (on both the write and the read op), a task's container never carries an image digest, DescribeDaemonDeployments accepts a request AWS refuses, and UpdateService's blank-family refusal carries the wrong class and message. 5 causes.

Cause 1 of 5: UpdateService returns availabilityZoneRebalancing ENABLED for a service that never had the field set.
AWS: Smithy: Service.availabilityZoneRebalancing -> AvailabilityZoneRebalancing [ENABLED, DISABLED], documented as "For update service requests, when no value is specified for AvailabilityZoneRebalancing, Amazon ECS defaults to the existing service's AvailabilityZoneRebalancing value. If the service never had an AvailabilityZoneRebalancing value set, Amazon ECS treats this as DISABLED." The recorded UpdateService response for a service created without the member therefore reports 'DISABLED'.
Emulator: `service.availabilityZoneRebalancing` is 'ENABLED' (status 200 both sides). Hypothesis: the update path applies the create-request default instead of carrying the stored value forward.
Why: a client reading the field after an unrelated update (a desiredCount change) is told ECS will rebalance the service's tasks across Availability Zones when it will not, so placement expectations and any configuration drift check on the service are wrong.
Fix: the UpdateService handler's merge of the request onto the stored service — an absent availabilityZoneRebalancing must preserve the stored value, defaulting to DISABLED when never set.
Traces: ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events#task#5 (representative; 4 steps in the cluster, including ecs-ecsnrt-traj-zero-capacity-provider-strategy-repair#task#15).

Cause 2 of 5: DescribeServices reports the same wrong availabilityZoneRebalancing.
AWS: as Cause 1 — DescribeServices returns the stored Service, so `services[0].availabilityZoneRebalancing` is 'DISABLED' for a service that never had the value set.
Emulator: 'ENABLED' (status 200 both sides).
Why: the wrong value is not confined to the update response; every subsequent read of the service repeats it, so a drift detector or IaC refresh keeps seeing a setting the service does not have.
Fix: the stored service state (the field persisted by Create/UpdateService) and the DescribeServices rendering of it.
Traces: ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events#task#6 (representative; 2 steps in the cluster, with ecs-ecsnrt-traj-zero-capacity-provider-strategy-repair#task#16).

Cause 3 of 5: DescribeTasks omits the container's imageDigest.
AWS: Smithy: Container.imageDigest, "The container image manifest digest." Recorded behavior: for a task whose container ran from a resolved image, AWS populates `tasks[].containers[].imageDigest` with the manifest digest (the recorded value is shape-asserted only — the digest itself is environment-specific and is not a requirement; its presence is).
Emulator: `tasks[0].containers[0].imageDigest` is absent (status 200 both sides).
Why: a client that pins or audits exactly which image a task ran — reconciling a running task against a repository digest, or recording provenance — has nothing to compare, so digest-based verification cannot be performed at all.
Fix: the task/container state must record the resolved manifest digest when the container's image is resolved, and the DescribeTasks container serializer must emit it.
Traces: ecs-fargate-run-task-reports-container-exit-code#task#12.

Cause 4 of 5: DescribeDaemonDeployments accepts an invalid request instead of refusing it.
AWS: Smithy: DescribeDaemonDeployments input `daemonDeploymentArns` (required, StringList); modeled errors include InvalidParameterException (HTTP 400). AWS refuses a request whose parameters do not validate with 400 InvalidParameterException and message 'Request parameter(s) are invalid.' — fixed text, no variable part.
Emulator: answers 200 with no `__type`/`message` at all.
Why: a client sending a malformed daemon-deployment ARN gets a success with no deployments rather than a validation failure, so the mistake is indistinguishable from "no such deployment" and client-side retries/repair never trigger. The open issue 20261029T220901Z-ecs-express-gateway-and-daemon-operations-skip-validation.md covers other daemon/express-gateway validation gaps but does not carry this step's replay key.
Fix: DescribeDaemonDeployments input validation, ahead of the lookup.
Traces: ecs-daemon-and-express-gateway-error-paths#task#5.

Cause 5 of 5: UpdateService's blank task-definition family refusal has the wrong exception class and message.
AWS: Smithy: UpdateService models both InvalidParameterException and ClientException at HTTP 400. When the request's taskDefinition has an empty family component, AWS refuses with InvalidParameterException and message 'Family should not be null or empty.' — fixed text, no variable part (it names the member, not the submitted value).
Emulator: returns 400 ClientException with message 'Family contains invalid characters.', i.e. it reports an unparseable family rather than a missing one.
Why: a client branching on the exception type and message to tell a missing required component from a malformed one reports the wrong fault to its caller and cannot auto-correct by supplying the family.
Fix: the UpdateService handler's taskDefinition parsing/validation must distinguish an empty family from one with invalid characters, and raise InvalidParameterException for the empty case.
Traces: ecs-ecsdep-traj-update-service-revision-mistakes#task#13 (representative; 2 steps in the cluster, with #task#14).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout.

Cause 1 — ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events#task#5, UpdateService, expected 200 / actual 200: "$.service.availabilityZoneRebalancing: value_mismatch (expected 'DISABLED', got 'ENABLED')". 4 steps in cluster (also ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events#task#11, ecs-ecsnrt-traj-zero-capacity-provider-strategy-repair#task#15 and #task#17).

Cause 2 — ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events#task#6, DescribeServices, expected 200 / actual 200: "$.services[0].availabilityZoneRebalancing: value_mismatch (expected 'DISABLED', got 'ENABLED')". 2 steps in cluster.

Cause 3 — ecs-fargate-run-task-reports-container-exit-code#task#12, DescribeTasks, expected 200 / actual 200: "$.tasks[0].containers[0].imageDigest: missing (expected '<shaped-not-asserted>', got None)".

Cause 4 — ecs-daemon-and-express-gateway-error-paths#task#5, DescribeDaemonDeployments, expected 400 / actual 200: "$.__type: missing (expected 'InvalidParameterException', got None)"; "$.message: missing (expected 'Request parameter(s) are invalid.', got None)".

Cause 5 — ecs-ecsdep-traj-update-service-revision-mistakes#task#13, UpdateService, expected 400 / actual 400: "$.__type: value_mismatch (expected 'InvalidParameterException', got 'ClientException')"; "$.message: value_mismatch (expected 'Family should not be null or empty.', got 'Family contains invalid characters.')".

Model evidence: smithy_query shape=Service member availabilityZoneRebalancing (doc: update requests with no value keep the existing value; a service that never had one is treated as DISABLED); shape=AvailabilityZoneRebalancing -> [ENABLED, DISABLED]; shape=Container member imageDigest ("The container image manifest digest."); operation=DescribeDaemonDeployments -> input daemonDeploymentArns required, errors include InvalidParameterException; operation=UpdateService -> errors include InvalidParameterException and ClientException.

## Steps to Reproduce
1. Replay fixture ecs-ecsnrt-traj-ec2-service-scaled-up-without-capacity-events: create an EC2-launch-type service without availabilityZoneRebalancing, then at step task#5 call UpdateService (desiredCount change only) and at task#6 DescribeServices — both report ENABLED instead of DISABLED.
2. Replay fixture ecs-fargate-run-task-reports-container-exit-code, step task#12: RunTask, then DescribeTasks — tasks[0].containers[0].imageDigest is absent.
3. Replay fixture ecs-daemon-and-express-gateway-error-paths, step task#5: DescribeDaemonDeployments with an invalid daemonDeploymentArns value — 200 instead of 400 InvalidParameterException 'Request parameter(s) are invalid.'.
4. Replay fixture ecs-ecsdep-traj-update-service-revision-mistakes, step task#13: UpdateService with a taskDefinition whose family component is empty — ClientException 'Family contains invalid characters.' instead of InvalidParameterException 'Family should not be null or empty.'.
