Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.ecracc-describe-unknown-repository-rejected
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.ecracc-describe-unknown-repository-rejected|run#9|AssertionError|assert host.startswith(f"{found[0]['registryId']}.dkr.ecr.{ctx.region}."), (
Issue-Commit: fc90a6588ac011398c7d49cbe030c73842e4efe1

# ecr: DescribeRepositories serves repositoryUri as the emulator's bare host:port, with no <registryId>.dkr.ecr.<region> registry authority

> _Note: this issue was identified against the code base at commit `fc90a6588ac011398c7d49cbe030c73842e4efe1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: the `repositoryUri` member of the ECR `Repository` shape returned by DescribeRepositories is serialized as the emulator's own listening socket address followed by the repository name, instead of an ECR registry DNS authority. Failing check: ecr.ecracc-describe-unknown-repository-rejected.

AWS: Smithy (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921) `Repository$repositoryUri` — "The URI for the repository. You can use this URI for container image `push` and `pull` operations." AWS composes that value as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`. The FIXED parts of the template are the label ordering and the literal labels `dkr` and `ecr` in the positions `<registryId>.dkr.ecr.<region>.`, the absence of any port component, and a path component equal exactly to `Repository$repositoryName`. The VARIABLE parts come from state and the request: the first label is `Repository$registryId` (the 12-digit account id owning the registry), the fourth label is the region of the call, the path is the repositoryName from the request. The structural rule is the requirement — no single run's account id, region, repository name, DNS suffix or port is part of it; the check accepts any served DNS suffix, so `amazonaws.com` specifically is not required. The call itself is a success: HTTP 200, no error code.

Emulator: DescribeRepositories answers HTTP 200 (no status or error-code divergence) with `$.repositories[0].repositoryUri` equal to the emulator's bare socket plus the name — recorded run `127.0.0.1:35147/sim2real-ecracc-repo-be119bf87cfb`. There is no registry-id label, no `dkr` label, no `ecr` label and no region label, and an explicit port is present, so the authority's first label is `127` rather than the body's own `registryId`. The check's earlier assertions on the same body pass (the repository is found by name and the URI path equals the repository name); only the authority diverges, and the check aborts there, so the two later behaviors it also covers (a name listed twice in one call must not be deduplicated, and one missing name beside a valid one must fail the whole call with RepositoryNotFoundException and the message "The repository with name '<name>' does not exist in the registry with id '<account>'") were never reached and are not asserted to be broken here.

Why: a client that uses the field as documented cannot. `docker login`/`push`/`pull` against the value addresses the emulator's control-plane socket rather than a registry, and any consumer that derives an ECS task-definition `image`, a Lambda `ImageUri`, a buildspec push target or an EKS pod image from it, or that parses the authority to recover the registry id and region (cross-account/cross-region checks, resource-policy conditions, IaC assertions that an image belongs to this account), gets a value from which none of that is recoverable. Because the authority is the per-run socket, artifacts derived from the API's own answer are not reproducible across runs either.

Fix: the ECR `Repository` serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and the authority it announces must be one the emulator actually answers on — both halves together (publish the registry-DNS form AND serve that host, e.g. via a host alias), not one traded for the other.

Status of related issues: 20261123T002251Z-ecr-describerepositories-reports-repositoryuri-as-the-emulator-s.md is marked FIXED and was grounded in this very check, but the check still fails the same way on this commit — the field is still the bare socket, so that resolution did not land for DescribeRepositories. This is not a refusal (the call still returns 200), it is the same 200-body divergence. Open issues recording the same field for OTHER checks, none of which carries this check's replay key: 20261124T024116Z-ecr-repositoryuri-is-served-as-the-emulator-s.md (ecr.ecsdeploy-repository-config-round-trips) and 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md (the reachability half). Fixing the single shared serializer should close all of them together.

Traces: ecr.ecracc-describe-unknown-repository-rejected (assertion at check_ecr_ecracc_describe_unknown_repository_rejected.py line 28).

## Evidence
Suite: service `ecr` scored 217/226 (96%), 9 failed; 8 of those are owned by open issues, this one is not.

