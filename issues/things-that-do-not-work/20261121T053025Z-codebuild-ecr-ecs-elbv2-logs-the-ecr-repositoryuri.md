Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Commit: 2f896de1ccf99b872a06872b90b77c54f78fc307

# codebuild__ecr__ecs__elbv2__logs: the ECR repositoryUri announced to the container CI-CD chain is a real-AWS dkr.ecr host the emulator does not serve, so no push/pull target exists (1 cause)

> _Note: this issue was identified against the code base at commit `2f896de1ccf99b872a06872b90b77c54f78fc307`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: the ECR repository URI the emulator hands the CodeBuild -> ECR -> ECS/Fargate -> ALB chain is minted on the public AWS registry DNS authority, which this emulator neither serves nor resolves. Both container-chain checks now get past the harness budget and die on the very first observable step — the published-URL check on the repositoryUri returned by the repository-creation call — so nothing downstream (the docker push, the image digest, the digest-pinned task definition, the ECS rollout, the ALB-served body, the circuit-breaker rollback) is reachable at all. Failing checks: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image and codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

AWS: Smithy (com.amazonaws.ecr): Repository.repositoryUri (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." AWS composes it as <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>, where the fixed text is '.dkr.ecr.' and '.amazonaws.com', registryId is Repository.registryId (pattern ^[0-9]{12}$) and the path segment is exactly Repository.repositoryName. The load-bearing property on AWS is not the spelling but that this authority IS the registry that serves the Docker Registry v2 API for that account and region: the client that received the URI can log in, push and pull against it. No error code or non-2xx is involved — the create call succeeds on AWS and on the emulator; the model says nothing further, so for the reachability requirement this is recorded behavior only: in the recording the URI the API returns is the one the subsequent push/pull actually uses.

Emulator: the repository-creation call answers 2xx with a repositoryUri whose authority is the literal public AWS registry name for the account/region — recorded as 'https://000000000000.dkr.ecr.us-west-2.amazonaws.com/<repositoryName>' (the 12-digit id, the region and the generated repository name are per-run values from state/the request, not part of the requirement). That host is not one of the emulator's own hosts and is not reachable in the emulator's environment; the harness reports 'the emulator published https://<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://<emulator host>:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'. Both checks fail on that assertion before any build is started.

Why: a client that does exactly what the field documents — derive a `docker login`/`docker push` target, a buildspec IMAGE_REPO, an ECS task-definition image of the form <repositoryUri>@<digest>, or a Lambda ImageUri from the value the API just returned — is sent off the emulator entirely. The name either fails to resolve or reaches real AWS, where the repository does not exist, so every image-push and deploy-by-digest flow built from the API's own answer breaks even though the control-plane call returned 2xx. In this chain it means the whole CodeBuild->ECR->ECS->ALB topology is unexercisable and the client gets no diagnosis, only an unusable endpoint.

Fix: the ECR Repository serializer / the component that computes repositoryUri from state (registry id, region, repository name), plus the registry endpoint it names — the emulator must announce a registry authority it actually answers on (publish the registry-DNS form AND serve/alias that host, or publish its own served registry endpoint) rather than an authority that is neither served nor resolvable. The same minting path feeds GetAuthorizationToken's proxyEndpoint and the registry host used by the CodeBuild build container.

Note on prior work: a FIXED issue claimed exactly this cause for this grouping — 20261109T141609Z-codebuild-ecr-ecs-elbv2-logs-the-ecr-repository.md ("the ECR repository URI announced to the CI-CD chain is a real-AWS dkr.ecr host the emulator does not serve") — and the behavior persists unchanged, so that fix did not land for this path. Related open issues carry other replay keys and do not own these two checks: 20261116T122519Z-codebuild-ecr-ecs-elbv2-logs-both-container-ci.md (the 60 s budget-exhaustion variant of the same two checks), 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md (the wire-corpus keys for CreateRepository/DeleteRepository/DescribeRepositories), 20261118T005003Z-ecr-getauthorizationtoken-publishes-a-proxyendpoint-on-a-host.md, 20261105T082918Z-codebuild-ecr-ecs-elbv2-logs-the-circuit-breaker.md.

Traces: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

## Evidence
Suite: service codebuild__ecr__ecs__elbv2__logs scored 0/2 (0%), 2 failed. Both failures are the same assertion, at the first observable step of each check.

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image
  check_..._cicdchain_alb_body_tracks_the_latest_codebuild_image.py, line 44, in run:
    assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
  AssertionError: the emulator published https://000000000000.dkr.ecr.us-west-2.amazonaws.com/sim2real-xsvc-ecr-cd07121a7785: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:40995/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
  check_..._cicdchain_unhealthy_build_rollout_rejected_and_old_bytes_still_served.py, line 46, in run:
    assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
  AssertionError: the emulator published https://000000000000.dkr.ecr.us-west-2.amazonaws.com/sim2real-xsvc-ecr-0dd6b86dfc5a: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34713/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Expected (AWS): the repository-creation response's repositoryUri is the authority that serves the registry for that account/region, so the value returned can be used verbatim for `docker login`, `docker push`, `docker pull` and as the <repositoryUri>@<digest> image of an ECS task definition; the chain then proceeds (build SUCCEEDED, a sha256: digest in ECR, rolloutState COMPLETED and the baked body served through the ALB; rolloutState FAILED with rollback and the previous bytes still served for the unhealthy revision).
Actual (emulator): 2xx with repositoryUri on the host <registryId>.dkr.ecr.<region>.amazonaws.com, which the emulator does not serve and which does not resolve to it. No status or error-code divergence �� the body value is wrong/unreachable, not the verdict. The 12-digit registry id, the region, the generated repository names (sim2real-xsvc-ecr-*) and the loopback ports (40995 / 34713) are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Point an AWS client at the emulator's ECR endpoint and create a repository with a valid repositoryName.
2. Read $.repository.repositoryUri and split it on the first '/': the authority is the public AWS ECR registry DNS name (<12-digit id>.dkr.ecr.<region>.amazonaws.com), not an address the emulator answers on.
3. Try to use it as the field documents — resolve it, `docker login` against it, or set it as a buildspec push target — and it does not reach the emulator that just created the repository.
4. Build the rest of the chain (build/task log groups, a codebuild.amazonaws.com role with ECR push permissions, a project whose buildspec pushes to that URI, a public VPC + ALB + target group, a Fargate service pinned to <repositoryUri>@<digest>): there is no registry to push to, so no image, no digest, no rollout and no ALB body are ever produced.
5. Re-replay checks codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image and codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served to confirm the fix.
