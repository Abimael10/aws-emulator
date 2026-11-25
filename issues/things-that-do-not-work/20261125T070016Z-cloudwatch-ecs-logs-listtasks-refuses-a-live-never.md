Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing
Issue-Key: cloudwatch__ecs__logs|ListTasks|ClusterNotFoundException
Issue-Commit: b381b9dbba4fbb02bac11f8b7bd070a116c82713

# cloudwatch__ecs__logs: ListTasks refuses a live, never-deleted cluster with 400 ClusterNotFoundException "The referenced cluster was inactive."

> _Note: this issue was identified against the code base at commit `b381b9dbba4fbb02bac11f8b7bd070a116c82713`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTasks

## Description
Cause 1 of 1: ECS ListTasks refuses a cluster the caller created and has not deleted, reporting it as inactive. Failing check: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing.

AWS: Smithy (com.amazonaws.ecs#ListTasks): input `cluster` is "The short name or full Amazon Resource Name (ARN) of the cluster to use when filtering the ListTasks results", `desiredStatus` is one of PENDING | RUNNING | STOPPED, output is {taskArns, nextToken} with HTTP 200. Its modelled errors are AccessDeniedException, ClientException, ClusterNotFoundException (httpError 400: "The specified cluster wasn't found. You can view your available clusters with ListClusters. Amazon ECS clusters are Region specific."), InvalidParameterException, ServerException, ServiceNotFoundException. ClusterNotFoundException is AWS's answer only for a name/ARN the account holds no cluster for, or for a cluster already deleted and still tombstoned INACTIVE — and in that INACTIVE case AWS's message is the fixed sentence "The referenced cluster was inactive." For a cluster that CreateCluster created and DescribeClusters reports with status ACTIVE, ListTasks answers 200 with that cluster's task ARNs (an empty list when it has none), whatever the tasks are doing — including right after StopTask has been accepted for them. For the telemetry half of this scenario the model says nothing, so that is recorded behavior only; the recording's calls all succeed (HTTP 200, no error code).

Emulator: in the scenario's own teardown leg — after StopTask was accepted for the tasks of a cluster this check created with CreateCluster and never deleted — ListTasks(cluster=<that cluster>, desiredStatus=RUNNING) is refused with HTTP 400 ClusterNotFoundException and the message "The referenced cluster was inactive." Message anatomy: that sentence is entirely fixed text with no variable part, and it is the correct sentence for a genuinely INACTIVE cluster; what diverges is that it is served at all here — the expected response is 200 with the cluster's task ARNs. The refusal is the INACTIVE branch of the handler's cluster gate: ListTasks returns ClusterNotFoundException built from the store's cluster record when `has_active_cluster` is false, so the store is holding that cluster with a status other than ACTIVE even though the client deleted nothing. Hypothesis: the cluster is retired by something other than the client's own DeleteCluster — either the DeleteCluster of a sibling cluster earlier in the scenario (the check creates three clusters with a shared name prefix and deletes only the third) or the lifecycle pass ListTasks itself drives before reading (the clock advance the handler performs) tombstones more than the cluster named.

Note on already-filed work: the open issue 20261123T014745Z-cloudwatch-ecs-logs-container-insights-inherited-from-the-account-default-publishes.md names the same check but is keyed on a different failure (the check being killed at the 600 s cap while polling for ECS/ContainerInsights TaskCount and the performance log group). This replay gets past that point and fails later, on this ListTasks refusal, so that issue does not cover this cause; the open issue 20261104T062603Z-ecs-reads-hang-or-mis-resolve-on-clusters.md (ECS reads mis-resolving clusters) is related but is grounded in other checks and does not carry this one.

Why: a client cannot enumerate the tasks of a cluster it owns. Any poll of ListTasks (waiting for tasks to stop, draining a cluster before DeleteCluster, inventory/reconciliation loops) fails with a 400 that a correct client treats as "this cluster no longer exists", so it stops retrying and either abandons live tasks or deletes state for a cluster that is in fact ACTIVE and still running work. Because the cluster is reported inactive by a refusal rather than by DescribeClusters, the client's view of the estate and the service's disagree.

Fix: the ECS service's cluster lifecycle in its state — whatever transitions a cluster record out of ACTIVE must do so only for the cluster a DeleteCluster named — together with the cluster gate at the head of the ListTasks handler, which must resolve a cluster that is ACTIVE and answer 200 with the filtered task ARNs.

Traces: cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing.

## Evidence
Suite report, ### cloudwatch__ecs__logs: "[error] cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing — botocore.errorfactory.ClusterNotFoundException: An error occurred (ClusterNotFoundException) when calling the ListTasks operation: The referenced cluster was inactive." The only ListTasks calls in the check are in its teardown helper, which calls ListTasks(cluster=<cluster>, desiredStatus="RUNNING") and ListTasks(cluster=<cluster>, desiredStatus="STOPPED") for each cluster it created, immediately after StopTask was accepted for that cluster's tasks and before any DeleteCluster of it.
Expected: HTTP 200 with {"taskArns": [...]} (the cluster's tasks, or an empty list), because the cluster was created by this caller, was never deleted, and DescribeClusters reports it ACTIVE.
Actual: HTTP 400, Error.Code "ClusterNotFoundException", Error.Message "The referenced cluster was inactive." (fixed text, no variable parts). The generated cluster names and task ARNs in the run differ per environment and are not part of the requirement.

## Steps to Reproduce
1. Replay cloudwatch__ecs__logs.obs-ecs-2-account-default-insights-inherited-explicit-disabled-publishes-nothing.
2. Short form: PutAccountSettingDefault(name=containerInsights, value=enabled); CreateCluster A with no settings, CreateCluster B with settings=[{name: containerInsights, value: disabled}], CreateCluster C with no settings and DeleteCluster C; RegisterTaskDefinition (FARGATE/awsvpc, cpu 256, memory 512) and RunTask one task in A and in B; wait for both tasks RUNNING.
3. StopTask each task, then call ListTasks(cluster=A, desiredStatus="RUNNING") and ListTasks(cluster=B, desiredStatus="RUNNING") — neither cluster has been deleted.
4. Observe 400 ClusterNotFoundException "The referenced cluster was inactive." instead of 200 with taskArns; DescribeClusters on the same name is expected to report status ACTIVE.
