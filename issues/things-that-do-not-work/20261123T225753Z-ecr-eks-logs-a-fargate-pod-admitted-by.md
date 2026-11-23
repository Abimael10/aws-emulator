Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery
Issue-Key: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch|run#26|TimeoutError|raise TimeoutError(f"pod {pod_name} leaving Pending: the run budget ended first; not graded")
Issue-Key: ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery|_wait_pod_phase#27|Timeout|time.sleep(2)
Issue-Commit: a6ebf3b3eb6378b4a705918818682bef1381d4ad

# ecr__eks__logs: a Fargate pod admitted by an ACTIVE Fargate profile never leaves Pending, so no Fargate pod ever runs or delivers stdout to the aws-logging ConfigMap's log group (1 cause)

> _Note: this issue was identified against the code base at commit `a6ebf3b3eb6378b4a705918818682bef1381d4ad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFargateProfile

## Description
Cause 1 of 1: the EKS Fargate data plane never starts a Pod. The served cluster Kubernetes API now accepts the whole documented setup (namespace `aws-observability`, the `aws-logging` ConfigMap, and the Pod itself are all created successfully — so this is NOT the CA/PEM handshake defect of open issue 20261106T070547Z-ecr-eks-logs-a-fargate-profile-cluster-s.md, whose replay keys no longer reproduce), but the admitted Pod's `status.phase` stays `Pending` for the entire wait and never becomes `Running`/`Succeeded`. Because the container never runs, its stdout never exists, and the CloudWatch Logs group named in the aws-logging ConfigMap's cloudwatch_logs OUTPUT stanza stays empty forever. Failing checks: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch (graded evidence: TimeoutError raised at the pod-phase wait), ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery (the negative twin of the same scenario — its control leg also needs a Fargate pod to run after the pod-execution role is granted logs:PutLogEvents, and it never reaches a verdict either).

AWS: Smithy (services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json): eks#FargateProfile.selectors is documented as "The selectors to match for a Pod to use this Fargate profile"; once CreateFargateProfile reports status ACTIVE for a selector on namespace `default`, a Pod created in that namespace is scheduled onto Fargate by the control plane and transitions Pending -> Running (then Succeeded for a one-shot command), with the Pod's stdout captured by the built-in Fluent Bit router configured from the `aws-logging` ConfigMap in namespace `aws-observability` and PutLogEvents'd into the `log_group_name` of its cloudwatch_logs OUTPUT stanza under streams prefixed by `log_stream_prefix`. The per-pod scheduling latency is recorded behavior only; the state transition itself is the requirement.

Emulator: CreateCluster, CreateFargateProfile and DescribeCluster all answer 200 with an ACTIVE cluster and ACTIVE profile, and the served Kubernetes API answers the namespace/ConfigMap/Pod creations, but `GET /api/v1/namespaces/default/pods/<pod>` keeps reporting `status.phase == "Pending"` for every poll until the wait's deadline; the check then raises `TimeoutError: pod <name> leaving Pending: the run budget ended first; not graded` (the pod name is a per-run generated value; the requirement-bearing part is that the phase never leaves Pending). No log stream and no log event is ever created in the ConfigMap's log group, so both the positive delivery leg and the negative (permission-denied) contrast are unobservable.

Why: a client cannot run any workload on an emulated EKS Fargate profile. Every Fargate scenario — pod execution, pod-execution-role-driven private ECR pulls, and Fargate pod log delivery to CloudWatch Logs — hangs in Pending instead of either running or being refused, so a caller gets no terminal state at all. The negative twin is worse: a scenario that asserts "no delivery without the IAM grant" passes vacuously against an emulator in which nothing is ever delivered, hiding a real over-permissiveness.

Fix: the EKS Fargate scheduling path of the served cluster Kubernetes API — the component that owns Pod state for pods matching an ACTIVE FargateProfile selector, and the built-in Fluent Bit log-router that reads the `aws-logging` ConfigMap and writes to CloudWatch Logs (services/service-com-amazonaws-eks-awswesleyfrontend/src, pod/kube-API state plus its awslogs delivery), must move an admitted Fargate pod to Running/Succeeded and emit its stdout into the configured log group; alternatively refuse the pod with the apiserver's own Status object rather than leaving it Pending indefinitely.

Note on related open issues: 20261121T142446Z-eks-4-net-new-root-causes-admitted-pods.md describes admitted pods not running under the `eks` heading and does not carry either of the two check ids above; this issue is grounded in the Fargate-profile checks under the `ecr__eks__logs` heading and should be re-replayed alongside it.

Traces: ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch, ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery.

## Evidence
[error] ecr__eks__logs.eks-fargate-pod-logs-delivered-to-cloudwatch
  File "check_ecr__eks__logs_eks_fargate_pod_logs_delivered_to_cloudwatch.py", line 143, in run
    raise TimeoutError(f"pod {pod_name} leaving Pending: the run budget ended first; not graded")
  TimeoutError: pod <generated pod name> leaving Pending: the run budget ended first; not graded

[mismatch] ecr__eks__logs.eks-fargate-logging-without-execution-role-permission-no-delivery
  Error: Check timed out after 60.0s (same scenario; its control leg also requires a Fargate pod to reach Running).

Expected: with a Fargate profile ACTIVE for a selector on namespace `default`, a Pod created in `default` reaches status.phase Running (then Succeeded), and the marker it prints on stdout appears as a log event in the log group named in the aws-logging ConfigMap's cloudwatch_logs OUTPUT stanza, in a stream carrying the configured log_stream_prefix.
Actual: the Pod is accepted (201 from the served Kubernetes API) but status.phase is reported as "Pending" on every subsequent read and never changes; no log stream or log event is ever created in the configured log group. The preceding control-plane calls (CreateCluster, CreateFargateProfile, DescribeCluster) all answered 200 with ACTIVE resources, so the divergence is in the pod lifecycle served for a Fargate profile, not in the control-plane API or in the runner's TLS setup (the namespace, ConfigMap and Pod creations all succeeded over the advertised endpoint).

## Steps to Reproduce
1. CreateRole for an EKS cluster role (trusting eks.amazonaws.com, AmazonEKSClusterPolicy) and a Fargate pod execution role (trusting eks-fargate-pods.amazonaws.com, AmazonEKSFargatePodExecutionRolePolicy + an inline policy granting logs:CreateLogGroup/CreateLogStream/DescribeLogStreams/PutLogEvents).
2. CreateCluster with private subnets; wait for status ACTIVE.
3. CreateFargateProfile with that pod execution role and selectors=[{namespace: "default"}]; wait for status ACTIVE.
4. Build a kubeconfig from DescribeCluster (endpoint + base64-decoded certificateAuthority.data) and, through the served Kubernetes API, create namespace `aws-observability` and the `aws-logging` ConfigMap whose output.conf declares `[OUTPUT] Name cloudwatch_logs / Match kube.* / log_group_name <group> / log_stream_prefix from-fluent-bit- / auto_create_group true`.
5. Create a Pod in namespace `default` that prints a unique marker to stdout.
6. Poll GET /api/v1/namespaces/default/pods/<pod> for status.phase.
   -> the phase is "Pending" on every poll and never becomes Running/Succeeded; the Pod's marker never appears in <group>, which stays with no streams and no events. Expected: Running/Succeeded within the scheduling window, and the marker delivered into <group>.