[mismatch] ecr.ecracc-describe-unknown-repository-rejected
  check_ecr_ecracc_describe_unknown_repository_rejected.py line 28:
    assert host.startswith(f"{found[0]['registryId']}.dkr.ecr.{ctx.region}.")
  AssertionError: the URI host must start with registry id and region; '127.0.0.1:35147/sim2real-ecracc-repo-be119bf87cfb'

Expected (AWS): HTTP 200 and `$.repositories[0].repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>` — the authority's first label is the 12-digit registry id (equal to `registryId` in the same body and to the calling account), followed by `dkr`, `ecr`, the request region and at least one suffix label; no port; the path equals repositoryName.
Actual (emulator): HTTP 200 and `$.repositories[0].repositoryUri` == `<emulator-host>:<emulator-port>/<repositoryName>` — a bare socket address with none of the required labels and with a port, so `repositoryUri.split('/',1)[0].split('.',1)[0]` is `127` instead of the registryId.

No status, error code or error message divergence is involved: the 200 body is wrong. The account id, region, repository name, host and port of the recorded run are per-run values filling the template and are not themselves requirements.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Point an AWS client at the emulator's ECR endpoint and call CreateRepository with a valid repositoryName and tags=[{Key:sim2real,Value:ecracc}].
3. Call DescribeRepositories with repositoryNames=[that name] and split `$.repositories[0].repositoryUri` on the first '/'.
   -> AWS: the authority is `<registryId>.dkr.ecr.<region>.<dns-suffix>` with no port, its first label equals both `registryId` in the body and the calling account, and the path is the repository name.
   -> Emulator: the authority is the emulator's bare `127.0.0.1:<port>`, so the first label is `127`.
4. Attempt what the field documents — resolve the authority, `docker login`/`push` against it, or extract the registry id and region from it: none is possible.
5. Re-replay check ecr.ecracc-describe-unknown-repository-rejected to confirm the fix (note it continues past the URI assertion into DescribeRepositories duplicate-name and RepositoryNotFoundException coverage once the URI is correct).

## Resolution
`Repository$repositoryUri` is now composed in ONE place from the stored record's own registry:
`Repo::to_repository` in
`services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/src/state.rs` calls
`registry_host_in_zone(registry_id, region, zone)` — the crate's one composer of a registry
host — over the zone `announced_registry_zone` reads off the endpoint this emulator answers
that caller at (`src/state/registry_address.rs`, through the tree's own
`service_common::data_plane_addressing::announced_zone_of_authority`), followed by the
repository's own name as the path. Every operation that reports a `Repository` passes that
zone: `CreateRepository`, `DescribeRepositories`, `DeleteRepository`.

Why there is no port on it, and how a client still reaches the registry: the member carries a
NAME, as the value AWS publishes does, and the two things that make the name reach this
registry are that its zone is one this process answers in (the zone RFC 6761 reserves for the
interface the core binds its listeners on, or the name an operator serves this process under)
and that the registry's `/v2` plane claims a request whatever host it arrived on
(`src/service.rs`, `src/service/registry_v2.rs`). The address for a client that resolves
nothing and dials what it was handed, port included, is the registry URL
`GetAuthorizationToken` carries (`AuthorizationData$proxyEndpoint`, built on
`announced_registry_endpoint`), which the same plane answers for the same credential — so the
repository's URI says WHICH registry and repository is addressed, in the spelling every
consumer of the member parses, and the login URL says where to open the connection on this
machine. The module header of `src/state/registry_address.rs` states this split.

Pinned by the crate's own `tests/all/repository_uri_registry_layout.rs`, which reads the
authority label by label against the identity the same response reports and pulls the pushed
manifest at it, plus `tests/all/repository_registry_uri.rs`,
`tests/all/published_registry_address.rs`, `tests/all/repository_uri_is_served.rs`,
`tests/all/repository_uri_is_dialable_wherever_served.rs`,
`tests/all/cached_repository_registry_address.rs` and `tests/all/named_read_projection.rs`,
and on the cross-service side by
`core/router/tests/all/ecr_repository_uri_is_a_registry_authority_for_its_consumers.rs` and
`core/router/tests/all/batch_job_definition_image_names_a_served_registry.rs`.
