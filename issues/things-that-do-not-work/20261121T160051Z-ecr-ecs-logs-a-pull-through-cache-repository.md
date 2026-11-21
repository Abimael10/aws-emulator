Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull
Issue-Key: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task|_pull_through_cache_repo#4|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Key: ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull|_pull_through_cache_repo#3|AssertionError|assert ".dkr.ecr." in uri, uri
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# ecr__ecs__logs: a pull-through-cache repository's repositoryUri is a bare host:port with no '.dkr.ecr.<region>.amazonaws.com' registry host, blocking both cache-backed image-pull checks at their first step

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePullThroughCacheRule

## Description
One net-new root cause covering both failing checks the suite lists under `### ecr__ecs__logs` (neither is marked ALREADY FILED nor OWNED).

NOTE FOR THE FIXER: the OPEN issue 20261111T010150Z-ecr-ecs-logs-ecr-pull-through-cache-repository-uri-is-not.md describes the same fault and lists the same two check ids, but the suite did not report either check as OWNED by it (its recorded replay keys no longer match the checks' current failure sites, `_pull_through_cache_repo#4`/`#3` vs the present assertion lines), so the failures are unowned at replay time and are filed here with their current grounding. Closing one should close the other. Other related OPEN issues that do not cover these keys: 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md, 20261118T072013Z-batch-ecr-lambda-logs-ecr-createrepository-mints-a-repositoryuri-with-no-region.md.

Cause 1 of 1: the repositoryUri reported for a pull-through-cache repository carries no ECR registry authority, so no image reference usable by an ECS task definition can be derived and both cache-backed pull scenarios abort before exercising any ECS behaviour.
AWS: Smithy: ecr Repository.repositoryUri — "The URI for the repository. You can use this URI for container image push and pull operations." AWS forms it as `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`, and for a pull-through cache the repository name is `<ecrRepositoryPrefix>/<upstream path>` (e.g. `<prefix>/docker/library/busybox`). The fixed parts are the `.dkr.ecr.` and `.amazonaws.com` registry-DNS form; the variable parts are the registry id (12-digit account), the service region, the cache rule's ecrRepositoryPrefix and the upstream repository path, all taken from the request/account.
Emulator: the URI comes back as the emulator's bare host:port followed by the path — the replay read `127.0.0.1:43679/sim2real-xsvc-ptc-<id>/docker/library/busybox:latest` and `127.0.0.1:46803/sim2real-xsvc-ptc-<id>/docker/library/busybox:latest`, failing `assert ".dkr.ecr." in uri`. No registry id, no region, no registry host. The host, port and generated prefix are environment values and are not the requirement; the registry-DNS authority is.
Why: a client cannot parse the registry id or region out of the URI, cannot build the `ecr:` resource ARN or `docker login` target from it, and an ECS task definition built from the reported URI references a registry that is not the account's ECR. Both replayed scenarios abort at their first step, so everything downstream — cache population on first pull, and the negative control where an execution role without ecr:GetAuthorizationToken must fail the pull — is never reached and remains unverified.
Fix: the ECR repository representation/serializer — the component that computes repositoryUri for CreateRepository, DescribeRepositories and the repository created on first pull through a cache rule must emit the `<registryId>.dkr.ecr.<region>.amazonaws.com/<prefix>/<upstream path>` form from the request's registry id and the service region (and, per 20261116T084727Z, that authority must also be served by the emulator).
Traces: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task, ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull — both fail on the same assertion inside their `_pull_through_cache_repo` helper.

## Evidence
Suite: service token `ecr__ecs__logs` 0/2 (0%), 2 failed — both of its checks.

Expected (AWS): the pull-through-cache repository's repositoryUri matches `<registryId>.dkr.ecr.<region>.amazonaws.com/<ecrRepositoryPrefix>/<upstream-path>` — i.e. it contains '.dkr.ecr.'.
Actual (mismatch), both checks:
  ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task — AssertionError on `assert ".dkr.ecr." in uri`: '127.0.0.1:43679/sim2real-xsvc-ptc-98f5a38ffdeb/docker/library/busybox:latest'
  ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull — AssertionError on `assert ".dkr.ecr." in uri`: '127.0.0.1:46803/sim2real-xsvc-ptc-1147272d24df/docker/library/busybox:latest'
No status or error-code divergence — the control-plane calls succeed and the body value is wrong.

## Steps to Reproduce
1. CreatePullThroughCacheRule with an ecrRepositoryPrefix and an upstream registry (Docker Hub / public ECR).
2. Trigger the cache repository's creation (first pull, or CreateRepository for '<prefix>/docker/library/busybox') and read repositoryUri from the CreateRepository/DescribeRepositories response.
3. AWS returns '<account>.dkr.ecr.<region>.amazonaws.com/<prefix>/docker/library/busybox'; the emulator returns '127.0.0.1:<port>/<prefix>/docker/library/busybox:latest', which contains no '.dkr.ecr.' and no account/region.
4. Re-replay checks ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task and ecr__ecs__logs.ecs-exec-role-without-ecr-auth-cannot-pull.
