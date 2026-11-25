Issue-Source: trace-sweep-divergence
Issue-Trace: api-image-push-via-layer-upload
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: api-image-push-via-layer-upload api-image-scanning-and-findings api-immutable-tag-conflict-and-image-deletion api-lifecycle-policy-preview-and-apply api-registry-policy-and-scanning-configuration api-repository-lifecycle-with-policy-and-tags ecr-duplicate-push-and-tag-based-storage-class-update ecr-image-and-repo-not-found-errors ecr-image-lookup-errors ecr-image-push-and-inspect ecr-immutable-tag-conflict ecr-layer-upload-lifecycle-and-errors ecr-multi-image-isolation-in-shared-repository ecr-push-multi-layer-image ecr-put-image-missing-layers-error ecr-put-image-missing-layers-error-then-recover ecr-repository-create-verify-delete ecr-repository-lifecycle-and-errors ecr-batch-diag-image-uri-passed-as-repository-name ecr-ecracc-cross-account-registry-id-reads ecr-repository-pagination-and-scoping-conflict
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr|CreateRepository|2xx->2xx|$.repository.repositoryUri
Issue-Key: ecr|DeleteRepository|2xx->2xx|$.repository.repositoryUri
Issue-Key: ecr|DescribeRepositories|2xx->2xx|$.repositories[*].repositoryUri
Issue-Commit: a1915270e40223bfb23a5a7de86fe74aa15a3df0

# ECR announces a repositoryUri host the emulator does not serve, on every Repository-shaped response

> _Note: this issue was identified against the code base at commit `a1915270e40223bfb23a5a7de86fe74aa15a3df0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
3 net-new causes, one per operation that serializes the ECR Repository shape. All three are the same fault in the `repositoryUri` the emulator puts in a Repository body: the host it announces is not an address the emulator serves or that resolves to it, so the URI the API hands a client for push/pull points somewhere other than the registry that answered the call. The replay verdict for all three is `host_not_own` (the value's authority is not the emulator's own), affecting 18 fixtures for CreateRepository, 18 for DeleteRepository and 5 for DescribeRepositories.

Cause 1 of 3: CreateRepository.
AWS: Smithy: ecr Repository.repositoryUri (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." AWS forms it as <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>, where registryId is Repository.registryId (pattern ^[0-9]{12}$) and region is the service region; the fixed parts are '.dkr.ecr.' and '.amazonaws.com' and the path is exactly Repository.repositoryName. The defining property of that host on AWS is that it IS the registry serving the Docker Registry v2 API for that account — the URI is actionable by the client that received it.
Emulator: CreateRepository answers 200 with a repositoryUri whose host is the literal public AWS registry DNS name for the account/region, which the emulator neither serves nor resolves in its own environment. The host is not the authority the request was made to, so the replay flags `$.repository.repositoryUri: host_not_own`.
Why: a client that does what the field documents — `docker login`/`docker push`/`docker pull` against repositoryUri, or deriving an ECS task-definition image, a Lambda ImageUri or a buildspec target from it — is sent off the emulator entirely: the name either fails to resolve or reaches real AWS, where the repository does not exist. Every image-push flow built from the API's own answer breaks even though the control-plane call succeeded.
Fix: the ECR Repository serializer / the component that computes repositoryUri from state (registry id, region, repository name) must emit a registry authority the emulator actually serves — i.e. publish the registry-DNS form AND make that host reachable on the emulator (host alias / served registry endpoint), instead of emitting an authority that is neither served nor resolvable. Related open issues: 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md (behavioral; demands the <registryId>.dkr.ecr.<region>.amazonaws.com shape and explicitly notes the announced host must also be served locally), 20261111T010150Z-ecr-ecs-logs-ecr-pull-through-cache-repository-uri-is-not.md and 20261115T024314Z-batch-ecr-lambda-logs-ecr-repositoryuri-is-not-region-qualified-so-the.md. None of those carries this wire replay key; a fix must satisfy both the shape requirement and this reachability requirement.
Traces: api-image-push-via-layer-upload#setup#1 (+17 more, incl. ecr-image-push-and-inspect#task#1, ecr-repository-create-verify-delete#task#1).

Cause 2 of 3: DeleteRepository.
AWS: same modeled field — DeleteRepository returns the deleted Repository, including repositoryUri in the registry-DNS form described above.
Emulator: DeleteRepository answers 200 with the same unserved authority in $.repository.repositoryUri (`host_not_own`).
Why: a teardown/confirmation step that re-checks or logs the URI it was handed records a registry that is not the one it was talking to; tooling that derives cleanup actions from the response targets the wrong registry.
Fix: same Repository serializer path as Cause 1 — the URI must be computed once, from state, into an authority the emulator serves.
Traces: api-image-push-via-layer-upload#teardown#14 (+17 more, incl. ecr-repository-create-verify-delete#task#4).

Cause 3 of 3: DescribeRepositories.
AWS: same modeled field, per element of RepositoryList.
Emulator: DescribeRepositories answers 200 with $.repositories[0].repositoryUri carrying the unserved authority (`host_not_own`).
Why: the read path is what CI and container tooling actually query to discover where to push; the discovered endpoint is unusable against the emulator.
Fix: same Repository serializer path as Cause 1.
Traces: api-repository-lifecycle-with-policy-and-tags#task#2 (+4 more, incl. ecr-repository-create-verify-delete#task#3, ecr-ecracc-cross-account-registry-id-reads#task#9).

## Evidence
Cause 1 — CreateRepository (fixture api-image-push-via-layer-upload, step setup#1): expected HTTP 200 with $.repository.repositoryUri whose authority is the serving registry; actual HTTP 200 with verdict `$.repository.repositoryUri: host_not_own` — the emulator emitted '<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>' (recorded run: '000000000000.dkr.ecr.us-west-2.amazonaws.com/sim2real-probe-ecr-ecr-repo-bab899a5'), a host the emulator does not serve. The 12-digit id, region and repository name are values from state/the request and are not themselves the requirement; the requirement is that the announced authority be one the emulator answers on. 18 fixture steps in the cluster.
Cause 2 — DeleteRepository (api-image-push-via-layer-upload#teardown#14): expected 200, actual 200, `$.repository.repositoryUri: host_not_own`; 18 fixture steps in the cluster.
Cause 3 — DescribeRepositories (api-repository-lifecycle-with-policy-and-tags#task#2): expected 200, actual 200, `$.repositories[0].repositoryUri: host_not_own`; 5 fixture steps in the cluster.
No status or error-code divergence in any of the three — the bodies are wrong, not the verdicts.

## Steps to Reproduce
1. Point an AWS client at the emulator's ECR endpoint and call CreateRepository with a valid repositoryName.
2. Read $.repository.repositoryUri and split it on the first '/': the authority is the public AWS ECR registry DNS name, not the endpoint the call was made to.
3. Try to use it as documented (resolve it / `docker login` against it): it does not reach the emulator that just created the repository.
4. Call DescribeRepositories for that name and DeleteRepository on it — both return the same unserved authority.
5. Replay fixture api-image-push-via-layer-upload step setup#1 (CreateRepository), step teardown#14 (DeleteRepository) and fixture api-repository-lifecycle-with-policy-and-tags step task#2 (DescribeRepositories).
