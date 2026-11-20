Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull
Issue-Key: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task|_pull_through_cache_repo#4|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Key: ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull|_pull_through_cache_repo#3|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# ecr__ecs__logs: CreateRepository returns a repositoryUri that is the emulator's bare host:port, not <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
One net-new root cause for this service pairing (related open issues exist for other service pairings — see below — but none carries these two checks' replay keys).

Cause 1 of 1: ECR CreateRepository mints Repository.repositoryUri from the emulator's own listening socket instead of from the account's ECR registry DNS name, so the value has no registry-id label, no `dkr.ecr` labels and no region.

AWS: Smithy (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921) — CreateRepository -> CreateRepositoryResponse.repository: Repository; member Repository.repositoryUri is documented as "The URI for the repository. You can use this URI for container image push and pull operations." The model gives no pattern, so the required FORM is AWS's documented registry URI (recorded behavior and AWS ECR docs): `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`, where registryId is the caller's account id (or the registryId of the repository), region is the request's region, and repositoryName is the name as created — including a pull-through-cache prefix path such as `<prefix>/docker/library/busybox`. The same value is echoed by DescribeRepositories/DeleteRepository and by Repository-shaped responses generally. The literal text `.dkr.ecr.` and the `.amazonaws.com` suffix are fixed; registryId, region and repositoryName are values from the request and from state. Status is 200 on both AWS and the emulator — the divergence is in the payload, not the status or an error code.

Emulator: repositoryUri comes back as `<host>:<port>/<repositoryName>` — the replays show `127.0.0.1:40353/sim2real-xsvc-ptc-714aebd7383e/docker/library/busybox` and `127.0.0.1:45123/sim2real-xsvc-ptc-06eb3516128d/docker/library/busybox` (the host/port vary per run and are not themselves the requirement; the absence of the `<registryId>.dkr.ecr.<region>.amazonaws.com` authority is). Both checks fail on the invariant `".dkr.ecr." in repositoryUri` immediately after CreateRepository, before any ECS/logs behavior is exercised, so both are the one cause.

Why: a client cannot use the returned URI as an ECR registry reference. It cannot derive the registry id or region from it, cannot address the repository in the caller's account/region (the same repositoryName in two regions collapses onto one identity), and any consumer that parses the URI — docker login/push/pull targets, ECS/EKS task-definition `image` fields, CFN templates that build an image reference from the repository output, pull-through-cache image references — is handed an unusable value. Here it blocks the ECS container-image-pull scenarios outright.

Fix: the ECR CreateRepository handler's repositoryUri construction (and the shared Repository serializer that DescribeRepositories/DeleteRepository reuse) must compose the URI from the repository's registryId and the request region plus the repository name, rather than from the server's bound socket address; the emulator must then also serve/accept that announced registry host so pushes and pulls against it resolve.

Related already-open issues describe the same misconstruction for other services/operations and should be cross-checked when fixing, but do not cover these checks: 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md, 20261116T122519Z-cloudwatch-ecr-ecs-the-ecr-repositoryuri-handed-to.md, 20261116T122519Z-ecr-ecs-iam-describerepositories-reports-a-repositoryuri-that.md, 20261118T072013Z-batch-ecr-lambda-logs-ecr-createrepository-mints-a-repositoryuri-with-no-region.md.

Traces: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task, ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull.

## Evidence
Check ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task, helper _pull_through_cache_repo:
  repo = ecr.create_repository(repositoryName='<prefix>/docker/library/busybox', tags=...)['repository']
  uri  = repo['repositoryUri'] + ':latest'
  assert '.dkr.ecr.' in uri, uri
  -> AssertionError: 127.0.0.1:40353/sim2real-xsvc-ptc-714aebd7383e/docker/library/busybox:latest

Check ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull, same helper:
  -> AssertionError: 127.0.0.1:45123/sim2real-xsvc-ptc-06eb3516128d/docker/library/busybox:latest

Expected (AWS form): <registryId>.dkr.ecr.<region>.amazonaws.com/<prefix>/docker/library/busybox:latest
Actual (emulator form): <emulator-host>:<emulator-port>/<prefix>/docker/library/busybox:latest

Fixed text required in the value: '.dkr.ecr.' and '.amazonaws.com'. Variable parts: registryId (caller's account id / repository registryId), region (request region), repositoryName (as created, including the pull-through-cache prefix path). The 127.0.0.1 host and the ports 40353/45123 are per-run environment values, not part of the requirement.

HTTP status: 200 in both cases; no error code involved — the divergence is the payload value.

## Steps to Reproduce
1. Boot the emulator and point an ECR client at it.
2. CreatePullThroughCacheRule(ecrRepositoryPrefix='<prefix>', upstreamRegistryUrl='public.ecr.aws').
3. CreateRepository(repositoryName='<prefix>/docker/library/busybox').
4. Read repository.repositoryUri from the 200 response (and again via DescribeRepositories(repositoryNames=[...])).
5. Observe it is '<emulator-host>:<port>/<prefix>/docker/library/busybox' and contains neither '.dkr.ecr.' nor the request's region; AWS returns '<registryId>.dkr.ecr.<region>.amazonaws.com/<prefix>/docker/library/busybox'.
6. Equivalently, re-replay the behavioral checks ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task and ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull; both abort at the '.dkr.ecr.' assertion in _pull_through_cache_repo.
