Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-escc-traj-describe-services-cluster-addressing-mistakes
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-escc-traj-describe-services-cluster-addressing-mistakes ecs-service-deployment-error-paths ecs-ecscm-traj-private-dns-srv-reference-path ecs-ecscm-traj-registry-triplet-mistakes-bridge ecs-ecsdep-traj-service-deployment-read-mistakes
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeServices|4xx->4xx|$.message
Issue-Key: ecs|DescribeServiceDeployments|4xx->4xx|$.message
Issue-Key: ecs|CreateService|4xx->4xx|$.message
Issue-Key: ecs|ListServiceDeployments|4xx->4xx|$.message
Issue-Related: 20261103T150504Z-ecs-error-paths-answer-with-the-wrong-exception.md
Issue-Related: 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md
Issue-Commit: 1d4ff0ec19313d445ee1c97e91766984f5ed6bfa

# ECS misclassifies four service and deployment refusals: ClusterNotFoundException for addressing errors and non-AWS validation prose

> _Note: this issue was identified against the code base at commit `1d4ff0ec19313d445ee1c97e91766984f5ed6bfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Four net-new causes where ECS refuses a request with the right HTTP status but the wrong error code and/or a message that is not AWS's. Clients branch on the __type error code, so a misclassified refusal is indistinguishable from a different fault.

Cause 1 of 4: DescribeServices for a service addressed against the wrong cluster is refused as a missing cluster.
AWS: Smithy ECS DescribeServices models both ClusterNotFoundException and InvalidParameterException (both client errors, httpError 400). When the cluster named in the request exists and the named service exists but belongs to a different cluster, AWS refuses with InvalidParameterException and the message 'Service is not within the provided Cluster.' — that text is fixed, with no variable part.
Emulator: answers 400 with $.__type 'ClusterNotFoundException' and $.message 'Cluster not found.'
Why: a client that retries/creates on ClusterNotFoundException and surfaces a user-facing "wrong cluster" error on InvalidParameterException takes the wrong branch, and the message tells the user their cluster does not exist when it does.
Fix: the DescribeServices handler's cluster/service resolution order — resolve the cluster first and only raise ClusterNotFoundException when the cluster itself is absent, then classify a service found in another cluster as InvalidParameterException.
Traces: ecs-escc-traj-describe-services-cluster-addressing-mistakes#task#6 (and task#8 in the same cluster).

Cause 2 of 4: DescribeServiceDeployments with a malformed service-deployment ARN is refused as a missing cluster.
AWS: Smithy ECS DescribeServiceDeployments takes only serviceDeploymentArns (required StringList) — there is no cluster member in the request — and models InvalidParameterException (client, 400) alongside ClusterNotFoundException. For an ARN that is not a well-formed service-deployment ARN, AWS refuses with InvalidParameterException and the message 'The ServiceDeployment ARN is invalid.' (fixed text, no variable part).
Emulator: answers 400 with $.__type 'ClusterNotFoundException' and $.message 'Cluster not found.'
Why: the operation has no cluster input, so a ClusterNotFoundException is a refusal the client cannot act on; a malformed-ARN bug is reported as missing infrastructure.
Fix: DescribeServiceDeployments' input validation — validate the ARN shape before any cluster lookup and raise InvalidParameterException.
Traces: ecs-service-deployment-error-paths#task#3.

Cause 3 of 4: CreateService's host/bridge serviceRegistries refusal uses emulator prose instead of AWS's message.
AWS: Smithy ECS CreateService -> serviceRegistries: ServiceRegistries, errors include InvalidParameterException (client, 400). When the task definition's networkMode is 'host' or 'bridge' and the registry entry carries a port, AWS refuses with the message "When specifying 'host' or 'bridge' for networkMode, you can\u2019t specify a value for 'port'. Instead, specify values for 'containerName' and 'containerPort' from the task definition.  Registry: <registryArn>" — everything up to 'Registry: ' is fixed text (note the two spaces before 'Registry:' and the typographic apostrophe in \u201ccan\u2019t\u201d), and the only variable part is the serviceRegistries entry's registryArn taken from the request.
Emulator: answers 400 with the same code but the message "When specifying 'host' or 'bridge' for networkMode, values for 'containerName' and 'containerPort' must be specified from the task definition. Registry: <registryArn>" — it describes a missing-containerName fault rather than the illegal 'port' member, so a caller who passed port is told to add fields it already has.
Why: the refusal no longer names the member to remove, so the message cannot be used to fix the request; message assertions and human operators are both misled.
Fix: the CreateService validation that checks serviceRegistries against the task definition's networkMode — emit AWS's fixed text with the registry ARN interpolated.
Traces: ecs-ecscm-traj-private-dns-srv-reference-path#task#12 (and ecs-ecscm-traj-registry-triplet-mistakes-bridge#task#9 in the same cluster).

Cause 4 of 4: ListServiceDeployments' statusFilter validation message lists the wrong enum value set.
AWS: Smithy ECS ListServiceDeployments -> status: ServiceDeploymentStatusList. AWS refuses a lower-cased member with the standard framework message "1 validation error detected: Value '<value>' at '<member path>' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy enum value set: [<values>]]" — the fixed text is that frame, the variable parts are the rejected value and the member path from the request, and the value set is the service's own accepted set. The recording's set is [IN_PROGRESS, CREATED, ROLLBACK_IN_PROGRESS, STOPPED, STOP_REQUESTED, ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, PENDING, ROLLBACK_FAILED, SUCCESSFUL] — ten members, in AWS's own order, including CREATED (which the on-disk ServiceDeploymentStatus enum does not list).
Emulator: answers 400 with the right frame but the set [IN_PROGRESS, PENDING, ROLLBACK_FAILED, ROLLBACK_IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, STOPPED, STOP_REQUESTED, SUCCESSFUL] — nine members, alphabetically sorted, with CREATED missing.
Why: the message is the only place the API tells a caller which statuses are accepted, so a client (or a human) reading it concludes CREATED is not a filterable status; the alphabetical re-ordering also breaks exact message comparison.
Fix: ListServiceDeployments' statusFilter validation — render the enum set in the service's declared order and include CREATED.
Traces: ecs-ecsdep-traj-service-deployment-read-mistakes#task#7 (and task#8 in the same cluster).

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 (score 91.6%, emulator up).

Cause 1 — ecs-escc-traj-describe-services-cluster-addressing-mistakes#task#6, DescribeServices, expected 400 / actual 400: $.__type: value_mismatch (expected 'InvalidParameterException', got 'ClusterNotFoundException'); $.message: value_mismatch (expected 'Service is not within the provided Cluster.', got 'Cluster not found.'). Cluster also covers task#8.
Cause 2 — ecs-service-deployment-error-paths#task#3, DescribeServiceDeployments, 400/400: $.__type: value_mismatch (expected 'InvalidParameterException', got 'ClusterNotFoundException'); $.message: value_mismatch (expected 'The ServiceDeployment ARN is invalid.', got 'Cluster not found.').
Cause 3 — ecs-ecscm-traj-private-dns-srv-reference-path#task#12, CreateService, 400/400: $.message: value_mismatch (expected "When specifying 'host' or 'bridge' for networkMode, you can\u2019t specify a value for 'port'. Instead, specify values for 'containerName' and 'containerPort' from the task definition.  Registry: <registryArn>", got "When specifying 'host' or 'bridge' for networkMode, values for 'containerName' and 'containerPort' must be specified from the task definition. Registry: <registryArn>"). The registry ARN is the request's own value and is the only variable part. Cluster also covers ecs-ecscm-traj-registry-triplet-mistakes-bridge#task#9.
Cause 4 — ecs-ecsdep-traj-service-deployment-read-mistakes#task#7, ListServiceDeployments, 400/400: $.message: value_mismatch (expected "1 validation error detected: Value '[successful]' at 'statusFilter' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy enum value set: [IN_PROGRESS, CREATED, ROLLBACK_IN_PROGRESS, STOPPED, STOP_REQUESTED, ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, PENDING, ROLLBACK_FAILED, SUCCESSFUL]]", got the same frame with "[IN_PROGRESS, PENDING, ROLLBACK_FAILED, ROLLBACK_IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_SUCCESSFUL, STOPPED, STOP_REQUESTED, SUCCESSFUL]"). Cluster also covers task#8.

Other already-open ECS error-taxonomy issues (e.g. 20261029T220901Z-ecs-service-deployment-task-set-and-attribute-error.md, 20261114T160322Z-ecs-daemon-deployment-and-revision-reads-misclassify-refusals.md) cover different steps; none of the four clusters above is owned by an open issue.

## Steps to Reproduce
1. Replay fixture ecs-escc-traj-describe-services-cluster-addressing-mistakes to step task#6: create two clusters and a service in one of them, then DescribeServices naming the service while addressing the other (existing) cluster — the emulator answers ClusterNotFoundException/'Cluster not found.' instead of InvalidParameterException/'Service is not within the provided Cluster.'
2. Replay fixture ecs-service-deployment-error-paths to step task#3: call DescribeServiceDeployments with a syntactically invalid service-deployment ARN — the emulator answers ClusterNotFoundException although the request has no cluster member.
3. Replay fixture ecs-ecscm-traj-private-dns-srv-reference-path to step task#12: register a task definition with networkMode 'bridge' and call CreateService with a serviceRegistries entry carrying a port — compare $.message with AWS's text.
4. Replay fixture ecs-ecsdep-traj-service-deployment-read-mistakes to step task#7: call ListServiceDeployments with status ['successful'] and compare the enum value set printed in $.message.
