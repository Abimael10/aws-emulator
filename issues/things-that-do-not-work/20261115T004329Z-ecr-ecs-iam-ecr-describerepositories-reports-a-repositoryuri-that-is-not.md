Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle
Issue-Key: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 7148f2b6638ce0f4ccfada01b07f71c83f7419cd

# ECR DescribeRepositories reports a repositoryUri that is not account- and region-qualified, so the registry URI does not address the repository in this account/region

> _Note: this issue was identified against the code base at commit `7148f2b6638ce0f4ccfada01b07f71c83f7419cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
ONE root cause (1 of 1), reached through the ECR-access lifecycle check of this service group.

Cause 1 of 1: Repository.repositoryUri is served with a bare registry authority that carries neither the account id nor the region, instead of AWS's account- and region-qualified registry hostname.
AWS: Smithy ecr#Repository.repositoryUri is documented only as "The URI for the repository. You can use this URI for container image push and pull operations" — the format itself is not in the model, so for the shape AWS's documented and recorded form applies: `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`, i.e. the authority is the caller's 12-digit account id (the repository's registryId), the literal label `dkr`, `ecr`, the region of the request, then the registry suffix; the path component after the first '/' is exactly the repositoryName. The account id and region are the contract-bearing parts: they are what makes the URI address THIS repository in THIS account and region, and they are what `docker login`/push/pull, task-definition `image` values and cross-account pull policies are derived from.
Emulator: DescribeRepositories returns a repositoryUri whose authority is a bare host:port with no account id and no region — '127.0.0.1:41529/<repositoryName>' in this run. The path component is correct (it is the repositoryName); only the authority diverges. The specific host and port are this environment's values and are not themselves the requirement — the defect is that the authority is not of the form `<accountId>.dkr.ecr.<region>.<suffix>` at all, so neither the owning account nor the region can be recovered from the URI.
Why: a client that parses repositoryUri to derive the registry it must authenticate to, to build the `image` string for an ECS task definition, or to assert that an image reference belongs to its own account and region, gets a value from which account and region are absent; a cross-account or cross-region check built on that parse silently passes. Any tool that round-trips repositoryUri back into an ECR API call, or compares it against an image reference read from a task definition, cannot match them.
Fix: the repositoryUri projection in the ECR service's repository state/serializer (shared by CreateRepository, DescribeRepositories and the other Repository-returning operations) must render the account-qualified registry authority `<registryId>.dkr.ecr.<region>.<endpoint suffix>` followed by '/<repositoryName>', with registryId and region taken from the repository's own state and the request's region rather than from the emulator's listening socket.
Traces: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle.

## Evidence
Suite simparity-behavioral-aws-v1, replayed against this checkout (713/1010 passed). The check is listed under `### ecr__ecs__iam` and is marked neither ALREADY FILED nor OWNED.

Cause 1 — ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle [mismatch], phase 2/4 'solve':
  AssertionError: [phase 2/4 solve] the resolved repository URI '127.0.0.1:41529/sim2real-ecracc-e2e-fc419148709e-repository-84d176e5af61' must address 'sim2real-ecracc-e2e-fc419148709e-repository-84d176e5af61' in this account and region
  The check's final read in its frozen six-operation walk is `DescribeRepositories(repositoryNames=[<repo>])` -> repositories[0].repositoryUri (equivalent CLI: `aws ecr describe-repositories --repository-names <repo> --query repositories[0].repositoryUri --output text`). It splits the value at the first '/' and requires path == <repositoryName> and host.startswith(f"{account}.dkr.ecr.{region}.").
  Expected: a repositoryUri whose authority is `<accountId>.dkr.ecr.<region>.<suffix>` and whose path is the repositoryName.
  Actual: authority '127.0.0.1:41529' — no account id, no region. The path component matched. The repository name and the host/port shown are this run's generated values; the requirement is the shape of the authority, not those literals.
  The five preceding operations of the walk (ecs ListServices, DescribeServices, DescribeTaskDefinition, iam ListAttachedRolePolicies, ListRolePolicies) and their assertions passed, so the walk reached ECR with the correct repository; the remaining 'can_access' verdict and the cleanup/destroy phases were not graded.

## Steps to Reproduce
Boot the emulator from this checkout and point an `ecr` client at it.
1. CreateRepository(repositoryName='r1').
2. DescribeRepositories(repositoryNames=['r1']) and read repositories[0].repositoryUri (CreateRepository's own response carries the same field).
3. Observe an authority with no account id and no region (e.g. '<host>:<port>/r1') instead of '<accountId>.dkr.ecr.<region>.amazonaws.com/r1'; split the URI at the first '/' and note that the host does not start with '<accountId>.dkr.ecr.<region>.'.
   Replay: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle
