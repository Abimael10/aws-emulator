Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden
Issue-Key: eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden|run#57|AssertionError|assert status == 200, f"ConfigMap delete was not 200: {status} {deleted!r}"
Issue-Commit: 2c036246c52324f8f473f1b5e9b5314460be2843

# eks__iam__sts: DELETE of a ConfigMap on the served cluster Kubernetes API is answered 405 MethodNotAllowed instead of 200, so nothing created through the cluster API can be removed

> _Note: this issue was identified against the code base at commit `2c036246c52324f8f473f1b5e9b5314460be2843`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
Cause 1 of 1: the Kubernetes API the emulator serves at the address DescribeCluster publishes as cluster.endpoint routes no DELETE verb for a namespaced object. DELETE of a ConfigMap is answered HTTP 405 with reason MethodNotAllowed, so the object cannot be removed, the 'deleted object reads 404' leg and the namespace teardown that follows it are never reached, and an RBAC-denied delete could never be authorized either.

AWS:      recorded behavior only — Smithy (eks) models only Cluster$endpoint and Cluster$certificateAuthority; the API served there is the Kubernetes API server's. A real cluster answers DELETE /api/v1/namespaces/<ns>/configmaps/<name> with HTTP 200 and a Status object (kind 'Status', status 'Success'), after which GET of the same path answers 404 with reason 'NotFound' and the apiserver's message — fixed text `configmaps "<name>" not found`, where <name> is the object name from the request path. DELETE /api/v1/namespaces/<ns> likewise answers 200 with the Namespace object in phase 'Terminating', and the namespace afterwards reads 404 with `namespaces "<ns>" not found`. When the caller's RBAC does not permit the verb the answer is 403 with reason 'Forbidden' and the apiserver's forbidden sentence, never 405: authorization is only evaluated for a verb the server routes.
Emulator: HTTP 405 with body {'kind': 'Status', 'apiVersion': 'v1', 'metadata': {}, 'status': 'Failure', 'message': 'the server does not allow DELETE on /api/v1/namespaces/<ns>/configmaps/<name>', 'reason': 'MethodNotAllowed', 'details': None, 'code': 405}. The 'the server does not allow DELETE on <path>' sentence is the emulator's own wording for a verb it does not route; the namespace, ConfigMap name and path in it are this run's values. Everything the check asserts before this point passed, including the 409 Conflict on a stale resourceVersion and the 403 Forbidden sentence for the group-mapped principal, so the DELETE verb is the only remaining gap on this path.
Why:      a client cannot tear down what it created — kubectl delete, Helm uninstall, a controller's garbage collection and any test cleanup all fail with a transport-level 405 — and a deletion-authorization test can never observe a 403 Forbidden, because the verb is rejected before RBAC is consulted. Leftover objects also keep namespaces from terminating.
Fix:      the request routing and authorization path of the served cluster Kubernetes API (the EKS cluster data-plane handler's verb table): route DELETE for namespaces and namespaced resources, evaluate the caller's RBAC for it, and answer 200 with the Status/object on success or 403 Forbidden with the apiserver's sentence when the bound Role lacks the verb.
Note:     the open issue 20261122T113249Z-eks-3-net-new-root-causes-in-the.md states this same 405-on-DELETE class as its Cause 2, but it carries only the 'eks' kubectl replay keys (eks.eks-kubectl-namespace, eks.kubectl-denied-verb-...); it does not carry this check's replay key and does not cover it, and the behavior reproduces here through the direct REST path rather than kubectl. The resolved issue 20261117T115837Z-eks-iam-sts-the-served-cluster-kubernetes-api.md fixed the missing metadata.resourceVersion on this same check, which is why the check now gets as far as the DELETE.
Traces:   eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden.

## Evidence
Check eks__iam__sts.eks-configmap-stale-version-conflict-and-rbac-group-update-forbidden [mismatch].

Expected (AWS): DELETE <cluster.endpoint>/api/v1/namespaces/<ns>/configmaps/<name> -> HTTP 200 with a Status object (status 'Success'); the following GET of the same path -> 404, reason 'NotFound', message `configmaps "<name>" not found`; DELETE of the namespace -> 200 with phase 'Terminating', and the namespace then reads 404 with `namespaces "<ns>" not found`.

Actual (emulator):

  AssertionError: ConfigMap delete was not 200: 405 {'kind': 'Status', 'apiVersion': 'v1', 'metadata': {}, 'status': 'Failure', 'message': 'the server does not allow DELETE on /api/v1/namespaces/<ns>/configmaps/<name>', 'reason': 'MethodNotAllowed', 'details': None, 'code': 405}

(check_eks__iam__sts_eks_configmap_stale_version_conflict_and_rbac_group_update_forbidden.py line 186.) The namespace and ConfigMap name in the message are per-run values; the divergence is the 405/MethodNotAllowed status and reason where AWS answers 200/Success, and the emulator-authored 'the server does not allow DELETE on <path>' sentence.

## Steps to Reproduce
1. CreateCluster with two subnets, a cluster service role, endpointPublicAccess true and accessConfig {authenticationMode: API, bootstrapClusterCreatorAdminPermissions: true}; wait for status ACTIVE.
2. Build a Kubernetes client against cluster.endpoint and cluster.certificateAuthority.data, authenticating with the caller's STS-signed bearer token.
3. POST /api/v1/namespaces to create a namespace (201), then POST /api/v1/namespaces/<ns>/configmaps to create a ConfigMap (201).
4. DELETE /api/v1/namespaces/<ns>/configmaps/<name> -> answered 405 MethodNotAllowed with 'the server does not allow DELETE on <path>' instead of 200 with a Status 'Success' object.
5. (Unreached) GET the same path -> expect 404 `configmaps "<name>" not found`; DELETE /api/v1/namespaces/<ns> -> expect 200 with phase 'Terminating', then 404 `namespaces "<ns>" not found`.
