Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected
Issue-Key: cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected|run#20|AssertionError|assert (labels[:4] == [account_id, "dkr", "ecr", ctx.region] and len(labels) > 4
Issue-Commit: 80ea869c5d431cbbd6e55b502825120813eb8704

# cloudwatch__ecr__ecs: the ECR repositoryUri handed to a CFN-deployed ECS task definition is a bare host:port, not an <registryId>.dkr.ecr.<region> registry authority (1 cause)

> _Note: this issue was identified against the code base at commit `80ea869c5d431cbbd6e55b502825120813eb8704`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: the `repositoryUri` the emulator puts in an ECR Repository body has no ECR registry authority at all — it is the emulator's bare socket address (`<host>:<port>`), so every artifact derived from it (here, the image of the stack's ECS task definition) carries a URI that is not an ECR registry name for the caller's account and region. Failing check: cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected.

AWS: Smithy: ecr `Repository$repositoryUri` (target Url) — "The URI for the repository. You can use this URI for container image push and pull operations." AWS composes it as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`, where `registryId` is `Repository$registryId` (12-digit account id) and `region` is the region the call was made in; the fixed parts are the `dkr` and `ecr` labels and the ordering `<registryId>.dkr.ecr.<region>.`, and the path component is exactly `Repository$repositoryName`. The account id, region and repository name are values taken from the request and the registry's state — they are not part of the requirement. The check asserts exactly that structural rule (first four DNS labels == [accountId, "dkr", "ecr", region], with at least one further label for the DNS suffix), which permits an emulator-served domain suffix; it does not require `amazonaws.com`.

Emulator: CreateRepository/DescribeRepositories answer 200 but the `repositoryUri` is `127.0.0.1:36063/sim2real-xsvc-repo-<suffix>` (the socket the emulator happens to be bound to in that run). The value has no `dkr`/`ecr` labels, no registry id label and no region label, and carries an explicit port. The ECS task definition the CloudFormation stack registers therefore reports image `127.0.0.1:36063/<repo>:latest` where AWS reports `<accountId>.dkr.ecr.<region>.<dns-suffix>/<repo>:latest`.

Why: a client that does what the field documents — `docker login`/`docker push`/`docker pull` against the URI, or deriving an ECS task-definition image, a Lambda ImageUri or a buildspec push target from it — cannot recover the registry id or the region from the value, so any logic that parses the URI (cross-account/cross-region checks, registry-id extraction, policy conditions keyed on `aws:PrincipalAccount`, IaC templates that assert the image belongs to this account) misbehaves or crashes. The URI also varies per emulator run, so templates and task definitions are not reproducible.

Fix: the ECR Repository serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and the host it announces must be one the emulator answers on. Related open issues record the same field from other angles but none carries this check's replay key: 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md (wire; demands the announced authority be served), 20261111T021229Z-ecr-describerepositories-reports-a-repositoryuri-whose-host-is.md, 20261111T010150Z-ecr-ecs-logs-ecr-pull-through-cache-repository-uri-is-not.md and 20261115T024314Z-batch-ecr-lambda-logs-ecr-repositoryuri-is-not-region-qualified-so-the.md. The fix must satisfy both the shape requirement and the reachability requirement at once.

Traces: cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected.

## Evidence
Suite: service cloudwatch__ecr__ecs scored 11/27 (41%), 11 failed; this check is the one not already owned by a filed issue.

[mismatch] cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected
  check_cloudwatch__ecr__ecs_deploy_cfn_ecr_repository_policy_restricts_cross_account_pull_rejected.py line 97:
    assert (labels[:4] == [account_id, "dkr", "ecr", ctx.region] and len(labels) > 4 ...)
  AssertionError: expected task def image 000000000000.dkr.ecr.us-west-2.<domain>/sim2real-xsvc-repo-5f8f0c7749cb:latest, got '127.0.0.1:36063/sim2real-xsvc-repo-5f8f0c7749cb:latest'

Expected (AWS): HTTP 200, `$.repository.repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>` — first four DNS labels are the 12-digit registry id, `dkr`, `ecr` and the request region, followed by at least one suffix label, no port component; the stack's ECS task definition image is that URI plus `:latest`.
Actual (emulator): HTTP 200, `$.repository.repositoryUri` == `127.0.0.1:36063/<repositoryName>` — a bare host:port with none of the required labels; the task definition image is `127.0.0.1:36063/<repositoryName>:latest`.
No status or error-code divergence: the body is wrong, not the verdict. The 12-digit id, the region, the repository name and the port in the recorded run are per-run values filling the template, not part of the requirement.

## Steps to Reproduce
1. Point an AWS client at the emulator and call `sts:GetCallerIdentity` to obtain the account id.
2. CreateRepository with a valid repositoryName (or deploy a CloudFormation stack declaring `AWS::ECR::Repository` plus an `AWS::ECS::TaskDefinition` whose container image is built from `GetAtt <Repo>.RepositoryUri`).
3. Read `$.repository.repositoryUri` (and `DescribeRepositories` for the same name) and split the authority from the path on the first '/'.
   -> AWS: the authority's first four labels are `<accountId>`, `dkr`, `ecr`, `<region>`, followed by the DNS suffix, with no port.
   -> Emulator: the authority is the emulator's bare `127.0.0.1:<port>`.
4. DescribeTaskDefinition on the stack's task definition and read `containerDefinitions[0].image`: it carries the same bare host:port instead of the registry DNS name.
5. Re-replay check cloudwatch__ecr__ecs.deploy-cfn-ecr-repository-policy-restricts-cross-account-pull-rejected to confirm.
