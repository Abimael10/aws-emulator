Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.hpa-min-replicas-above-max-replicas-rejected eks.hpa-scales-out-under-cpu-load-and-back-in-when-it-stops eks.hpa-target-without-a-cpu-request-is-blocked-from-scaling eks.nodegroup-full-lifecycle-transitions eks.e2e-cluster-nodegroup-workload-scale-teardown eks.e2e-certificate-authority-issue-activate-disable-delete eks.addon-configuration-values-reach-the-in-cluster-workload
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks.hpa-min-replicas-above-max-replicas-rejected|_sh#8|AssertionError|raise AssertionError(
Issue-Key: eks.hpa-scales-out-under-cpu-load-and-back-in-when-it-stops|_sh#7|AssertionError|raise AssertionError(
Issue-Key: eks.hpa-target-without-a-cpu-request-is-blocked-from-scaling|_sh#6|AssertionError|raise AssertionError(
Issue-Key: eks.nodegroup-full-lifecycle-transitions|_wait_deleted#36|Timeout|return
Issue-Key: eks.e2e-cluster-nodegroup-workload-scale-teardown|_wait_nodegroup_deleted#21|Timeout|return
Issue-Key: eks.e2e-certificate-authority-issue-activate-disable-delete|_wait_ca_gone#21|Timeout|return
Issue-Key: eks.addon-configuration-values-reach-the-in-cluster-workload|_wait_for#25|Timeout|time.sleep(interval_s)
Issue-Commit: ed9ba62862d633303704a0a66a6d19ea681ad35f

# eks: the cluster's Kubernetes data plane serves no HorizontalPodAutoscaler and no OpenAPI document a stock kubectl can read, and four long end-to-end checks still exhaust their budget in their last wait

> _Re-filed from `20261104T105252Z-eks-2-net-new-root-causes-the-metrics.md`, whose other causes are closed: the metrics-server configuration schema now declares the add-on's keys where its own document declares them with the replica count its build ships as that key's `default`, an install's `configurationValues` replica count reaches the Deployment the cluster runs, and `DisassociateIdentityProviderConfig` answers a configuration the cluster does not hold with the service's generic not-found payload. These keys are the ones that remain, re-filed so they are not recorded as working._

## Operation
Kubernetes data plane (`apis/autoscaling/v2`, `/openapi/v2`), DeleteNodegroup, DeleteCertificateAuthority

## Description

Cause 1 of 2: a stock `kubectl` cannot send a manifest to a cluster's API server, and the API server serves no HorizontalPodAutoscaler.
  AWS: a cluster's API server publishes its OpenAPI surface in the encodings a client negotiates — including the Protocol Buffers encoding `kubectl` asks the `/openapi/v2` document for — and serves `autoscaling/v2`, so `kubectl apply` of a Deployment and a HorizontalPodAutoscaler is admitted, an HPA whose `minReplicas` is above its `maxReplicas` is refused by admission, an HPA whose target declares no CPU request reports that it cannot scale, and an HPA scales its target out while the pods' CPU is above the target utilisation and back in when it stops.
  Emulator: every `kubectl apply` of these three checks fails before anything is sent — `error validating data: the server was unable to respond with a content type that the client supports` — because the API server answers the Protocol Buffers `Accept` of the OpenAPI v2 request `406` (`state/kubernetes_openapi.rs` writes JSON only) and serves neither the legacy `/swagger-2.0.0.pb-v1` path the client falls back to; and `autoscaling/v2` is not a group version the routing serves at all (`state/kubernetes_api.rs`), so no HorizontalPodAutoscaler can be created, read or acted on.
  Why: the checks that drive an HPA are `kubectl`-driven end to end, so a client that cannot validate a manifest cannot reach any of this cluster's workload API; and an HPA is how a Kubernetes client scales a workload.
  Note for whoever takes this: the SCALE-OUT behaviour needs a source of pod CPU utilisation. The pods of this emulator are records — nothing runs in them — so a `currentCPUUtilizationPercentage` and the replica moves derived from it cannot be computed from state as it stands; that part may need a decision about what the cluster measures, rather than code alone. The admission rule (`minReplicas` above `maxReplicas`) and the "target declares no CPU request" status are derivable from the stored objects.
  Traces: eks.hpa-min-replicas-above-max-replicas-rejected, eks.hpa-scales-out-under-cpu-load-and-back-in-when-it-stops, eks.hpa-target-without-a-cpu-request-is-blocked-from-scaling.

Cause 2 of 2: four long end-to-end checks exhaust the suite's 60-second budget in their LAST wait.
  AWS: each flow settles and the check then asserts the settled representation.
  Emulator: every resource these checks poll does settle and does leave the store — each move is covered end to end by the crate's own suites (`tests/all/nodegroup_lifecycle.rs`, `tests/all/child_resource_deletion.rs`, `tests/all/cluster_deletion_window.rs`, `tests/all/certificate_authority_deletion.rs`, `tests/all/addon_configuration_replicas.rs`) — and these four checks now get PAST every wait they used to fail in; what remains is that the whole check does not finish inside the 60 seconds the suite gives it, and the interrupt lands in its last teardown wait (`_wait_deleted`, `_wait_nodegroup_deleted`, `_wait_ca_gone`) or, for the add-on check, in the wait that follows the install. No answer of this service is known to be wrong in them.
  Why: a flow that cannot finish inside the budget is reported as a timeout, so the assertions at its end never run.
  Note for whoever takes this: cutting the accepted-deletion span (`CLUSTER_DELETION_DELAY`, 3 s, the only owed transition of this service longer than `CLUSTER_ACTIVATION_DELAY`) to the span every other move uses was measured against these checks and changed none of their verdicts, so the time is going somewhere else — worth measuring where before changing a span.
  Traces: eks.nodegroup-full-lifecycle-transitions, eks.e2e-cluster-nodegroup-workload-scale-teardown, eks.e2e-certificate-authority-issue-activate-disable-delete, eks.addon-configuration-values-reach-the-in-cluster-workload.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout.

Cause 1: all three `eks.hpa-*` checks report `DATAPLANE_UNIMPLEMENTED: command failed (rc=1): kubectl apply -n <namespace> -f - stderr: error: error validating "STDIN": error validating data: the server was unable to respond with a content type that the client supports`.

Cause 2: `Check timed out after 60.0s`, with the interrupt in `_wait_deleted`, `_wait_nodegroup_deleted`, `_wait_ca_gone` and `_wait_for` respectively — each AFTER the wait the same check used to fail in.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. `kubectl --kubeconfig <a cluster's kubeconfig> apply -f -` with any Deployment manifest: it fails validating STDIN instead of creating the object. `GET /openapi/v2` with `Accept: application/com.github.proto-openapi.spec.v2@v1.0+protobuf` answers 406, and `GET /apis/autoscaling/v2` answers 404.
3. Replay the trace ids above and read where each timed-out check was interrupted.
