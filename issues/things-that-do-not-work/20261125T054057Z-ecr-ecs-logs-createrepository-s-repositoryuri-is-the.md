Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull
Issue-Key: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task|_pull_through_cache_repo#4|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Key: ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull|_pull_through_cache_repo#3|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Commit: 5dff3459c9fa0114d27ae1b22046b841878cac27

# ecr__ecs__logs: CreateRepository's repositoryUri is the emulator's bare host:port, so a pull-through-cache image URI is not an ECR registry URI

> _Note: this issue was identified against the code base at commit `5dff3459c9fa0114d27ae1b22046b841878cac27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: a private ECR repository created under a pull-through-cache prefix reads back a repositoryUri that is not an ECR registry URI, so no container image reference derived from it can name the registry, region or account. Failing checks: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task, ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull.

AWS: Smithy: CreateRepository -> CreateRepositoryResponse.repository (shape Repository), member repositoryUri documented as "The URI for the repository. You can use this URI for container image push and pull operations." AWS mints that URI as the registry authority followed by the repository name: <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName> — i.e. the account id of the registry, the literal label sequence `.dkr.ecr.`, the region of the call, and the partition's registry domain, then `/` and the repositoryName exactly as submitted (here `<prefix>/docker/library/busybox` for a pull-through-cache rule whose ecrRepositoryPrefix is <prefix>). The account id, region, prefix and port are per-environment values; the `.dkr.ecr.` registry authority shape is the requirement.

Emulator: repositoryUri comes back as the emulator's bare serving host and port followed by the repository name, with no registry authority at all — recorded as `127.0.0.1:33579/sim2real-xsvc-ptc-90f2aea11b52/docker/library/busybox` and `127.0.0.1:35529/sim2real-xsvc-ptc-a2470580bd6a/docker/library/busybox` in the two checks. The substring `.dkr.ecr.` is absent, so both checks abort on `assert ".dkr.ecr." in uri` immediately after CreateRepository, before any ECS task is launched.

Why: a client that takes repositoryUri and uses it as a container image reference (as a task definition's `image`, or for docker push/pull, or to derive the registry to `docker login` against) gets a reference that identifies neither the registry, the region nor the account. Two registries in different regions, or the same repository name in a second region, collapse onto one identity, and nothing downstream — ECS image pull, pull-through-cache import, execution-role authorization on the registry — can be exercised at all.

Why a similar open issue does not cover these checks: 20261124T024116Z-ecr-repositoryuri-is-served-as-the-emulator-s.md, 20261121T160051Z-ecr-ecs-iam-describerepositories-reports-a-repositoryuri-that--r2.md, 20261118T072013Z-batch-ecr-lambda-logs-ecr-createrepository-mints-a-repositoryuri-with-no-region.md and 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md all describe repositoryUri defects, but none carries the replay key of these two ecr__ecs__logs checks, so neither check is re-replayed when they are closed.

Fix: the ECR CreateRepository handler's response serialization (and the shared Repository-shape projection used by DescribeRepositories/CreateRepository), where repositoryUri is composed — it must be built from the registry id, the request's region and the partition's ECR registry domain, not from the emulator's listener host:port.

Traces: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task (AssertionError at _pull_through_cache_repo, `assert ".dkr.ecr." in uri`), ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull (same assertion).

## Evidence
Expected (AWS): CreateRepository(repositoryName="<prefix>/docker/library/busybox") -> repository.repositoryUri == "<registryId>.dkr.ecr.<region>.amazonaws.com/<prefix>/docker/library/busybox" — contains the registry authority ".dkr.ecr.".

Actual (emulator): repository.repositoryUri == "127.0.0.1:33579/sim2real-xsvc-ptc-90f2aea11b52/docker/library/busybox" (and "127.0.0.1:35529/sim2real-xsvc-ptc-a2470580bd6a/..." in the second check) — the emulator's bare host:port, no registry authority.

Check output:
  File "check_ecr__ecs__logs_ecr_pull_through_cache_image_pulled_by_ecs_task.py", line 122, in _pull_through_cache_repo
    assert ".dkr.ecr." in uri, uri
  AssertionError: 127.0.0.1:33579/sim2real-xsvc-ptc-90f2aea11b52/docker/library/busybox:latest

  File "check_ecr__ecs__logs_ecs_exec_role_without_ecr_auth_cannot_pull.py", line 215, in _pull_through_cache_repo
    assert ".dkr.ecr." in uri, uri
  AssertionError: 127.0.0.1:35529/sim2real-xsvc-ptc-a2470580bd6a/docker/library/busybox:latest

## Steps to Reproduce
1. ecr.create_pull_through_cache_rule(ecrRepositoryPrefix="<prefix>", upstreamRegistryUrl="public.ecr.aws").
2. ecr.create_repository(repositoryName="<prefix>/docker/library/busybox").
3. Read repository.repositoryUri from the response (or call ecr.describe_repositories for the same name).
4. Observe the value is the emulator's host:port + "/<repositoryName>" and contains no ".dkr.ecr." registry authority, instead of "<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>".
5. Re-replay checks ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task and ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull to confirm.
