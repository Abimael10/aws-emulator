Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served|run#2|AssertionError|assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}")), p
Issue-Commit: 78eeb93e75738c0cf0c5c47abcc6a48fc1935112

# ecr: repositoryUri announces <registryId>.dkr.ecr.<region>.localhost:<port>, a host the emulator does not serve, so the CI/CD chain cannot push or pull the image it was told to

> _Note: this issue was identified against the code base at commit `78eeb93e75738c0cf0c5c47abcc6a48fc1935112`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
1 net-new cause, failing both CI/CD-chain checks of this service group.

Cause 1 of 1: the repositoryUri the ECR API hands back names an authority the emulator does not answer on.
AWS: Smithy: ecr Repository.repositoryUri — 'The URI for the repository. You can use this URI for container image push and pull operations.' On AWS the value is <registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>, where registryId and repositoryName come from the request/state and the region is the service region; the fixed parts are '.dkr.ecr.' and the registry domain, and the defining property is that this authority IS the registry that serves the Docker Registry v2 API for that account — the URI is actionable by the client that received it. Nothing in the model licenses an authority the service does not serve.
Emulator: the Repository body (CreateRepository, and the DescribeRepositories read-back the checks use to drive the build) carries a repositoryUri whose host is '<registryId>.dkr.ecr.<region>.localhost:<port>' — a synthesized name that is not one of the emulator's own hosts and does not resolve to the listener the call was answered on. The harness flags it as a published-URL mismatch: 'its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'. The fixed part of the divergence is the announced authority: whatever shape is published, it must be a host the emulator serves (or that resolves to it) on the port it serves ECR on; the account id, region, port and repository name in any one run are values from state and are not the requirement.
Why: every step downstream of the control-plane call is driven by this URI. A buildspec that does `docker login`/`docker push` against it, a CodeBuild job that derives its image target from it, and an ECS task definition or ALB-backed service that pulls `<repositoryUri>:<tag>` are all sent to a name that does not resolve to the registry that just created the repository, so the image never lands and the service never serves the bytes the pipeline built — even though CreateRepository answered 200.
Fix: the ECR Repository serializer / the component that computes repositoryUri from state (registry id, region, repository name, serving endpoint) must emit an authority the emulator actually serves, and the registry endpoint must answer on that authority (host alias or per-resource listener). Related open issues describe the same field but different announced values and are not marked as owning these checks: 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md (wire corpus; the '.amazonaws.com' form, unserved), 20261124T024116Z-cloudformation-cloudwatch-ecr-ecs-elbv2-logs-a-repository.md and 20261123T002251Z-ecr-describerepositories-reports-repositoryuri-as-the-emulator-s.md (bare host:port with no registry authority). A fix must satisfy both the shape requirement and this reachability requirement.
Traces: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

## Evidence
Both checks fail at `assert not (p := ctx.published_url_mismatch(f"https://{repo_uri}"))`.

Expected (AWS): the repositoryUri handed back by the ECR API is usable for push/pull by the client that received it — i.e. its authority is the registry that answered the call (for the emulator: a host it serves, reached on the port the client is using).

Actual (emulator):
  cicdchain-alb-body-tracks-the-latest-codebuild-image: 'the emulator published https://000000000000.dkr.ecr.us-west-2.localhost:36505/sim2real-xsvc-ecr-19d8df855f1c: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:36505/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'
  cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served: same verdict, 'https://000000000000.dkr.ecr.us-west-2.localhost:38813/sim2real-xsvc-5bbc96335964'-shaped value.
The account id (000000000000), region, generated repository name and the port differ per run and are not the requirement; the invariant that fails is that the announced host is not one the emulator serves. No HTTP status or error code divergence — the control-plane call succeeds and the body is wrong.

## Steps to Reproduce
1. Point an AWS client at the emulator and run the CI/CD-chain flow: CreateRepository for a build target, then DescribeRepositories to discover where to push.
2. Read repository.repositoryUri and split it on the first '/': the authority is '<registryId>.dkr.ecr.<region>.localhost:<port>'.
3. Try to use it as the field documents — resolve it, or `docker login`/`docker push` against it: it is not one of the emulator's own hosts, so it does not reach the registry that just answered.
4. Drive the rest of the chain from that URI (CodeBuild push, ECS task-definition image, ALB-fronted service): the image never becomes pullable.
5. Re-replay checks cicdchain-alb-body-tracks-the-latest-codebuild-image and cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served to confirm the fix.
