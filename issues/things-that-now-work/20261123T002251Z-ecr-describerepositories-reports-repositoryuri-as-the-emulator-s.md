Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.ecracc-describe-unknown-repository-rejected
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.ecracc-describe-unknown-repository-rejected|run#9|AssertionError|assert host.startswith(f"{found[0]['registryId']}.dkr.ecr.{ctx.region}."), (
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# ecr: DescribeRepositories reports repositoryUri as the emulator's bare host:port, with no <registryId>.dkr.ecr.<region> registry authority (1 cause)

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: the `repositoryUri` the emulator serializes into an ECR Repository body carries no ECR registry authority at all — it is the emulator's bare socket address `<host>:<port>` followed by the repository name. Failing check: ecr.ecracc-describe-unknown-repository-rejected.

AWS: Smithy (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921) `Repository$repositoryUri` (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." AWS composes that value as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`: the FIXED text is the label ordering and the literal labels `dkr` and `ecr`, in the positions `<registryId>.dkr.ecr.<region>.`, with no port component and with the path component exactly `Repository$repositoryName`. The VARIABLE parts are taken from state and the request: `registryId` is `Repository$registryId` (the 12-digit account id of the registry that owns the repository), `region` is the region the call was made in, `repositoryName` is the name in the request. The structural rule is the requirement; the concrete id, region, name and DNS suffix of any one run are not. The check asserts exactly that rule — the authority's first four DNS labels must be `[registryId, "dkr", "ecr", region]` plus at least one further non-empty suffix label — so an emulator-served domain suffix is permitted; `amazonaws.com` is not required.

Emulator: DescribeRepositories answers HTTP 200 (no status or error-code divergence) with `$.repositories[0].repositoryUri` == the emulator's listening socket plus the name (recorded run: `127.0.0.1:42017/sim2real-ecracc-repo-8e281e448bdc`). The value has no registry-id label, no `dkr` label, no `ecr` label and no region label, and it carries an explicit port. The same Repository serializer feeds CreateRepository and DeleteRepository, so every Repository-shaped response is affected.

Why: a client that does what the field documents — `docker login`/`push`/`pull` against the URI, or deriving an ECS task-definition image, a Lambda `ImageUri`, a buildspec push target or an EKS pod image from it — cannot recover the registry id or the region from the value. Any logic that parses the URI (cross-account/cross-region checks, registry-id extraction, resource-policy conditions keyed on `aws:PrincipalAccount`, IaC assertions that the image belongs to this account) misbehaves or fails. Because the authority is the per-run socket, templates and task definitions built from the API's own answer are also not reproducible between runs.

Fix: the ECR Repository serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and the host it announces must be one the emulator answers on — the two requirements have to be satisfied together (publish the registry-DNS form AND make that authority reachable, e.g. via a served host alias), not traded off against each other. Note: an earlier issue on this exact field for this service, 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, is marked FIXED, yet the check still fails on the same field — the response is now a bare host:port rather than an unserved registry DNS name, so the shape requirement is still unmet. The open wire issue 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md records the reachability half of the same field and does not carry this check's replay key.

Traces: ecr.ecracc-describe-unknown-repository-rejected (assertion at check_ecr_ecracc_describe_unknown_repository_rejected.py line 28).

## Evidence
Suite: service `ecr` scored 217/226 (96%), 9 failed; this is the only one of those not owned by an open issue.

[mismatch] ecr.ecracc-describe-unknown-repository-rejected
  check_ecr_ecracc_describe_unknown_repository_rejected.py line 28:
    assert host.startswith(f"{found[0]['registryId']}.dkr.ecr.{ctx.region}.")
  AssertionError: the URI host must start with registry id and region; '127.0.0.1:42017/sim2real-ecracc-repo-8e281e448bdc'

Expected (AWS): HTTP 200 and `$.repositories[0].repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>` — first four authority labels are the 12-digit registry id, `dkr`, `ecr` and the request region, then at least one suffix label, no port, path == repositoryName.
Actual (emulator): HTTP 200 and `$.repositories[0].repositoryUri` == `127.0.0.1:42017/<repositoryName>` — a bare host:port with none of the required labels.
No status, error code or message divergence is involved: the 200 body is wrong. The 12-digit id, the region, the repository name and the port seen in the recorded run are per-run values filling the template, not part of the requirement.
Because the check aborts on this assertion, the later assertions in the same check (a name repeated in one call must not be deduplicated; one missing name among valid ones must fail the whole call with RepositoryNotFoundException and message "The repository with name '<name>' does not exist in the registry with id '<accountId>'") were never reached and are not claimed here.

## Steps to Reproduce
1. Point an AWS client at the emulator's ECR endpoint and call CreateRepository with a valid repositoryName.
2. Call DescribeRepositories with repositoryNames=[that name] and split `$.repositories[0].repositoryUri` on the first '/'.
   -> AWS: the authority's first four labels are `<registryId>`, `dkr`, `ecr`, `<region>`, followed by a DNS suffix, with no port; the path is the repository name.
   -> Emulator: the authority is the emulator's bare `127.0.0.1:<port>`.
3. Try to use the value as the field documents (resolve the authority / `docker login` against it / extract the registry id and region from it): none of those is possible.
4. Re-replay check ecr.ecracc-describe-unknown-repository-rejected to confirm the fix.
