Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery
Issue-Key: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch|run#26|TimeoutError|raise TimeoutError(f"pod {pod_name} leaving Pending: the run budget ended first; not graded")
Issue-Key: ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery|_wait_pod_phase#27|Timeout|time.sleep(2)

# ecr__eks__logs: a Fargate pod's containers are never started, so the pod never reports Running and nothing it writes is delivered to CloudWatch Logs (2 causes)

> _Filed by the fixer of `20261106T070547Z-ecr-eks-logs-a-fargate-profile-cluster-s.md` (now under `things-that-now-work`). That report's one cause was the cluster's certificate-authority material, which is fixed: the two checks below now get through `CreateCluster`, `CreateFargateProfile`, `DescribeCluster`, the TLS handshake against the published authority, `POST /api/v1/namespaces`, the `aws-logging` ConfigMap and `POST /api/v1/namespaces/default/pods`, and then diverge on what is recorded here instead. The replay keys above are the steps they diverge on now._

## Operation
CreateNamespacedPod (the cluster's Kubernetes API), and the CloudWatch Logs delivery the Fargate log router makes

## Description
Cause 1 of 2: a pod a Fargate profile placed never reports the phase a pod whose containers were started is in.
AWS: a pod the Fargate scheduler placed on capacity has its containers created and started once its images are pulled, so `Pod.status.phase` reaches `Running` (`Succeeded`/`Failed` only once those containers have terminated), each `status.containerStatuses[].state` carries `running` with a `startedAt`, and `Ready`/`ContainersReady` become `True`. A deployment tool therefore polls the pod until it leaves `Pending` and then reads a RUNNING pod.
Emulator: the pod is admitted, placed on the Fargate node its profile provisions and its images are pulled as the Pod execution role — all of which is right — and then it ends in `Failed` with `reason: StartError`, because this emulator starts no container at all (`services/service-com-amazonaws-eks-awswesleyfrontend/src/state/kubernetes_objects.rs`, `POD_PHASE_FAILED` / `POD_NO_CONTAINER_RUNTIME_MESSAGE`). A client waiting for a phase a running pod reports waits until its deadline.
Why: every scenario whose subject is what a pod DOES — a workload serving traffic, a job finishing, a sidecar shipping logs — is unreachable on an emulated cluster, because no pod of one ever runs.
Fix: a pod's containers have to be RUN in process, the way the Amazon ECS crate already runs a task's container command (`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state/container_shell.rs`, `container_filesystem.rs`, `container_servers.rs`, over `service_common::posix_shell_session` and `service_common::static_web_servers`), so the phase, the container statuses and the pod's conditions report what those containers really did. The readings those modules hold are the ones to share rather than copy.
Traces: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch, ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery.

Cause 2 of 2: nothing delivers a Fargate pod's output to CloudWatch Logs.
AWS: Amazon EKS on Fargate runs a log router beside each pod, configured by the `aws-logging` ConfigMap of the `aws-observability` namespace ("Fargate logging", Amazon EKS User Guide): with a `cloudwatch_logs` output stanza naming a log group, the router creates that group and a stream per pod and puts what the pod's containers wrote to stdout/stderr there, AS THE POD EXECUTION ROLE — so the delivery happens only while that role's policies allow `logs:CreateLogGroup`/`logs:CreateLogStream`/`logs:PutLogEvents`, and no group, stream or event appears when they do not.
Emulator: the ConfigMap is stored and served, but nothing reads it: no log group is created, no stream is opened and no event is put for any pod, whatever the Pod execution role allows. The check that expects delivery and the check that expects none are therefore indistinguishable — both see an empty CloudWatch Logs.
Why: the documented Fargate logging path cannot be exercised at all, and the negative case (no permission, no delivery) passes only because the positive case does not work either, which is not evidence of anything.
Fix: the EKS side has to read the `aws-logging` ConfigMap of `aws-observability` as the log router's configuration and deliver what a pod's containers wrote through the CloudWatch Logs service, as the Pod execution role, with the role's policies deciding it — which depends on cause 1, since there is no output to deliver until a pod's containers run. The Amazon ECS crate's `awslogs` delivery is the shape to follow (`service_common::cloudwatch_logs`).
Traces: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch (expects the delivery), ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery (expects none).

## Evidence
Both checks now get past every call the earlier report was about and stop on the pod's phase:

[error] ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch
  TimeoutError: pod <per-run pod name> leaving Pending: the run budget ended first; not graded

[error] ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery
  Check timed out after 60.0s, in _wait_pod_phase

Read directly against the assembled emulator, a pod created in a namespace an ACTIVE Fargate profile selects is placed on that profile's node, leaves `Pending` within the span the service owes a pod's start, and then reports:
  status.phase = "Failed", status.reason = "StartError",
  status.message = "This emulator does not run containers, so the pod's containers were never started",
  status.containerStatuses[0].state.terminated.reason = "StartError"
and `DescribeLogGroups` in the same account holds no group the `aws-logging` ConfigMap named.

Expected: `status.phase` = "Running" with the container reporting `state.running`, and — with a Pod execution role allowed the CloudWatch Logs calls — a log group named by that ConfigMap holding a stream for the pod; with the permission withheld, no group and no events.

## Steps to Reproduce
1. CreateRole for a cluster role and a pod execution role (trusting `eks-fargate-pods.amazonaws.com`, with `AmazonEKSFargatePodExecutionRolePolicy` and, for the delivery case, the CloudWatch Logs calls above).
2. CreateCluster with private subnets; wait for ACTIVE. CreateFargateProfile with that role and a selector on namespace `default`; wait for ACTIVE.
3. Build a kubeconfig from DescribeCluster, create the `aws-observability` namespace and an `aws-logging` ConfigMap whose `output.conf` carries a `cloudwatch_logs` output with a `log_group_name`.
4. POST a pod into `default` and poll it.
   -> it leaves `Pending` for `Failed`/`StartError` instead of `Running` (cause 1), and no log group the ConfigMap named is ever created (cause 2).
