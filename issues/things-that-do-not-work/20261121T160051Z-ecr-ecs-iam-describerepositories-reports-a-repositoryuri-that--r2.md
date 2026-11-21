Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle
Issue-Key: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# ecr__ecs__iam: DescribeRepositories reports a repositoryUri that is the emulator's bare host:port, so the ECR access walk cannot resolve the repository to this account and region

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
One net-new root cause, grounded in the only failing check the suite lists under `### ecr__ecs__iam` (marked neither ALREADY FILED nor OWNED). Related OPEN issues that do not carry this check's replay key and therefore do not cover it: 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md, 20261116T122519Z-cloudwatch-ecr-ecs-the-ecr-repositoryuri-handed-to.md, 20261118T072013Z-batch-ecr-lambda-logs-ecr-createrepository-mints-a-repositoryuri-with-no-region.md and 20261121T053025Z-codebuild-ecr-ecs-elbv2-logs-the-ecr-repositoryuri.md. Those make the same demand for other replay keys; this one is what the ECR-access lifecycle grades.

Cause 1 of 1: the repositoryUri in a DescribeRepositories response has no ECR registry authority — it is the emulator's own host:port — so the account id and region cannot be parsed out of it and the URI does not address the repository in this account and region.
AWS: Smithy: ecr Repository.repositoryUri — "The URI for the repository. You can use this URI for container image push and pull operations." AWS always forms it as `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`: the fixed parts are the `.dkr.ecr.` and `.amazonaws.com` registry-DNS form, and the variable parts are Repository.registryId (the 12-digit account id, pattern ^[0-9]{12}$), the service region and Repository.repositoryName, all taken from state/the request. The authority is the registry that serves the Docker Registry v2 API for that account, so the URI is actionable by the client that received it.
Emulator: DescribeRepositories answers 200 with repositoryUri `<emulator-host>:<port>/<repositoryName>` — e.g. the replay read `127.0.0.1:39255/sim2real-ecracc-e2e-<id>-repository-<acct>` where the path is right but the authority carries no registry id, no region and no `.dkr.ecr.`/`.amazonaws.com`. The specific host, port and generated repository name are environment values and are not the requirement; the missing registry-DNS authority is.
Why: the frozen six-operation ECR-access walk (ecs ListServices -> DescribeServices -> DescribeTaskDefinition -> iam ListAttachedRolePolicies/ListRolePolicies -> ecr DescribeRepositories) ends by splitting repositoryUri on the first '/' and requiring the host to begin `<account>.dkr.ecr.<region>.`; with a bare host:port it cannot confirm the repository belongs to the account and region under inspection. More broadly, a client cannot derive the registry id or region from the URI, cannot build the `ecr:` resource ARN or the `docker login` target from it, and any ECS task-definition image or buildspec target derived from the reported URI points at something that is not the account's ECR registry.
Fix: the ECR Repository serializer / the component that computes repositoryUri from state (registry id, region, repository name) for CreateRepository, DescribeRepositories and DeleteRepository — it must emit the `<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>` form, and (per 20261116T084727Z) that announced authority must also be one the emulator actually serves, so both the shape and the reachability requirement hold.
Traces: ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle (phase 2/4 solve).

## Evidence
Suite: service token `ecr__ecs__iam` 11/12 (92%), 1 failed.

Expected (AWS): `ecr describe-repositories --repository-names <repo> --query repositories[0].repositoryUri` returns `<12-digit account>.dkr.ecr.<region>.amazonaws.com/<repo>`; splitting on the first '/' yields a host starting `<account>.dkr.ecr.<region>.` and a path equal to the requested repository name.
Actual (mismatch): AssertionError "[phase 2/4 solve] the resolved repository URI '127.0.0.1:39255/sim2real-ecracc-e2e-5e6fe6bcdb96-repository-188917889075' must address 'sim2real-ecracc-e2e-5e6fe6bcdb96-repository-188917889075' in this account and region" — the path is the repository name, the authority is the emulator's bare host:port. No status or error-code divergence: the call answers 200 with a wrong body value.

## Steps to Reproduce
1. Deploy the ECR-access estate (an ECR repository, an ECS cluster with one Fargate service whose task definition has no execution role and a policy-free task role) and wait for CREATE_COMPLETE.
2. Call ecr DescribeRepositories with repositoryNames=[<the stack's repository name>].
3. Read repositories[0].repositoryUri and split it on the first '/': the host is the emulator's host:port instead of `<registryId>.dkr.ecr.<region>.amazonaws.com`, so the account id and region cannot be parsed and the URI does not address the repository in this account/region.
4. Re-replay check ecr__ecs__iam.e2e-ecracc-ecs-services-ecr-access-check-lifecycle.
