Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.ecsdeploy-repository-config-round-trips
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.ecsdeploy-repository-config-round-trips|run#15|AssertionError|assert path == name and registry.split(".", 1)[0] == repo["registryId"] == ctx.account, (
Issue-Commit: 7a64cae84a9cfda7163325030ecd9d33dd5ead34

# ecr: repositoryUri is served as the emulator's bare host:port, so a repository read back after create carries no <registryId>.dkr.ecr.<region> registry authority

> _Note: this issue was identified against the code base at commit `7a64cae84a9cfda7163325030ecd9d33dd5ead34`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: the `repositoryUri` member of the ECR Repository shape is serialized as the emulator's own listening socket address followed by the repository name, instead of an ECR registry DNS authority. Failing check: ecr.ecsdeploy-repository-config-round-trips.

AWS: Smithy (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921) `Repository$repositoryUri` — "The URI for the repository. You can use this URI for container image `push` and `pull` operations." AWS composes the value as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`. The FIXED part of that template is the label ordering and the literal labels `dkr` and `ecr` in the positions `<registryId>.dkr.ecr.<region>.`, with no port component, and a path component equal exactly to `Repository$repositoryName`. The VARIABLE parts come from state and the request: `registryId` is `Repository$registryId` (the 12-digit account id owning the registry, which must equal the calling account), `region` is the region of the call, `repositoryName` is the name in the request. The structural rule — not any one run's account id, region, name or DNS suffix — is the requirement; the check accepts any served DNS suffix, so `amazonaws.com` specifically is not required.

Emulator: CreateRepository and DescribeRepositories both answer HTTP 200 (no status or error-code divergence) with `$.repositories[0].repositoryUri` equal to the emulator's bare socket plus the name — recorded run `127.0.0.1:39599/sim2real-ecsdeploy-repo-2476d8c33020`. There is no registry-id label, no `dkr` label, no `ecr` label and no region label, and an explicit port is present, so `repositoryUri.split('/',1)[0].split('.',1)[0]` is `127:0` rather than the registryId/account. Everything else this check asserts on the same body is already correct: the omitted `imageTagMutability` defaults to MUTABLE, `imageScanningConfiguration` round-trips as `{"scanOnPush": false}`, the unspecified KMS key resolves to a real `arn:aws:kms:<region>:<account>:key/<id>` and round-trips, and `repositoryArn` ends with `:<account>:repository/<name>` — only the URI diverges.

Why: a client that uses the field as documented cannot. `docker login`/`push`/`pull` against the value addresses the emulator's control-plane socket, not a registry; and any consumer that derives an ECS task-definition `image`, a Lambda `ImageUri`, a buildspec push target or an EKS pod image from it, or that parses the authority to recover the registry id and region (cross-account/cross-region checks, resource-policy conditions, IaC assertions that an image belongs to this account), gets a value from which none of that is recoverable. Because the authority is the per-run socket, artifacts built from the API's own answer are also not reproducible across runs.

Fix: the ECR Repository serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and the authority it announces must be one the emulator actually answers on — both halves together (publish the registry-DNS form AND serve that host, e.g. via a host alias), not one traded for the other.

Related already-open issues, neither of which carries this check's replay key: 20261123T002251Z-ecr-describerepositories-reports-repositoryuri-as-the-emulator-s.md records the same field for check ecr.ecracc-describe-unknown-repository-rejected, and 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md records the reachability half. Fixing the serializer should close all of them together.

Traces: ecr.ecsdeploy-repository-config-round-trips (assertion at check_ecr_ecsdeploy_repository_config_round_trips.py line 44).

## Evidence
Suite: service `ecr` scored 217/226 (96%), 9 failed; 8 of those are owned by open issues, this one is not.

[mismatch] ecr.ecsdeploy-repository-config-round-trips
  check_ecr_ecsdeploy_repository_config_round_trips.py line 44:
    registry, path = repo["repositoryUri"].split("/", 1)
    assert path == name and registry.split(".", 1)[0] == repo["registryId"] == ctx.account
  AssertionError: the URI must be <account registry>/<name>; got '127.0.0.1:39599/sim2real-ecsdeploy-repo-2476d8c33020'

Expected (AWS): HTTP 200 and `$.repositories[0].repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>` — the authority's first label is the 12-digit registry id (equal to the calling account and to `registryId` in the same body), then `dkr`, `ecr`, the request region and at least one suffix label; no port; path == repositoryName.
Actual (emulator): HTTP 200 and `$.repositories[0].repositoryUri` == `<emulator-host>:<emulator-port>/<repositoryName>` — a bare socket address with none of the required labels and with a port.

No status, error code or error message divergence is involved: the 200 body is wrong. The account id, region, repository name and port of the recorded run are per-run values filling the template and are not themselves requirements. The create and describe bodies agree with each other (the check's `repositoryUri` create-vs-describe equality assertion passed), so the defect is in the single shared serializer, not in one operation's copy.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Point an AWS client at the emulator's ECR endpoint and call CreateRepository with a valid repositoryName, imageScanningConfiguration={scanOnPush:false} and encryptionConfiguration={encryptionType:'KMS'} (no kmsKey).
3. Call DescribeRepositories with repositoryNames=[that name] and split `$.repositories[0].repositoryUri` on the first '/'.
   -> AWS: the authority is `<registryId>.dkr.ecr.<region>.<dns-suffix>` with no port, and its first label equals both `registryId` in the body and the calling account; the path is the repository name.
   -> Emulator: the authority is the emulator's bare `127.0.0.1:<port>`, so the first label is `127`.
4. Attempt what the field documents — resolve the authority, `docker login`/`push` against it, or extract the registry id and region from it: none is possible.
5. Re-replay check ecr.ecsdeploy-repository-config-round-trips to confirm the fix.
