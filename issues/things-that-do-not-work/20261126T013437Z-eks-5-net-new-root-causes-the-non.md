Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.identity-provider-config-lifecycle eks.kubectl-applied-deployment-pod-runs-and-serves-its-configmap-through-a-service eks.eks-ebs-csi-volume-data-survives-a-pod-reschedule eks.delete-certificate-authority-in-use-blocked eks.eks-coredns-resolves-a-service-to-the-clusterip-the-api-reports eks.eks-deployment-pod-is-reachable-through-a-cluster-service eks.hpa-min-replicas-above-max-replicas-rejected eks.k8s-configmap-mounted-value-readable-inside-the-pod eks.k8s-deployment-apply-runs-pods-on-real-nodes eks.k8s-deployment-scale-converges-to-the-requested-replicas eks.k8s-rbac-get-only-role-rejects-deleting-a-pod eks.k8s-service-clusterip-load-balances-across-pods
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks.identity-provider-config-lifecycle|run#21|AssertionError|assert non_tls_message == expected, f"expected message {expected!r}, got {non_tls_message!r}"
Issue-Key: eks.kubectl-applied-deployment-pod-runs-and-serves-its-configmap-through-a-service|run#29|AssertionError|assert status["phase"] == "Running", (
Issue-Key: eks.eks-ebs-csi-volume-data-survives-a-pod-reschedule|_wait_for#20|TimeoutError|raise TimeoutError(f"{desc}: the run budget ended first; not graded")
Issue-Key: eks.delete-certificate-authority-in-use-blocked|_poll_until#19|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last observed={last!r}")
Issue-Key: eks.eks-coredns-resolves-a-service-to-the-clusterip-the-api-reports|_wait_for#12|Timeout|val = fn()
Issue-Key: eks.eks-deployment-pod-is-reachable-through-a-cluster-service|_wait_for#11|Timeout|val = fn()
Issue-Key: eks.hpa-min-replicas-above-max-replicas-rejected|_wait_for#9|Timeout|val = fn()
Issue-Key: eks.k8s-configmap-mounted-value-readable-inside-the-pod|_wait_for#7|Timeout|val = fn()
Issue-Key: eks.k8s-deployment-apply-runs-pods-on-real-nodes|_run#9|Timeout|return subprocess.run(
Issue-Key: eks.k8s-deployment-scale-converges-to-the-requested-replicas|_wait_for#8|Timeout|val = fn()
Issue-Key: eks.k8s-rbac-get-only-role-rejects-deleting-a-pod|_wait_for#6|Timeout|val = fn()
Issue-Key: eks.k8s-service-clusterip-load-balances-across-pods|_wait_for#10|Timeout|val = fn()
Issue-Commit: bf205090aef11dede75ce528b7b57e0b8feca92b

# eks: 5 net-new root causes — the non-TLS issuerUrl refusal is worded by the emulator, an admitted Deployment pod ends 'Failed' instead of Running, the EBS CSI controller never becomes Ready, and ten control-plane/data-plane flows never settle inside their budget

> _Note: this issue was identified against the code base at commit `bf205090aef11dede75ce528b7b57e0b8feca92b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateIdentityProviderConfig, DeleteCertificateAuthority, Kubernetes data plane (pods, Deployments, Services, RBAC, HorizontalPodAutoscaler)

## Description
Five distinct root causes, all in the eks service, found replaying the behavioral suite against the emulator built from this checkout. None of them is carried by an open issue's replay key (the suite reported none of these checks as OWNED or ALREADY FILED).

Cause 1 of 5: AssociateIdentityProviderConfig refuses a non-https oidc.issuerUrl with the emulator's own wording instead of AWS's message.
  AWS: Smithy: AssociateIdentityProviderConfig — POST /clusters/{clusterName}/identity-provider-configs/associate, input oidc -> OidcIdentityProviderConfigRequest (required), modeled error InvalidParameterException (httpError 400). The model does not state the scheme rule, so for the message this is recorded behavior only: AWS refuses an oidc.issuerUrl whose scheme is not https with InvalidParameterException (HTTP 400) and the message "IssuerURL must use https:// scheme." — that string is ENTIRELY fixed text; it interpolates nothing from the request (no URL, no config name, no cluster name).
  Emulator: the error code is right (InvalidParameterException) but the message is "oidc.issuerUrl must begin with https://, got http://sim2real.example.com" — fixed text "oidc.issuerUrl must begin with https://, got " followed by the request's oidc.issuerUrl value (the host in that example is this run's fixture value, not part of the requirement).
  Why: the message is part of the error contract. A client or test that matches AWS's sentence — or a human reading it — sees a different diagnosis string for a refusal AWS words one fixed way, and a message that interpolates the request value cannot be matched at all.
  Fix: the issuer-URL validation in this operation's handler, src/ops/associate_identity_provider_config.rs (the refusal path, not the Update/state write, which the same check accepts). Everything else this check asserts about the operation passed: the absent-cluster ResourceNotFoundException and its "No cluster found for name: <name>." message, the absent-config DisassociateIdentityProviderConfig refusal, the post-refusal cluster status ACTIVE and the empty ListIdentityProviderConfigs.
  Traces: eks.identity-provider-config-lifecycle

Cause 2 of 5: a Deployment pod the cluster admits and schedules never starts its containers — it reports status.phase 'Failed' instead of 'Running'.
  AWS: recorded behavior only (Kubernetes data plane, not the Smithy surface): a pod created by a Deployment on an EKS cluster with a Ready node is scheduled, pulls and starts its container, and reports status.phase 'Running' with conditions PodScheduled=True, Initialized=True, ContainersReady=True and Ready=True; the container then serves (here, the value mounted from its ConfigMap through a ClusterIP Service).
  Emulator: the pod gets PodScheduled=True and Initialized=True, but ContainersReady=False and Ready=False with reason 'ContainersNotReady' and message "containers with unready status: [<container name>]" (the container name comes from the applied manifest; the timestamps are per-run), and status.phase is 'Failed'. Nothing the pod was supposed to serve is ever reachable.
  Why: a client that applies a workload and waits for it to become Running waits forever and then sees a terminal 'Failed' pod it never caused; every behavior that depends on a running container (serving a ConfigMap value, answering through a Service, writing logs) is unreachable.
  Fix: the Kubernetes data-plane pod lifecycle in this crate's state (src/state/kubernetes_api.rs and the pod/container state it owns) — whatever advances an admitted pod from Initialized to ContainersReady/Running, and the phase it reports when the containers are not started.
  Note: open issues 20261121T142446Z-eks-4-net-new-root-causes-admitted-pods.md and 20261125T195450Z-ecr-eks-logs-a-pod-on-an-ec2.md describe nearby symptoms in other checks; neither carries this check's replay key, and this check's pod is a plain kubectl-applied Deployment on the shared cluster.
  Traces: eks.kubectl-applied-deployment-pod-runs-and-serves-its-configmap-through-a-service

Cause 3 of 5: the aws-ebs-csi-driver add-on's controller never becomes Ready, so no EBS-backed volume can ever be used.
  AWS: recorded behavior only: installing the aws-ebs-csi-driver add-on on an ACTIVE cluster brings up an ebs-csi-controller whose pods report Ready, after which a PersistentVolumeClaim binds and a pod's data on that volume survives a reschedule.
  Emulator: the wait for 'ebs-csi-controller ready' is never satisfied — the check is interrupted in that wait (TimeoutError: "ebs-csi-controller ready: the run budget ended first; not graded"), so nothing after the add-on install is ever exercised.
  Why: a client cannot get persistent storage on a cluster at all: the CSI controller is the precondition for every PVC, PV bind and volume reattach.
  Fix: the add-on install path for aws-ebs-csi-driver and the controller workload it is supposed to materialise in the cluster's Kubernetes data plane (this crate's add-on handling plus src/state/kubernetes_api.rs pod readiness — the same readiness gap as Cause 2 may be the reason the controller pods never report Ready; Hypothesis: one fix may close both, but they are listed separately because this one blocks the add-on's own object, not just a user pod).
  Traces: eks.eks-ebs-csi-volume-data-survives-a-pod-reschedule

Cause 4 of 5: the DeleteCertificateAuthority flow's own control-plane steps consume the entire run budget, so the successor CertificateAuthorityUpdate is never observed.
  AWS: recorded behavior only: on a fresh cluster exactly one certificate authority is IN_USE; DeleteCertificateAuthority on it is refused with ResourceInUseException and the message "Cannot delete the active certificate authority. Activate a different CA first." (entirely fixed text), the CA stays listed and IN_USE, and a freshly appended successor CA starts NOT_USED and its CertificateAuthorityUpdate then settles to Successful (or Failed), after which the successor can be deleted.
  Emulator: the refusal itself is correct — code, message, and the CA remaining listed and IN_USE all matched, as did the successor starting NOT_USED. What fails is the wait that follows: DescribeUpdate for the successor's update id is given a deadline of 0.0 s and is never called even once (AssertionError: "condition not met within 0.0s; last observed=None"), i.e. the run budget was already exhausted by the steps BEFORE it (CreateCluster to ACTIVE, ListCertificateAuthorities, the refused delete, CreateCertificateAuthority). Hypothesis: one or more of those control-plane moves is served far slower than AWS answers it, or an owed asynchronous transition is held for a span that does not fit a graded run; the replay output does not say which, so measure where the time goes before changing any span.
  Why: a client cannot complete a CA rotation: the positive half of the contract (a not-yet-activated successor CA can be deleted once its update settles) is never reachable, so the only graded part of the flow is the refusal.
  Fix: the certificate-authority update machinery in this crate (CreateCertificateAuthority/DeleteCertificateAuthority handlers and the owed CertificateAuthorityUpdate transition in this crate's state) and the time it holds before a read can settle it.
  Traces: eks.delete-certificate-authority-in-use-blocked

Cause 5 of 5: eight Kubernetes data-plane checks never finish inside the suite's 60-second budget — every one of them is interrupted with "Check timed out after 60.0s" and nothing it asserts is ever graded.
  AWS: recorded behavior only: each of these flows settles against a real cluster — CoreDNS resolves a Service name to the ClusterIP the API reports; a Deployment's pod is reachable through a ClusterIP Service and that Service load-balances across its pods; a mounted ConfigMap value is readable inside the pod; `kubectl apply` of a Deployment runs pods on real nodes and a scale converges to the requested replica count; a get-only RBAC Role is refused a pod delete; and an HPA whose minReplicas exceeds its maxReplicas is refused by admission.
  Emulator: all eight are interrupted at the budget with no assertion reached. The replay output does not report WHERE each interrupt landed, so the common factor cannot be read off it directly; Hypothesis: these are exactly the checks that wait for a pod to become Running or to serve traffic, which Cause 2 shows never happens (the pod reports 'Failed' with ContainersNotReady instead), so each check loops in that wait until the budget ends. eks.hpa-min-replicas-above-max-replicas-rejected additionally appears in the Issue-Trace-Ids of open issue 20261124T090000Z-eks-the-kubernetes-data-plane-serves-no-hpa-and-four-checks-still-time-out.md, but under a different replay key (that issue records an AssertionError from `kubectl apply` validation; this run times out instead), so that issue does not carry it and the suite did not report it as owned.
  Why: the entire Kubernetes workload surface of a served cluster is unusable from a client's point of view: nothing it applies converges, nothing it exposes answers, and the admission refusals it relies on are never produced.
  Fix: the served cluster Kubernetes data plane in this crate's state (src/state/kubernetes_api.rs and the pod/Service/endpoint/HPA objects it serves), starting from the pod readiness gap in Cause 2; whoever takes this should first record where each of these eight checks is interrupted, since the budget interrupt hides the failing wait.
  Traces: eks.eks-coredns-resolves-a-service-to-the-clusterip-the-api-reports, eks.eks-deployment-pod-is-reachable-through-a-cluster-service, eks.hpa-min-replicas-above-max-replicas-rejected, eks.k8s-configmap-mounted-value-readable-inside-the-pod, eks.k8s-deployment-apply-runs-pods-on-real-nodes, eks.k8s-deployment-scale-converges-to-the-requested-replicas, eks.k8s-rbac-get-only-role-rejects-deleting-a-pod, eks.k8s-service-clusterip-load-balances-across-pods

## Evidence
Behavioral suite simparity-behavioral-aws-v1, replayed against the emulator built from this checkout. Overall 297/364 passed; eks 270/318 (85%), 48 failed, of which 12 are not already filed/owned and are clustered here.

Cause 1 — eks.identity-provider-config-lifecycle:
  check_eks_identity_provider_config_lifecycle.py:210
  AssertionError: expected message 'IssuerURL must use https:// scheme.', got 'oidc.issuerUrl must begin with https://, got http://sim2real.example.com'
  (the error CODE matched: InvalidParameterException; the request was associate_identity_provider_config(clusterName=<active cluster>, oidc={identityProviderConfigName=<unique>, issuerUrl='http://sim2real.example.com', clientId='sim2real'}). Expected: HTTP 400 InvalidParameterException, message exactly "IssuerURL must use https:// scheme." — all fixed text. Actual: HTTP 400 InvalidParameterException, message "oidc.issuerUrl must begin with https://, got " + the request's issuerUrl.)

Cause 2 — eks.kubectl-applied-deployment-pod-runs-and-serves-its-configmap-through-a-service:
  check_...:477
  AssertionError: the applied Deployment's pod never reached Running: phase='Failed' conditions=[{'status': 'True', 'type': 'PodScheduled'}, {'status': 'True', 'type': 'Initialized'}, {'message': 'containers with unready status: [web]', 'reason': 'ContainersNotReady', 'status': 'False', 'type': 'ContainersReady'}, {'message': 'containers with unready status: [web]', 'reason': 'ContainersNotReady', 'status': 'False', 'type': 'Ready'}]
  ('web' is the container name from the applied manifest; the lastTransitionTime values are per-run and not part of the requirement.) Expected: status.phase 'Running' with ContainersReady=True and Ready=True.

Cause 3 — eks.eks-ebs-csi-volume-data-survives-a-pod-reschedule:
  check_...:594 -> _wait_for at :83
  TimeoutError: ebs-csi-controller ready: the run budget ended first; not graded

Cause 4 — eks.delete-certificate-authority-in-use-blocked:
  check_...:52 -> _poll_until at :149
  AssertionError: condition not met within 0.0s; last observed=None
  (the poll is DescribeUpdate(name=<cluster>, updateId=<successor CA update>) waiting for status in (Successful, Failed); last observed=None means it was never called. The ResourceInUseException refusal and its message "Cannot delete the active certificate authority. Activate a different CA first.", the CA remaining IN_USE, and the successor starting NOT_USED all passed before it.)

Cause 5 — all eight checks report exactly:
  Error: Check timed out after 60.0s
  with no assertion reached:
  eks.eks-coredns-resolves-a-service-to-the-clusterip-the-api-reports
  eks.eks-deployment-pod-is-reachable-through-a-cluster-service
  eks.hpa-min-replicas-above-max-replicas-rejected
  eks.k8s-configmap-mounted-value-readable-inside-the-pod
  eks.k8s-deployment-apply-runs-pods-on-real-nodes
  eks.k8s-deployment-scale-converges-to-the-requested-replicas
  eks.k8s-rbac-get-only-role-rejects-deleting-a-pod
  eks.k8s-service-clusterip-load-balances-across-pods

## Steps to Reproduce
1. Build and boot the emulator from this checkout; replay the behavioral suite (simparity-behavioral-aws-v1).
2. Cause 1: create an ACTIVE cluster, then call AssociateIdentityProviderConfig with oidc.issuerUrl='http://<any-host>' (any non-https scheme). Observe HTTP 400 InvalidParameterException with message 'oidc.issuerUrl must begin with https://, got http://<that host>' instead of AWS's fixed 'IssuerURL must use https:// scheme.'.
3. Cause 2: on a cluster with a Ready node, `kubectl apply` a Deployment with one container plus a ConfigMap and a ClusterIP Service, then read the pod. Observe status.phase 'Failed' with ContainersReady=False / Ready=False, reason 'ContainersNotReady', instead of 'Running'.
4. Cause 3: install the aws-ebs-csi-driver add-on on an ACTIVE cluster and wait for the ebs-csi-controller pods to report Ready; they never do.
5. Cause 4: create a cluster, list its certificate authorities (one IN_USE), call DeleteCertificateAuthority on it (correctly refused), then CreateCertificateAuthority and poll DescribeUpdate for the returned update id. Time the whole flow: the steps before that poll consume the entire graded budget, so the successor's CertificateAuthorityUpdate is never observed settling.
6. Cause 5: replay the eight check ids listed in that cause and record where each is interrupted (the suite reports only the 60 s budget interrupt, not the failing wait).
