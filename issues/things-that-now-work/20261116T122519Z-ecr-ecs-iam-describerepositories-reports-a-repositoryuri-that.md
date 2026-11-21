Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle
Issue-Key: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 80ea869c5d431cbbd6e55b502825120813eb8704

# ecr__ecs__iam: DescribeRepositories reports a repositoryUri that does not address the repository in the caller's account and region — it is the emulator's bare host:port (1 cause)

> _Note: this issue was identified against the code base at commit `80ea869c5d431cbbd6e55b502825120813eb8704`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: in the ECR-access diagnosis flow, the repository URI resolved from the read path (`DescribeRepositories` on the repository backing the Fargate service's image) is the emulator's bare socket address rather than the account- and region-qualified ECR registry authority, so the solve phase cannot establish that the URI addresses the named repository in this account and region. Failing check: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle (phase 2/4 'solve').

AWS: Smithy: ecr `Repository$repositoryUri` (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." AWS composes it as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`: the registry id label is `Repository$registryId` (the 12-digit owning account), the region label is the region of the call, the `dkr` and `ecr` labels and their ordering are fixed text, and the path component is exactly `Repository$repositoryName`. That is what makes the URI self-describing: a client can read the owning account, the region and the repository name back out of it. The account id, region and repository name in any one run are values from the request and the registry's state, not part of the requirement.

Emulator: `DescribeRepositories` answers 200 with `repositoryUri` = `127.0.0.1:42889/sim2real-ecracc-e2e-<id>-repository-<id>` — the host:port the emulator is bound to in that run. There is no registry-id label, no `dkr`/`ecr` labels and no region label, so the URI does not address the repository in the caller's account and region at all; the check's solve phase fails with "the resolved repository URI '127.0.0.1:42889/<repo>' must address '<repo>' in this account and region".

Why: the whole point of an ECR access diagnosis is to read, from the task definition's image and the repository read path, which account and region own the image and compare them to the caller. A client (or an operator following AWS's own ECR-access runbook) that parses `repositoryUri` to extract the registry id and region gets nothing back, so cross-account and cross-region pull diagnoses, `aws:PrincipalAccount`-guarded repository policies, and any `docker login`-by-registry flow derived from the field all fail. The value also changes with the emulator's port on every run, so nothing derived from it is reproducible.

Fix: the ECR Repository serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and that host must be one the emulator answers on. Related open issues cover the same field for other service groupings or for the wire corpus but none carries this check's replay key: 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md, 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, 20261111T010150Z-ecr-ecs-logs-ecr-pull-through-cache-repository-uri-is-not.md, 20261115T024314Z-batch-ecr-lambda-logs-ecr-repositoryuri-is-not-region-qualified-so-the.md, and 20261109T141609Z-ecr-ecs-iam-the-ecr-access-e2e-deploy.md (which covers this check's DEPLOY phase LifecyclePolicy gap, a different cause from this solve-phase URI shape).

Traces: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle.

## Evidence
Suite: service ecr__ecs__iam scored 11/12 (92%), 1 failed — this check, not owned by any filed issue.

[mismatch] ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle
  check_ecr__ecs__iam_e2e_ecracc_ecs_services_ecr_access_check_lifecycle.py line 156 (phase wrapper):
  AssertionError: [phase 2/4 solve] the resolved repository URI '127.0.0.1:42889/sim2real-ecracc-e2e-62e44af2d34d-repository-35637e690a37' must address 'sim2real-ecracc-e2e-62e44af2d34d-repository-35637e690a37' in this account and region

The graded solve trajectory is (ecs:ListServices, ecs:DescribeServices, ecs:DescribeTaskDefinition, iam:ListAttachedRolePolicies, iam:ListRolePolicies, ecr:DescribeRepositories); the divergence is in the body of the last call.

Expected (AWS): HTTP 200 from DescribeRepositories with `$.repositories[0].repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`, from which the caller's account id and the call's region are recoverable, and whose path equals the requested repository name.
Actual (emulator): HTTP 200 with `$.repositories[0].repositoryUri` == `127.0.0.1:42889/<repositoryName>` — a bare host:port; no registry id label, no `dkr`/`ecr` labels, no region label, and an explicit port.
No status or error-code divergence. The specific account id, region, repository name and port of the recorded run are per-run values; the requirement is the label structure.

## Steps to Reproduce
1. Deploy the ECR-access stack (ECR repository + Fargate service + task execution role) against the emulator.
2. Run the diagnosis trajectory: ecs:ListServices -> ecs:DescribeServices -> ecs:DescribeTaskDefinition -> iam:ListAttachedRolePolicies -> iam:ListRolePolicies -> ecr:DescribeRepositories for the repository named by the task definition's image.
3. Read `$.repositories[0].repositoryUri` and parse the authority:
   -> AWS: `<accountId>.dkr.ecr.<region>.<dns-suffix>` — the owning account and the region are readable from the labels, path == repository name.
   -> Emulator: `127.0.0.1:<port>` — nothing identifies the account, region or registry.
4. Re-replay check ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle to confirm the solve phase passes.
