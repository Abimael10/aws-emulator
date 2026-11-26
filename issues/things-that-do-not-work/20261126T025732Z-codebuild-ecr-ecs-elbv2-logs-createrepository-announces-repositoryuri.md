Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Commit: f0e185a528f8edd646da50d28e9f657e7fd1f0a5

# codebuild__ecr__ecs__elbv2__logs: CreateRepository announces repositoryUri on <registryId>.dkr.ecr.<region>.localhost, a host the emulator does not serve, so the container CI/CD chain can neither push nor pull

> _Note: this issue was identified against the code base at commit `f0e185a528f8edd646da50d28e9f657e7fd1f0a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
1 net-new cause, grounded in BOTH surviving failing checks of this service group (0/2, 0% this run). Neither check is marked ALREADY FILED or OWNED this run.

Cause 1 of 1: ECR's CreateRepository returns a repositoryUri whose authority is a registry-shaped but unserved name, `<registryId>.dkr.ecr.<region>.localhost/<repositoryName>` (no port, non-resolvable TLD). The emulator answers on its own listener host:port, so the address it publishes for container `push`/`pull` is not an address any client of the emulator can dial. Every downstream step of the chain (the CodeBuild `docker push`, the task definition's `<repositoryUri>@<digest>` image, the Fargate pull) is addressed through that URI, so the whole flow is cut off at its first assertion.
Failing check ids for this cause: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

AWS: Smithy (com.amazonaws.ecr) Repository.repositoryUri — "The URI for the repository. You can use this URI for container image push and pull operations." On real AWS that URI is `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`, a host that actually serves the registry API, so the value is simultaneously the registry identity AND a dialable address. The requirement is that property: the published URI must carry registry/account/region identity AND name a host the serving endpoint actually answers on. The account id, region, repository name and port in any one run are values taken from the request and the emulator's own listener, never literals.

Emulator: CreateRepository returns HTTP 200 with repositoryUri `000000000000.dkr.ecr.us-west-2.localhost/sim2real-xsvc-ecr-<suffix>`. The harness's own check of published addresses rejects it: "the emulator published https://000000000000.dkr.ecr.us-west-2.localhost/<repo>: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". Both checks abort there (run.py line ~44/46, AssertionError from ctx.published_url_mismatch), before any build, digest, task, target health or ALB body is produced.

Note on prior work: fixed issue 20261124T134157Z-codebuild-ecr-ecs-elbv2-logs-ecr-repositoryuri-announces-registryid-dkr-ecr-region-localhost.md claimed exactly this URI defect for this service group; its fix only dropped the `:<port>` suffix — the announced host `<registryId>.dkr.ecr.<region>.localhost` is still not a host the emulator serves, so the published address is still undialable. Open issue 20261125T110100Z-codebuild-ecr-ecs-elbv2-logs-both-container-ci--r2.md lists the same two checks but is keyed on a later poll-timeout step ('_poll_until#2|Timeout'), not on this published-URL assertion, so it does not cover this run's failure. The related ECR-side issues (20261124T024116Z-ecr-repositoryuri-is-served-as-the-emulator-s.md, 20261125T054057Z-ecr-ecs-logs-createrepository-s-repositoryuri-is-the.md) describe the opposite symptom (a bare host:port with no registry authority) and carry none of this group's keys.

Why: a client that uses repositoryUri the way AWS documents it — as the target of `docker login`/`docker push`/`docker pull`, or as the image reference in an ECS task definition — cannot reach the registry at all: the name does not resolve and nothing listens on it. The whole CodeBuild -> ECR -> ECS/ALB topology is unusable against the emulator, and the failure appears as an unresolvable host rather than as an API error, so it is hard to attribute.

Fix: the ECR repository-URI construction used by the CreateRepository handler and every Repository-shaped response (DescribeRepositories, DeleteRepository, and the registry URI used for GetAuthorizationToken's proxyEndpoint, which must agree with it) must mint an authority the emulator actually serves while keeping the registry/account/region identity in it — i.e. resolve the host from the serving endpoint configuration rather than appending a synthetic `.localhost` TLD to a real-AWS registry name. The same serializer must be the single source for the registry authority so the auth endpoint and the repository URI cannot disagree.

Traces: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

## Evidence
Suite run (180/203 passed, 88.7%): service codebuild__ecr__ecs__elbv2__logs scored 0/2 (0%), 2 failed. Neither failure is marked ALREADY FILED or OWNED.

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image
  File "check_..._cicdchain_alb_body_tracks_the_latest_codebuild_image.py", line 44, in run
    assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
  AssertionError: the emulator published https://000000000000.dkr.ecr.us-west-2.localhost/sim2real-xsvc-ecr-280b8f3f5af2: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:33303/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
  File "check_..._cicdchain_unhealthy_build_rollout_rejected_and_old_bytes_still_served.py", line 46, in run
    assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
  AssertionError: the emulator published https://000000000000.dkr.ecr.us-west-2.localhost/sim2real-xsvc-ecr-b37d5ad1d3c5: its host is not one of the emulator's own hosts; ... needs http(s)://127.0.0.1:38397/<resource path> ...

Expected (AWS): CreateRepository answers 200 with repository.repositoryUri whose authority carries the registry identity for the caller's account and region AND is a host the serving endpoint answers on, so the same string works for `docker push`/`docker pull` and as an ECS task-definition image reference (on real AWS: <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>).

Actual (emulator): 200 with repositoryUri `<registryId>.dkr.ecr.<region>.localhost/<repositoryName>` — e.g. `000000000000.dkr.ecr.us-west-2.localhost/<repo>`. The fixed text of the divergence is the authority pattern `<registryId>.dkr.ecr.<region>.localhost`; the account id (000000000000), region (us-west-2), repository name and the harness's loopback port (33303 / 38397) are per-run values from the request and this run's listener, not requirements. No HTTP error is raised — the defect is the value of the published address, and it makes the host unreachable rather than returning an error code.

## Steps to Reproduce
1. Point an AWS client at the emulator (note the host:port the emulator is actually served on).
2. Call ecr:CreateRepository with any repositoryName in region us-west-2.
3. Read `repository.repositoryUri` from the 200 response.
   -> AWS: the URI's authority is the account's ECR registry host for that region and is a host that serves the registry (it can be dialed and used for docker push/pull).
   -> Emulator: the URI is `<registryId>.dkr.ecr.<region>.localhost/<repositoryName>` — a host the emulator does not serve and that does not resolve; no push/pull or ECS image pull through it can succeed.
4. Confirm by using that URI as a docker registry target, or as the `image` of a RegisterTaskDefinition container definition pinned by digest: nothing listens on the announced host.
5. Re-replay checks codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image and codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served to confirm the fix (they assert the published URL before any build step).
