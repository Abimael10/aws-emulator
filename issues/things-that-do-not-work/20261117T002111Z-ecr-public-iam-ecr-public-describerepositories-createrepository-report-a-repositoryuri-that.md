Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action
Issue-Key: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action|run#30|AssertionError|assert _public_ecr_host(host) and path.count("/") >= 1, (
Issue-Commit: 8a68b551a0cf0fdfcdf4f4adc4f121b6833b21cb

# ecr-public: DescribeRepositories/CreateRepository report a repositoryUri that is the emulator's bare host:port instead of a public.ecr registry authority with the registry alias (1 cause)

> _Note: this issue was identified against the code base at commit `8a68b551a0cf0fdfcdf4f4adc4f121b6833b21cb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: ECR Public reports `Repository$repositoryUri` as the emulator's bare socket address with a fixed `emulator` path segment, so nothing in the URI identifies the public registry, its alias, or the repository as an ECR Public repository. Failing check: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action (the final read-back beat, after the scoped principal's CreateRepository is allowed again).

AWS: Smithy (ecr-public) `Repository$repositoryUri` (target `Url`) — "The URI for the repository. You can use this URI for container image push and pull operations." For the PUBLIC registry AWS composes that URI as `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>`: the leading labels `public` and `ecr` are fixed text, the remaining labels are the partition's own DNS suffix for the public registry, the first path segment is the registry's alias (the account's public-registry alias, `RegistryAlias`/`Registry$aliases`, not the 12-digit account id), and the last path segment is exactly `Repository$repositoryName` as the request spelled it. `Repository$registryId` separately carries the 12-digit owning account (pattern `^[0-9]{12}$`). That label structure is the requirement; the concrete alias, DNS suffix and repository name of any one run are values from the account's registry state and the request. The call itself is a 200 with no error code — this is a body-shape divergence, not a status or error divergence.

Emulator: `DescribeRepositories` (and `CreateRepository`, which returns the same shape) answers 200 with `repositoryUri` = `127.0.0.1:43735/emulator/sim2real-behav-ecrpub-iamrepo-<id>` — the host:port the emulator happens to be bound to in that run, plus a literal `emulator` segment standing in for the registry alias. There is no `public.ecr.` prefix and no domain at all, so the check's host test (`labels[:2] == ["public", "ecr"]` and a further non-empty domain label) fails with "the repository's URI is '127.0.0.1:43735/emulator/<repo>'; an ECR Public repository is public.ecr.<domain>/<alias>/<name>".

Why: `repositoryUri` is the one field a client uses to push and pull — `docker login public.ecr.<suffix>` followed by `docker push <uri>:tag`, and any tooling that parses the URI to recover the registry alias (to build a pull URI for an unauthenticated consumer, or to tell a public registry URI apart from a private `<acct>.dkr.ecr.<region>` one). A client relying on AWS's shape gets a URI whose authority is not a registry name at all, cannot tell that the repository is public, and cannot recover the alias; the value also changes with the emulator's listening port on every run, so nothing derived from it is reproducible.

Fix: the ecr-public `Repository` serializer / the state component that composes `repositoryUri` from the registry's alias and the repository name (shared by CreateRepository, DescribeRepositories and the registry read paths) must emit the documented `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>` authority, and must source the alias from the account's public-registry state rather than a constant — and that host must be one the emulator answers on. Note: open issue 20261116T122519Z-ecr-ecs-iam-describerepositories-reports-a-repositoryuri-that.md (and the private-registry issues it lists, e.g. 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md) cover the SAME field for the PRIVATE `ecr` registry (`<acct>.dkr.ecr.<region>`); none of them carries this check's replay key or the ecr-public alias form, so this is a distinct cause in a distinct service.

Traces: ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action.

## Evidence
Suite: service ecr-public__iam scored 0/1 (0%), 1 failed; this check is not marked ALREADY FILED and is not OWNED by any open issue.

[mismatch] ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action
  check_ecr_public__iam_iam_policy_without_create_repository_blocks_only_that_action.py line 258, in run:
    assert _public_ecr_host(host) and path.count("/") >= 1
  AssertionError: the repository's URI is '127.0.0.1:43735/emulator/sim2real-behav-ecrpub-iamrepo-fe6027b559a7'; an ECR Public repository is public.ecr.<domain>/<alias>/<name>

The check's own definition of the required host (in the same file) is:
  def _public_ecr_host(host): labels = host.split("."); return labels[:2] == ["public", "ecr"] and len(labels) > 2 and all(labels[2:])
and it additionally requires `repositoryUri.endswith("/" + repositoryName)`.

Expected (AWS): HTTP 200; `$.repositories[0].repositoryUri` == `public.ecr.<dns-suffix>/<registryAlias>/<repositoryName>` — fixed `public.ecr` labels, then the partition's DNS suffix, then the account's public-registry alias, then the repository name; `$.repositories[0].registryId` == the caller's 12-digit account.
Actual (emulator): HTTP 200; `$.repositories[0].repositoryUri` == `127.0.0.1:<port>/emulator/<repositoryName>` — a bare IP:port authority and a constant `emulator` segment where the registry alias belongs.
No status-code or error-code divergence. The port, the generated repository name and any one run's alias are per-run values; the label structure is the requirement.

Everything the check asserts before this point passed, so the divergence is isolated to the URI shape: the scoped principal's DescribeRepositories/GetAuthorizationToken were allowed, CreateRepository was correctly refused AccessDeniedException while the policy omitted ecr-public:CreateRepository, the repository was absent at the destination, and the identical create succeeded once the action was granted.

## Steps to Reproduce
1. Against the emulator, create an ECR Public repository in us-east-1: `aws ecr-public create-repository --repository-name <name> --region us-east-1`.
2. Read it back as any principal: `aws ecr-public describe-repositories --repository-names <name> --region us-east-1`.
3. Inspect `$.repositories[0].repositoryUri` and split it on the first '/':
   -> AWS: authority `public.ecr.<dns-suffix>` (fixed `public`, `ecr` labels, then a real domain), path `<registryAlias>/<name>`.
   -> Emulator: authority `127.0.0.1:<port>` (no `public.ecr.` labels, an explicit port), path `emulator/<name>`.
4. Confirm the same field is returned by CreateRepository's own response.
5. Re-replay check ecr-public__iam.iam-policy-without-create-repository-blocks-only-that-action to confirm the final read-back beat passes.
