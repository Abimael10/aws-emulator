Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks__iam__sts.eks-kubernetes-api-namespace-via-sts-bearer-token
Issue-Key: eks__iam__sts.eks-kubernetes-api-namespace-via-sts-bearer-token|run#33|AssertionError|assert anon_version_status == 401, (
Issue-Commit: ce8395c3daba4e967b88f3e4355a15950c5b599c

# eks__iam__sts: the served cluster Kubernetes API accepts unauthenticated requests — anonymous GET /version is answered 200 instead of 401 Unauthorized (1 cause)

> _Note: this issue was identified against the code base at commit `ce8395c3daba4e967b88f3e4355a15950c5b599c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
Cause 1 of 1: the Kubernetes API server the emulator serves for an EKS cluster performs no authentication — a request carrying no Authorization header is answered 200.

AWS: recorded behavior only (the cluster's Kubernetes API server is not part of the EKS Smithy model; Smithy models only DescribeCluster -> Cluster.endpoint / Cluster.certificateAuthority.data, which is how a client reaches it). EKS disables anonymous authentication on the cluster API server entirely, so EVERY request — including the endpoint clients normally treat as public, GET /version — must be authenticated. A request with no (or an unverifiable) bearer token is refused HTTP 401 with a Kubernetes Status object: kind "Status", apiVersion "v1", status "Failure", reason "Unauthorized", message "Unauthorized", code 401. That message and reason are fixed text for this error class; there are no request- or state-derived parts. HTTP 403 with reason "Forbidden" is correct only where a request DID authenticate and the authorizer then denied it — an unauthenticated caller is never answered 2xx.

Emulator: an anonymous GET <cluster.endpoint>/version (no Authorization header, TLS validated against the cluster's own certificateAuthority.data) is answered HTTP 200 with a body of the form {"major":"1","minor":"<minor>","gitVersion":"v<major>.<minor>","platform":"emulator"}. No authentication filter runs in front of the served API listener, so the same path answers identically with and without an STS-derived bearer token.

Why: a client relying on AWS's behavior reads a 200 from the cluster API as proof that its IAM-derived (sts:GetCallerIdentity presigned) bearer token was accepted, and a 401 as the signal to re-mint or refresh that token. Against an always-open endpoint every authentication assertion passes vacuously: the access-entry / IAM authentication path is never actually exercised, credential-expiry and token-refresh handling is never driven, and a client whose token is wrong or expired still appears to succeed. It also means the emulated cluster API is reachable by any unauthenticated party.

Fix: the authentication filter in front of the served cluster Kubernetes API listener in the eks service crate's cluster data-plane component — it must verify the bearer token (the k8s-aws-v1. encoded presigned sts:GetCallerIdentity URL, scoped by the x-k8s-aws-id header) and reject requests with no or an unverifiable token with the 401 Status object above BEFORE routing to any resource handler, /version included.

Related open issue, which does NOT cover this check: 20261113T151622Z-eks-10-net-new-root-causes-in-the.md describes the same anonymous-access defect as its Cause 1, but it is keyed to the `eks.*` checks (eks.cluster-kubernetes-api-server-serves-advertised-version, eks.deploy-cluster-active-and-api-reachable, eks.deploy-eks-cluster-cloudformation-active) and carries none of this check's replay key, so eks__iam__sts.eks-kubernetes-api-namespace-via-sts-bearer-token is not covered by it. 20261117T115837Z-eks-iam-sts-the-served-cluster-kubernetes-api.md (missing metadata.resourceVersion) is a different defect of the same served API.

Traces: eks__iam__sts.eks-kubernetes-api-namespace-via-sts-bearer-token — the authenticated GET /version already returned 200, then the same GET with authenticate=False also returned 200 instead of 401.

## Evidence
Failing check eks__iam__sts.eks-kubernetes-api-namespace-via-sts-bearer-token:
  File check_eks__iam__sts_eks_kubernetes_api_namespace_via_sts_bearer_token.py, line 160, in run
    assert anon_version_status == 401, (
  AssertionError: unauthenticated GET /version was not refused with 401 (EKS disables anonymous auth): 200 {'major': '1', 'minor': '36', 'gitVersion': 'v1.36', 'platform': 'emulator'}

Expected (AWS): HTTP 401 with a Kubernetes Status body {"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Unauthorized","message":"Unauthorized","code":401} for a request to the cluster endpoint carrying no Authorization header.
Actual (emulator): HTTP 200 with the version document — identical to the response served to the authenticated request immediately before it, so the bearer token is not consulted at all. (The specific minor/gitVersion values above are this run's; the requirement is the 401 refusal, not those numbers.)

## Steps to Reproduce
1. eks.create_cluster(name=<n>, roleArn=<cluster role>, resourcesVpcConfig={subnetIds:[...], endpointPublicAccess:true}, accessConfig={authenticationMode:'API_AND_CONFIG_MAP', bootstrapClusterCreatorAdminPermissions:true}) and wait for status ACTIVE.
2. DescribeCluster -> take cluster.endpoint and base64-decode certificateAuthority.data to a PEM CA bundle.
3. As the cluster creator, GET <endpoint>/version with header 'Authorization: Bearer k8s-aws-v1.<base64url of a presigned sts:GetCallerIdentity URL carrying x-k8s-aws-id: <n>>' -> 200 with the version object (this part passes).
4. Repeat the SAME GET <endpoint>/version with NO Authorization header -> emulator answers 200 with the version object; AWS answers 401 with reason 'Unauthorized' and message 'Unauthorized'.
