Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected
Issue-Key: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected|run#18|AssertionError|assert other['repositoryArn'] != first['repositoryArn'] and (
Issue-Commit: 25ec19db617af3039a65e7f007e5f3e3f2921f74

# ECR repositoryUri is not region-qualified, so the same repository name in two regions reports one identical URI

> _Note: this issue was identified against the code base at commit `25ec19db617af3039a65e7f007e5f3e3f2921f74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
ONE root cause (1 of 1), reached through this service group's ECR name-collision rejection check.

Cause 1 of 1: Repository.repositoryUri is minted from the emulator's listening socket plus the repository name, with no account id and no region in the authority, so two repositories that differ ONLY in region collapse onto one and the same repositoryUri.

AWS: Smithy `ecr#Repository.repositoryUri` is documented only as "The URI for the repository. You can use this URI for container image push and pull operations" — the model states no format, so the documented/recorded form governs: `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`, i.e. the authority is the repository's own 12-digit registryId, the literal labels `dkr` and `ecr`, the region the repository lives in, then the partition's registry suffix; the path after the first '/' is exactly the repositoryName. The ECR registry namespace is per account PER REGION (Smithy `ecr#Repository.repositoryArn` is documented as containing "the region of the repository, Amazon Web Services account ID of the repository owner, repository namespace, and repository name"), so the SAME repositoryName may exist in two regions as two distinct repositories, and each one's repositoryUri — like its ARN — must name its own region and therefore differ from the other's.

Emulator: CreateRepository in the home region and CreateRepository under the IDENTICAL name in a second region both answer with the same repositoryUri: a bare `<host>:<port>/<repositoryName>` authority carrying neither registryId nor region. The repositoryArn is correctly region-qualified in both (the second repository's ARN names the second region), so only the URI projection diverges; the host and port are this environment's values and are not themselves the requirement — the defect is that the authority is not of the form `<registryId>.dkr.ecr.<region>.<suffix>` at all, which is why the region cannot be recovered from it and two regions collide. Everything else in this check agreed with AWS: the duplicate-name CreateRepository was refused with `RepositoryAlreadyExistsException` and the exact message "The repository with name '<name>' already exists in the registry with id '<accountId>'" (fixed text, with the submitted repositoryName and the caller's account id as the variable parts), the rejected create left imageTagMutability IMMUTABLE and the image list empty, and the second region accepted the name.

Why: a client that derives the registry to `docker login` to, builds an ECS task-definition `image` value, or asserts that an image reference belongs to its own account and region gets a URI from which both account and region are absent. Worse, two genuinely distinct repositories are indistinguishable by URI, so cross-region tooling that keys on repositoryUri silently treats the us-east-1 repository as the us-west-2 one — exactly the repository whose creation the home region had just refused.

Fix: the repositoryUri projection in the ECR service's repository state/serializer, shared by CreateRepository and the other Repository-returning operations (DescribeRepositories, DeleteRepository), must render `<registryId>.dkr.ecr.<region>.<endpoint suffix>/<repositoryName>` with registryId and region taken from the repository's own stored identity rather than from the emulator's listening socket.

Related but NOT covering this failure (the replay marked this check neither ALREADY FILED nor OWNED): open issues 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md and 20261115T004329Z-ecr-ecs-iam-ecr-describerepositories-reports-a-repositoryuri-that-is-not.md describe the un-qualified authority on the DescribeRepositories read path; this failure is the same projection reached through CreateRepository and shows its cross-region consequence (two regions, one URI). Also open: 20261111T010150Z-ecr-ecs-logs-ecr-pull-through-cache-repository-uri-is-not.md for the pull-through-cache variant.

Traces: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected.

## Evidence
Suite simparity-behavioral-aws-v1, replayed against this checkout (1884/2850 passed). The check is listed under `### batch__ecr__lambda__logs` and is marked neither ALREADY FILED nor OWNED.

Cause 1 — batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected [mismatch]:
  AssertionError: the us-east-1 repository shares its identity with the us-west-2 one (arn='arn:aws:ecr:us-east-1:<account>:repository/<name>', uri='<host>:<port>/<name>') — two regions must not collapse to the one repository the home-region call was rejected for

The check's walk, and where it diverged:
  1. CreateRepository(repositoryName=<name>, imageTagMutability='IMMUTABLE') in the home region — PASSED.
  2. CreateRepository(repositoryName=<name>) again — PASSED: refused with ('RepositoryAlreadyExistsException', "The repository with name '<name>' already exists in the registry with id '<account>'"), exactly AWS's code and message for that class.
  3. DescribeRepositories(repositoryNames=[<name>]) — PASSED: one repository, repositoryUri unchanged, imageTagMutability still IMMUTABLE, ListImages empty.
  4. CreateRepository(repositoryName=<name>) against the SECOND region — the name was accepted and the returned repositoryArn correctly named that second region, so the per-account-per-region collision scoping and the ARN projection are right.
  5. The assertion `other.repositoryArn != first.repositoryArn and other.repositoryUri != first.repositoryUri` FAILED. The ARNs differ (they carry different regions); the repositoryUris are byte-identical, both `<host>:<port>/<name>`.
  Expected: each region's repositoryUri is `<registryId>.dkr.ecr.<region>.amazonaws.com/<name>`, and the two therefore differ in their region label.
  Actual: one and the same `<host>:<port>/<name>` for both regions — no registryId, no region. The repository name, host and port shown are this run's generated values; the requirement is the shape of the authority, not those literals.

## Steps to Reproduce
Boot the emulator from this checkout and point an `ecr` client at it.
1. In region A: CreateRepository(repositoryName='r1', imageTagMutability='IMMUTABLE'); record repository.repositoryUri.
2. In region B (a different region, same account): CreateRepository(repositoryName='r1'); record repository.repositoryUri.
3. Compare the two URIs. Observe they are identical (`<host>:<port>/r1`), while the two repositoryArns correctly differ in their region field.
4. Note that neither URI's authority is of the form '<accountId>.dkr.ecr.<region>.amazonaws.com', so neither the owning account nor the region can be recovered from it.
   Replay: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected
