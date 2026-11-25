Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__eks__logs.deploy-eks-nodegroup-with-ecr-readonly-policy
Issue-Key: ecr__eks__logs.deploy-eks-nodegroup-with-ecr-readonly-policy|run#29|AssertionError|raise AssertionError(f"kubectl apply of test pod failed: {apply.stderr[-500:]}")
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# eks: the served cluster Kubernetes API exposes no OpenAPI discovery document, so every `kubectl apply` fails client-side validation

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster (served Kubernetes API: GET /openapi/v2, GET /openapi/v3)

## Description
Cause 1 of 1: the Kubernetes API served for an EKS cluster answers the OpenAPI schema routes as a missing resource, so no manifest can be applied with validation on.

AWS: recorded behavior only (this is the cluster's Kubernetes surface, not a modelled AWS API). The recording shows: the endpoint EKS publishes for a cluster serves the full Kubernetes discovery surface, including the OpenAPI schema documents kubectl downloads before an apply — `GET /openapi/v2` (and the aggregated `GET /openapi/v3` plus its group index) returning the schema document with a 200. `kubectl apply -f -` of a valid core/v1 Pod manifest therefore succeeds (`pod/<name> created`) with kubectl's default `--validate=true`, and the pod then runs to completion pulling its image from the private ECR repository.

Emulator: kubectl's schema download is answered as a not-found resource, so the apply never issues the create: `error: error validating "STDIN": error validating data: failed to download openapi: the server could not find the requested resource; if you choose to ignore these errors, turn validation off with --validate=false`. Nothing is created, and the node-group/ECR-pull leg the check exists to exercise is never reached.

Why: kubectl validates by default, so a client cannot create a single Kubernetes object against an emulated cluster without being told to disable validation — which real AWS never requires. Every tool that builds on client-side schema validation or on server-side schema discovery (kubectl apply/diff/explain, helm, controller-runtime clients that read the OpenAPI document) fails on the first call, and the failure is reported as a validation error rather than as an unimplemented endpoint, so it looks like the user's manifest is wrong.

Fix: the Kubernetes API surface served for an EKS cluster in the eks crate — serve the OpenAPI discovery routes (`/openapi/v2`, `/openapi/v3` and the `/openapi/v3` group index) alongside the existing `/api` and `/apis` discovery, returning the schema document for the groups the emulator serves.

Note: open issues 20261113T151622Z-eks-10-net-new-root-causes-in-the.md and 20261114T131945Z-eks-2-net-new-root-causes-in-the--r3.md report other gaps in this same served Kubernetes API (discovery objects, create bodies refused 415); neither covers the OpenAPI schema routes, and neither carries this check's replay key.

Traces: ecr__eks__logs.deploy-eks-nodegroup-with-ecr-readonly-policy.

## Evidence
Check ecr__eks__logs.deploy-eks-nodegroup-with-ecr-readonly-policy, check line 249:
`AssertionError: kubectl apply of test pod failed: error: error validating "STDIN": error validating data: failed to download openapi: the server could not find the requested resource; if you choose to ignore these errors, turn validation off with --validate=false`

kubectl reached the served endpoint and was answered; the answer to its OpenAPI request was a missing-resource response. Expected: `pod/<name> created` and the pod running to completion from the ECR pull-through cache repository.

## Steps to Reproduce
1. Deploy the check's CloudFormation stack: an EKS cluster plus a managed node group whose node role carries AmazonEC2ContainerRegistryReadOnly, and an ECR pull-through cache rule.
2. Build a kubeconfig for the cluster from DescribeCluster (endpoint + certificateAuthority) and run `kubectl apply -f -` with a valid core/v1 Pod manifest whose image is the private pull-through-cache repository URI.
3. The emulator answers kubectl's OpenAPI schema request as a missing resource and the apply aborts with the validation error above; AWS serves the schema and the pod is created and pulls its image.
4. Direct probe: `GET /openapi/v2` and `GET /openapi/v3` on the published cluster endpoint — AWS returns the schema document, the emulator does not serve the route.
