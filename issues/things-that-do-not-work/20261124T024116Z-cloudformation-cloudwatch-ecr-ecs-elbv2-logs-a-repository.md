Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period|_assert_children_are_real_stacks#28|AssertionError|assert (labels[:4] == [repo["registryId"], "dkr", "ecr", ctx.region] and len(labels) > 4
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow|_assert_children_are_real_stacks#28|AssertionError|assert (labels[:4] == [repo["registryId"], "dkr", "ecr", ctx.region] and len(labels) > 4
Issue-Commit: 7a64cae84a9cfda7163325030ecd9d33dd5ead34

# cloudformation__cloudwatch__ecr__ecs__elbv2__logs: a repository provisioned by a nested stack reads back with repositoryUri set to the emulator's bare host:port instead of a <registryId>.dkr.ecr.<region> registry authority

> _Note: this issue was identified against the code base at commit `7a64cae84a9cfda7163325030ecd9d33dd5ead34`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRepositories

## Description
Cause 1 of 1: in the nested-stack microservice-platform scenario, the AWS::ECR::Repository the service child stack provisions is read back through ECR itself and its `repositoryUri` has no ECR registry authority at all — it is the emulator's bare socket address `<host>:<port>` followed by the repository name. Failing checks: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period and cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow.

AWS: Smithy (services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921) `Repository$repositoryUri` — "The URI for the repository. You can use this URI for container image `push` and `pull` operations." AWS composes it as `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`. FIXED: the label ordering and the literal labels `dkr` and `ecr` in the positions `<registryId>.dkr.ecr.<region>.`, no port component, and a path equal to `Repository$repositoryName`. VARIABLE, taken from state and the request: the 12-digit `registryId` of the owning registry (the same value the body reports in `Repository$registryId`), the region of the call, and the repository name. Both checks assert exactly that rule — the authority's first four DNS labels must be `[registryId, "dkr", "ecr", region]` followed by at least one further non-empty suffix label — so an emulator-served DNS suffix is acceptable and `amazonaws.com` is not required.

Emulator: the nested stacks deploy and the three children reach CREATE_COMPLETE with their expected physical resources, and DescribeRepositories answers HTTP 200 for the stack-created repository with the declared `imageScanningConfiguration.scanOnPush == true` — but `$.repositories[0].repositoryUri` is the emulator's listening socket plus the name (recorded runs `127.0.0.1:39489/sim2real-xsvc-repo-202c35649912` and `127.0.0.1:39279/sim2real-xsvc-repo-2c7f76df9ebe`). No registry-id, `dkr`, `ecr` or region label is present and an explicit port is. Both checks abort there, in the "children" leg, so everything they assert afterwards (cross-stack Fn::GetAtt reference equality, the lifecycle policy text, the CloudWatch alarm echoing its declared Period, and the rollback legs) is never reached and is not claimed as diverging here.

Why: a nested-stack platform exists so downstream resources address the repository the stack made. A client that takes the URI from the API's own answer and feeds it to an ECS task definition's `image`, a buildspec push target or a `docker push` reaches the emulator's control-plane socket rather than a registry, and cannot recover the registry id or the region from the value for cross-account/cross-region or policy-condition logic. Because the authority is the per-run socket, task definitions and templates derived from it are not reproducible between runs.

Fix: the ECR Repository serializer / the component that computes `repositoryUri` from state (registry id, region, repository name) must emit a registry DNS authority of the documented shape, and the authority it announces must also be one the emulator answers on — both halves together, not one traded for the other. This is the same field and the same serializer named by the open ECR issues 20261123T002251Z-ecr-describerepositories-reports-repositoryuri-as-the-emulator-s.md and 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md, and by the open issue 20261118T072013Z-batch-ecr-lambda-logs-ecr-createrepository-mints-a-repositoryuri-with-no-region.md; none of those carries either of these two checks' replay keys, so these nested-stack checks are not gated by them.

Traces: cfn-nested-stack-microservice-platform-alarm-echoes-declared-period (assertion at line 781 of its check file, reached from the "children" step) and cfn-nested-stack-microservice-platform-workflow (same assertion at line 785 of its check file).

## Evidence
Suite: service `cloudformation__cloudwatch__ecr__ecs__elbv2__logs` scored 0/2 (0%), 2 failed; neither is owned by an open issue. Both fail on the identical assertion over the identical field.

[mismatch] cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period
  legs.step("children", ...) -> _assert_children_are_real_stacks, line 781:
    repo = ecr.describe_repositories(repositoryNames=[repo_name])["repositories"][0]
    host, _, path = repo["repositoryUri"].partition("/"); labels = host.split(".")
    assert (labels[:4] == [repo["registryId"], "dkr", "ecr", ctx.region] and len(labels) > 4 and all(labels[4:]) and path == repo_name)
  AssertionError: ('127.0.0.1:39489/sim2real-xsvc-repo-202c35649912', 'sim2real-xsvc-repo-202c35649912')

[mismatch] cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow
  same assertion, line 785:
  AssertionError: ('127.0.0.1:39279/sim2real-xsvc-repo-2c7f76df9ebe', 'sim2real-xsvc-repo-2c7f76df9ebe')

Expected (AWS): HTTP 200 and `$.repositories[0].repositoryUri` == `<registryId>.dkr.ecr.<region>.<dns-suffix>/<repositoryName>`; first four authority labels are the body's own `registryId`, `dkr`, `ecr` and the request region, then a non-empty suffix, no port, path == the repository name the stack declared.
Actual (emulator): HTTP 200 and `$.repositories[0].repositoryUri` == `<emulator-host>:<emulator-port>/<repositoryName>`; `labels[:4]` is `['127','0','0','1:39489']`.

No status, error code or error message divergence is involved: the 200 body is wrong. The host, port and generated repository name of the recorded runs are per-run values filling the template, not requirements. The nested-stack machinery itself is not implicated — the parent and all three children reached CREATE_COMPLETE with their expected resources and the scanOnPush setting round-tripped.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateStack the nested-stack platform parent (platform-parent.yaml, with the network/service/observability children), where the service child declares an AWS::ECR::Repository with ImageScanningConfiguration.ScanOnPush: true; wait for CREATE_COMPLETE.
3. Call ecr:DescribeRepositories with repositoryNames=[the repository the service child created] and partition `$.repositories[0].repositoryUri` on the first '/'.
   -> AWS: the authority's labels are `<registryId>`, `dkr`, `ecr`, `<region>`, then a DNS suffix, with no port; the path is the declared repository name.
   -> Emulator: the authority is the emulator's bare `127.0.0.1:<port>`.
4. Use that URI as an ECS task definition `image` or as a `docker push` target: it addresses the emulator's control-plane socket, not a registry, and the registry id and region cannot be parsed out of it.
5. Re-replay both checks in trace_ids to confirm the fix (they continue past this assertion into the cross-stack reference, lifecycle-policy and alarm legs).
