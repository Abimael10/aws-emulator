Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected
Issue-Key: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected|run#18|AssertionError|assert other['repositoryArn'] != first['repositoryArn'] and (
Issue-Commit: 92e7e832444d3251314187cac9454f231606c9e1

# ecr: CreateRepository mints a repositoryUri with no region in it, so the same repository name in two regions collapses onto one identity

> _Note: this issue was identified against the code base at commit `92e7e832444d3251314187cac9454f231606c9e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: ECR's Repository.repositoryUri is minted without the region (and without the registry authority), so a repository created under the same name in a second region comes back carrying the IDENTICAL repositoryUri as the one in the first region. The two regional registries therefore present one shared push/pull identity even though their ARNs differ.

AWS: Smithy (com.amazonaws.ecr#Repository) — `repositoryUri` targets `Url`, documented "The URI for the repository. You can use this URI for container image `push` and `pull` operations."; `repositoryArn` is documented as containing "the `arn:aws:ecr` namespace, followed by the region of the repository, Amazon Web Services account ID of the repository owner, repository namespace, and repository name". On AWS the repositoryUri is the region-scoped registry authority followed by the repository name — i.e. `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>` — so two same-named repositories in two regions NEVER share a repositoryUri. The duplicate-name guard is per account PER REGION: the second CreateRepository in the home region is refused RepositoryAlreadyExistsException with the message "The repository with name '<repositoryName>' already exists in the registry with id '<registryId>'" (fixed text: "The repository with name '", "' already exists in the registry with id '", "'"; variable parts: the requested repositoryName and the caller's 12-digit account id), while the identical name in another region is accepted and must be a distinct repository.

Emulator: after the home-region duplicate was refused, creating the same name in the other region returned a repository whose repositoryArn names the other region but whose repositoryUri is byte-identical to the home-region repository's URI — the URI is a bare emulator authority plus the repository name, with no region (and no registry) component at all. The replay reports: "the us-east-1 repository shares its identity with the us-west-2 one (arn='arn:aws:ecr:us-east-1:<account>:repository/<name>', uri='<emulator-host>/<name>') — two regions must not collapse to the one repository the home-region call was rejected for". (The host and port in that URI are per-environment values, not a requirement; the requirement is that the authority be region-scoped and the URIs be distinct per region.)

Why: a client stores repositoryUri as the image reference — a Batch job definition, an ECS task definition or a `docker push` target. With one URI shared by two regions, a workload pinned to the us-east-1 repository silently addresses the us-west-2 one (or whichever the emulator resolves), and a caller cannot tell two regional repositories apart from their read-back. Cross-region image promotion, per-region immutability settings and per-region lifecycle rules all become unobservable.

Fix: ECR's CreateRepository handler and the shared Repository serializer that derives repositoryUri (the registry-authority derivation), plus the per-region scoping of the repository registry in ECR state so each region mints its own authority.

Related: open issue 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md reports that the announced repositoryUri host is one the emulator does not serve. It does not cover this defect: the failure here is that the URI carries no region at all, so it is not unique per region, which is a separate requirement from the host being reachable.

Traces: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected

## Evidence
Check: batch__ecr__lambda__logs.refarch-batch-repository-name-collision-rejected [mismatch]

Expected (AWS): the second CreateRepository in the home region is refused RepositoryAlreadyExistsException with "The repository with name '<name>' already exists in the registry with id '<account>'"; the same name in another region is then accepted and yields a DISTINCT repository — distinct repositoryArn (naming that region) AND distinct repositoryUri, because the URI is <registryId>.dkr.ecr.<region>.amazonaws.com/<name>.

Actual (emulator): the other-region create succeeded and returned repositoryArn='arn:aws:ecr:us-east-1:<account>:repository/<name>' (region present) but repositoryUri='<emulator-host:port>/<name>' — the same repositoryUri the home-region repository carries. Assertion raised at line 74 of the check:
  AssertionError: the us-east-1 repository shares its identity with the us-west-2 one (arn='arn:aws:ecr:us-east-1:...:repository/sim2real-refarch-batch-repo-...', uri='<host:port>/sim2real-refarch-batch-repo-...') — two regions must not collapse to the one repository the home-region call was rejected for

## Steps to Reproduce
1. CreateRepository(repositoryName=N, imageTagMutability='IMMUTABLE') in region A; note repository.repositoryUri and repository.repositoryArn.
2. CreateRepository(repositoryName=N) again in region A — expect RepositoryAlreadyExistsException with message "The repository with name 'N' already exists in the registry with id '<account>'".
3. CreateRepository(repositoryName=N) in a different region B (same account).
4. Compare the two returned repositories: repositoryArn must differ and must name its own region, and repositoryUri must differ (region-scoped registry authority). The emulator returns the same repositoryUri for both.
