Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.containers-per-task-definition-limit-rejected ecs.create-service-invalid-name-characters-rejected ecs.create-task-set-default-controller-rejected-deployment-unchanged ecs.delete-cluster-refused-with-running-count-then-inactive ecs.inactive-cluster-refusals-with-active-neighbor ecs.list-clusters-maxresults-above-hundred-rejected ecs.list-taskdefs-vs-families-status-enum-rejected ecs.register-task-definition-rejects-unrecognized-device-permission ecs.stop-task-wrong-cluster-rejected ecs.tag-resource-fifty-tag-cap-rejects-fifty-first-key
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.containers-per-task-definition-limit-rejected|run#9|AssertionError|assert message == "Too many containers.", f"unexpected message: {message!r}"
Issue-Key: ecs.create-service-invalid-name-characters-rejected|run#13|AssertionError|assert error == expected, f"CreateService with a malformed name: expected {expected!r}, got {error!r}"
Issue-Key: ecs|CreateTaskSet|InvalidParameterException
Issue-Key: ecs.delete-cluster-refused-with-running-count-then-inactive|run#18|AssertionError|assert (code, message) == _REFUSED, (
Issue-Key: ecs.inactive-cluster-refusals-with-active-neighbor|expect_error#15|AssertionError|assert got == (code, message), f"expected {(code, message)!r}, got {got!r}"
Issue-Key: ecs.list-clusters-maxresults-above-hundred-rejected|run#12|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: ecs.list-taskdefs-vs-families-status-enum-rejected|run#13|AssertionError|assert message == expected_td_message, (
Issue-Key: ecs|RegisterTaskDefinition|InvalidParameterException
Issue-Key: ecs.stop-task-wrong-cluster-rejected|run#21|AssertionError|assert message == "Invalid identifier: cluster identifiers mismatch", (
Issue-Key: ecs.tag-resource-fifty-tag-cap-rejects-fifty-first-key|run#11|AssertionError|assert code == "ClientException", f"expected ClientException, got {code!r}"
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ECS refusal wording and error classification diverge from AWS on ten refusals (10 causes)

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTaskDefinition

## Description
Ten net-new root causes, all of one family: the emulator refuses the request with the right HTTP status but answers a different message than AWS words for that error class, and in three of them a different error CODE. An HTTP status, an error code and an error message are all requirements, so each of these is a divergence even where the status matches. Each cause below gives AWS's template (fixed text, plus which parts come from the request or from state), the emulator's answer, and the component it belongs in. This is the same FAMILY as fixed issues 20261103T150504Z-ecs-validation-refusals-use-emulator-authored-wording-instead.md and 20261104T062603Z-ecs-refusal-messages-and-error-classes-diverge-from.md, but none of the ten operations below is carried by either (those cover CreateService/List*/Describe* deployment reads and RunTask/CreateCapacityProvider), so the causes here are net-new.

Cause 1 of 10: RegisterTaskDefinition over the container-per-task-definition cap.
AWS: 400, message exactly 'Too many containers.' — recorded behavior only (Smithy: RegisterTaskDefinitionRequest.containerDefinitions is a plain ContainerDefinitions list with NO @length trait; ClientException, InvalidParameterException and LimitExceededException are all modeled on the operation). The whole sentence is fixed text; it names neither the member nor the limit.
Emulator: 400 with 'container definitions can have at most 10 items.' — the generic list-length sentence built from the wire member name and the cap.
Why: a client or operator matching ECS's documented refusal text does not recognise the refusal, and the emulator exposes a limit phrasing AWS never emits.
Fix: the RegisterTaskDefinition validation — a per-operation literal sentence instead of the shared list-length message.
Traces: ecs.containers-per-task-definition-limit-rejected; handler services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/register_task_definition.rs.

Cause 2 of 10: CreateService with a serviceName that violates the name pattern.
AWS: 400 InvalidParameterException, template 'ServiceName must match ^[a-zA-Z0-9\\-_]{1,255}$, but was <serviceName> not allowed!' — fixed text with the subject spelled 'ServiceName'; the only variable part is the rejected name taken from the request (recorded behavior only; Smithy models CreateServiceRequest.serviceName as a @required String with no @pattern).
Emulator: 400 InvalidParameterException with the same sentence but the subject spelled 'Service name'.
Why: message-matching clients and runbooks keyed to AWS's text miss the refusal.
Fix: the CreateService name validation message (src/ops/create_service.rs).
Traces: ecs.create-service-invalid-name-characters-rejected.

Cause 3 of 10: CreateTaskSet against a service left on the default (ECS) deployment controller.
AWS: 400 InvalidParameterException 'Amazon ECS only supports task set management on services configured to use external deployment controllers.' — entirely fixed text, no variable part (recorded behavior only; Smithy models InvalidParameterException and UnsupportedFeatureException on CreateTaskSet).
Emulator: 400 InvalidParameterException 'A task set can only be created in a service that uses the EXTERNAL or CODE_DEPLOY deployment controller.' — emulator-authored prose that also enumerates controllers AWS's sentence does not.
Fix: the CreateTaskSet controller guard's message (src/ops/create_task_set.rs).
Traces: ecs.create-task-set-default-controller-rejected-deployment-unchanged.

Cause 4 of 10: DeleteCluster while a task of the cluster is still running.
AWS: 400 ClusterContainsTasksException with message 'The Cluster cannot be deleted while Tasks are active.' — fixed text, no variable part (Smithy: ClusterContainsTasksException is modeled on DeleteCluster, httpError 400).
Emulator: the right code, but the message "You can't delete a cluster that has active tasks." — which is verbatim the Smithy DOC string of ClusterContainsTasksException, not the message the service returns. The defect is using the shape's documentation as the error message.
Fix: the DeleteCluster active-tasks refusal (src/ops/delete_cluster.rs) — the message must be the service's sentence, never the shape doc.
Traces: ecs.delete-cluster-refused-with-running-count-then-inactive.

Cause 5 of 10: operations addressed at a cluster that has been deleted (INACTIVE) while an ACTIVE cluster exists beside it.
AWS: 400 ClientException 'Cluster was not ACTIVE.' — fixed text, no variable part (recorded behavior only; ClientException and ClusterNotFoundException are both modeled, and AWS distinguishes them: an INACTIVE cluster is found but not usable).
Emulator: 400 ClusterNotFoundException 'The referenced cluster was inactive.' — the wrong error class as well as the wrong sentence.
Why: a client branching on ClusterNotFoundException to recreate the cluster takes the wrong branch, because AWS says the cluster exists and is not ACTIVE.
Fix: the shared cluster-resolution guard in the ECS crate's state module that the cluster-scoped handlers call.
Traces: ecs.inactive-cluster-refusals-with-active-neighbor.

Cause 6 of 10: ListClusters with maxResults above the documented maximum.
AWS: 400 InvalidParameterException 'Max results can not be greater than 100' — recorded behavior only (Smithy models ListClustersRequest.maxResults as BoxedInteger with NO @range; the bound lives in prose). Fixed text ending in the operation's documented maximum; it echoes neither the value sent nor the member spelling.
Emulator: 400 InvalidParameterException 'MaxResults must be a positive number between 1-100.'
Why: the above-maximum refusal for this operation now carries another operation's sentence, so page-size error handling keyed to ListClusters' text breaks. Note fixed issue 20261028T122853Z-ecs-listdaemontaskdefinitions-maxresults-below-the-documented-range-is.md moved the shared page-size helper onto the 'MaxResults must be a positive number between 1-100.' sentence at BOTH ends of the range and explicitly warned that the other lists' wording must be confirmed against their own recordings rather than assumed; this check is that confirmation failing for ListClusters' high end.
Fix: the ListClusters maxResults guard (src/ops/list_clusters.rs) / the shared page-size violation helper — the sentence must be per-operation.
Traces: ecs.list-clusters-maxresults-above-hundred-rejected.

Cause 7 of 10: ListTaskDefinitions with a status outside the enum.
AWS: 400 'Status should be one of [ACTIVE,INACTIVE,DELETE_IN_PROGRESS]' — capitalised subject and the enum in the MODEL's order (Smithy TaskDefinitionStatus values: ACTIVE, INACTIVE, DELETE_IN_PROGRESS). Fixed text; the bracketed list is the operation's accepted set.
Emulator: 400 'status should be one of [ACTIVE,DELETE_IN_PROGRESS,INACTIVE]' — lower-cased subject and the enum sorted alphabetically.
Fix: the ListTaskDefinitions status validation (src/ops/list_task_definitions.rs) — render the per-operation subject and the modeled order, not a sort of the enum.
Traces: ecs.list-taskdefs-vs-families-status-enum-rejected.

Cause 8 of 10: RegisterTaskDefinition with an unrecognised device cgroup permission in linuxParameters.devices.
AWS: 400 ClientException 'Invalid permission: execute for device /dev/null' — template 'Invalid permission: ' + the rejected permission value + ' for device ' + the device's containerPath, both from the request (recorded behavior only; Smithy DeviceCgroupPermission = [read, write, mknod]).
Emulator: 400 InvalidParameterException 'permissions should be one of [mknod,read,write]' — the wrong error class and the generic enum sentence, which also loses which device and which value were rejected.
Fix: the RegisterTaskDefinition linuxParameters.devices validation (src/ops/register_task_definition.rs).
Traces: ecs.register-task-definition-rejects-unrecognized-device-permission.

Cause 9 of 10: StopTask naming a task that exists, but in a different cluster than the one in the request.
AWS: 400 InvalidParameterException 'Invalid identifier: cluster identifiers mismatch' — fixed text, no variable part (recorded behavior only).
Emulator: 400 with 'The referenced task was not found' — it reports the task as absent instead of reporting the cluster/task identifier mismatch.
Why: the caller is told the task does not exist when AWS tells it the task is in another cluster; a client that reacts by re-creating or giving up on the task takes the wrong branch.
Fix: the StopTask identifier resolution (src/ops/stop_task.rs) — a task found under another cluster is a mismatch, not a miss.
Traces: ecs.stop-task-wrong-cluster-rejected.

Cause 10 of 10: TagResource past the 50-tag cap (the fifty-first key).
AWS: 400 ClientException (Smithy: TagResourceRequest.tags is @required with @length max 50, and ClientException, InvalidParameterException and LimitExceededException are all modeled on the operation; the recording classifies the cap breach as ClientException).
Emulator: 400 LimitExceededException.
Why: the error class is the part clients branch on — LimitExceededException reads as a quota/throttle condition that may clear on retry, while ClientException is a permanent request defect.
Fix: the TagResource tag-cap guard (src/ops/tag_resource.rs) — raise ClientException for the cap breach.
Traces: ecs.tag-resource-fifty-tag-cap-rejects-fifty-first-key.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against this checkout; all ten checks are listed under '### ecs' and none is marked ALREADY FILED or OWNED.
Cause 1 — ecs.containers-per-task-definition-limit-rejected: AssertionError: unexpected message: 'container definitions can have at most 10 items.' (expected 'Too many containers.').
Cause 2 — ecs.create-service-invalid-name-characters-rejected: AssertionError: CreateService with a malformed name: expected ('InvalidParameterException', 'ServiceName must match ^[a-zA-Z0-9\\-_]{1,255}$, but was <serviceName> not allowed!'), got ('InvalidParameterException', 'Service name must match ^[a-zA-Z0-9\\-_]{1,255}$, but was <serviceName> not allowed!'). The service name quoted in the run is a per-run generated name filling the template.
Cause 3 — ecs.create-task-set-default-controller-rejected-deployment-unchanged: expected ('InvalidParameterException', 'Amazon ECS only supports task set management on services configured to use external deployment controllers.'), got ('InvalidParameterException', 'A task set can only be created in a service that uses the EXTERNAL or CODE_DEPLOY deployment controller.').
Cause 4 — ecs.delete-cluster-refused-with-running-count-then-inactive: expected ('ClusterContainsTasksException', 'The Cluster cannot be deleted while Tasks are active.') while a task is running, got ('ClusterContainsTasksException', "You can't delete a cluster that has active tasks.").
Cause 5 — ecs.inactive-cluster-refusals-with-active-neighbor: expected ('ClientException', 'Cluster was not ACTIVE.'), got ('ClusterNotFoundException', 'The referenced cluster was inactive.').
Cause 6 — ecs.list-clusters-maxresults-above-hundred-rejected: expected message 'Max results can not be greater than 100', got 'MaxResults must be a positive number between 1-100.'.
Cause 7 — ecs.list-taskdefs-vs-families-status-enum-rejected: ListTaskDefinitions: expected message 'Status should be one of [ACTIVE,INACTIVE,DELETE_IN_PROGRESS]', got 'status should be one of [ACTIVE,DELETE_IN_PROGRESS,INACTIVE]'.
Cause 8 — ecs.register-task-definition-rejects-unrecognized-device-permission: expected error code 'ClientException' (message 'Invalid permission: execute for device /dev/null'), got 'InvalidParameterException' ('permissions should be one of [mknod,read,write]').
Cause 9 — ecs.stop-task-wrong-cluster-rejected: unexpected error message: 'The referenced task was not found' (expected 'Invalid identifier: cluster identifiers mismatch').
Cause 10 — ecs.tag-resource-fifty-tag-cap-rejects-fifty-first-key: expected ClientException, got 'LimitExceededException'.

## Steps to Reproduce
1. git_clone this checkout and boot the emulator.
2. Cause 1: RegisterTaskDefinition with 11 container definitions.
3. Cause 2: CreateService with a serviceName containing a character outside ^[a-zA-Z0-9\\-_]{1,255}$.
4. Cause 3: CreateService without deploymentController (default ECS), then CreateTaskSet against it.
5. Cause 4: run a task in a cluster, then DeleteCluster on it.
6. Cause 5: CreateCluster, DeleteCluster (leaving it INACTIVE), then address a cluster-scoped operation at the INACTIVE cluster while another cluster is ACTIVE.
7. Cause 6: ListClusters with maxResults=400.
8. Cause 7: ListTaskDefinitions with status outside [ACTIVE,INACTIVE,DELETE_IN_PROGRESS].
9. Cause 8: RegisterTaskDefinition with linuxParameters.devices=[{hostPath:'/dev/null', containerPath:'/dev/null', permissions:['execute']}].
10. Cause 9: run a task in cluster A, then StopTask with cluster=B and that task's id.
11. Cause 10: TagResource with 51 tags on an ECS resource.
12. Re-replay the ten check ids in trace_ids to confirm each status, code and message.
