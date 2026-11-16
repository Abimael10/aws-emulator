Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-daemon-and-express-gateway-error-paths
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-daemon-and-express-gateway-error-paths ecs-daemon-error-paths-and-empty-listings
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|ListDaemonDeployments|2xx->4xx|$.daemonDeployments
Issue-Key: ecs|ListDaemonDeployments|2xx->4xx|status
Issue-Key: ecs|DescribeDaemonDeployments|4xx->4xx|$.__type
Issue-Key: ecs|DescribeDaemonDeployments|4xx->4xx|$.message
Issue-Key: ecs|DescribeDaemonRevisions|4xx->4xx|$.__type
Issue-Key: ecs|DescribeDaemonRevisions|4xx->4xx|$.message
Issue-Key: ecs|UpdateDaemon|4xx->4xx|$.__type
Issue-Key: ecs|UpdateDaemon|4xx->4xx|$.message
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.__type
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.message
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|status
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|$.__type
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|$.message
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|status
Issue-Key: ecs|DeleteDaemon|4xx->4xx|$.message
Issue-Commit: 2936d3a144ae5eebae9f6b7387af703153caab29

# ECS daemon deployment and revision reads misclassify refusals

> _Note: this issue was identified against the code base at commit `2936d3a144ae5eebae9f6b7387af703153caab29`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDaemonDeployments

## Description
Seven net-new root causes on the ECS daemon deployment/revision/update/delete operations. They share one fault: these handlers resolve the cluster (or nothing at all) instead of validating the daemon/deployment/revision ARN the request actually carries, so a parameter fault is reported as a missing cluster, an empty read is refused, an invalid read is accepted, and one refusal is emulator-worded. All seven must be closed before submitting.

Cause 1 of 7: ListDaemonDeployments (fixture ecs-daemon-and-express-gateway-error-paths, step task#4).
AWS: ListDaemonDeployments is a read (Smithy: ECS 2014-11-13 ListDaemonDeployments, readonly, input daemonArn @required, output daemonDeployments -> DaemonDeploymentSummaryList). When the named daemon has no deployments to list, AWS answers HTTP 200 with the modeled list present and empty: daemonDeployments: []. It is not an error path -- the operation's modeled errors (ClientException, ClusterNotFoundException, InvalidParameterException, UnsupportedFeatureException, all 400) describe bad input or a missing cluster, not an empty result set.
Emulator: HTTP 400, and no daemonDeployments member in the body at all.
Why: a client that enumerates a daemon's deployments before the first one exists gets a hard failure instead of zero rows, so discovery-then-create and pagination loops abort rather than proceeding; the "no deployments yet" state is unrepresentable.
Fix: the ListDaemonDeployments handler -- serve 200 with an empty daemonDeployments list for a resolvable daemon that has no deployments, and reserve the 400s for bad input.

Cause 2 of 7: DescribeDaemonDeployments (fixture ecs-daemon-and-express-gateway-error-paths, step task#5).
AWS: Smithy models daemonDeploymentArns as @required on DescribeDaemonDeploymentsRequest, with InvalidParameterException (400) and ClusterNotFoundException (400) as separate modeled errors. When the supplied deployment ARN list is not usable, the refusal is HTTP 400 with error code InvalidParameterException and message 'Request parameter(s) are invalid.' -- that whole string is fixed text for this error class, with no request- or state-derived parts.
Emulator: HTTP 400 but with __type 'ClusterNotFoundException' and message 'Cluster not found.'
Why: the request's defect is in the deployment ARN, not the cluster. A client branching on ClusterNotFoundException to create the cluster and retry takes a branch that can never succeed, and code handling InvalidParameterException to correct the ARN never runs.
Fix: DescribeDaemonDeployments' validation stage -- validate the daemonDeploymentArns members and raise InvalidParameterException before any cluster resolution.

Cause 3 of 7: DescribeDaemonRevisions (fixture ecs-daemon-and-express-gateway-error-paths, step task#6).
AWS: Smithy models daemonRevisionArns as @required on DescribeDaemonRevisionsRequest, with InvalidParameterException (400) among its errors. For a revision ARN that is not a valid daemon-revision ARN, the refusal is HTTP 400, error code InvalidParameterException, message 'The Daemon Revision ARN is invalid.' -- fixed text for this error class, no variable parts.
Emulator: HTTP 400 with __type 'ClusterNotFoundException' and message 'Cluster not found.'
Why: same as cause 2 -- an ARN-syntax programming error is reported as a missing resource, so the caller is pointed at the wrong field and the modeled error class it handles never arrives.
Fix: DescribeDaemonRevisions' validation stage -- validate each daemonRevisionArns member ahead of cluster lookup.

Cause 4 of 7: UpdateDaemon (fixture ecs-daemon-and-express-gateway-error-paths, step task#7).
AWS: Smithy models UpdateDaemon with daemonArn, daemonTaskDefinitionArn and capacityProviderArns all @required, and lists ClientException, DaemonNotFoundException and ClusterNotFoundException as distinct 400 errors. When the referenced daemon task definition does not exist, the refusal is HTTP 400, error code ClientException, message 'DaemonTaskDefinition not found.' -- fixed text.
Emulator: HTTP 400 with __type 'ClusterNotFoundException' and message 'Cluster not found.'
Why: the caller is told the cluster is missing when the task definition is; it cannot distinguish "register the task definition" from "create the cluster", and ClientException handling never fires.
Fix: UpdateDaemon's validation order -- resolve the daemon task definition and report its absence as ClientException before falling back to a cluster-not-found refusal. The open issue 20261029T220901Z-ecs-express-gateway-and-daemon-operations-skip-validation.md carries this same misclassification for CreateDaemon, but does not carry UpdateDaemon's replay key, so it does not cover this cause.

Cause 5 of 7: DescribeDaemonDeployments (fixture ecs-daemon-error-paths-and-empty-listings, step task#6).
AWS: as in cause 2, an unusable daemonDeploymentArns input is REFUSED -- HTTP 400, error code InvalidParameterException, message 'Request parameter(s) are invalid.' (fixed text).
Emulator: HTTP 200 with no __type and no message -- the invalid read is accepted and answered as a success.
Why: this is the inverse failure of cause 2 and the more dangerous half: a negative test of parameter validation passes wrongly, and a client that passed a malformed ARN gets a success response it will read as "this deployment has nothing to report" rather than "your request was invalid".
Fix: DescribeDaemonDeployments' validation -- the ARN-list check must reject, not pass through to a 200.

Cause 6 of 7: DescribeDaemonRevisions (fixture ecs-daemon-error-paths-and-empty-listings, step task#7).
AWS: as in cause 3, an invalid daemon-revision ARN is refused -- HTTP 400, error code InvalidParameterException, message 'The Daemon Revision ARN is invalid.' (fixed text).
Emulator: HTTP 200 with no __type and no message.
Why: as cause 5 -- invalid input is silently accepted, so callers and negative tests both conclude the ARN was fine.
Fix: DescribeDaemonRevisions' validation -- reject invalid daemonRevisionArns members instead of answering 200.

Cause 7 of 7: DeleteDaemon (fixture ecs-daemon-error-paths-and-empty-listings, step task#10).
AWS: Smithy models DeleteDaemon with daemonArn @required and DaemonNotFoundException/ClientException among its 400 errors. When no daemon metadata backs the named daemon, the refusal message is 'No daemon metadata found.' -- the entire string is fixed text for this error class; AWS does not interpolate the requested ARN into it.
Emulator: HTTP 400 with message 'Daemon not found: <daemonArn>' -- emulator-authored wording that additionally interpolates the request's daemon ARN (the recorded run shows one such ARN; the ARN value itself is per-run and is not part of the requirement).
Why: a client or test matching AWS's refusal text does not recognize the failure, and log/alert rules keyed on AWS's message miss it.
Fix: DeleteDaemon's error message -- emit AWS's fixed text for this error class.

Traces: ecs-daemon-and-express-gateway-error-paths#task#4 (cause 1, 2 affected steps), #task#5 (cause 2), #task#6 (cause 3), #task#7 (cause 4, 2 affected steps); ecs-daemon-error-paths-and-empty-listings#task#6 (cause 5), #task#7 (cause 6), #task#10 (cause 7).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, 1380 steps, 1226 passed):

- ListDaemonDeployments (ecs-daemon-and-express-gateway-error-paths#task#4, also ecs-daemon-error-paths-and-empty-listings#task#5): status expected 200, got 400; $.daemonDeployments expected [], got absent.
- DescribeDaemonDeployments (ecs-daemon-and-express-gateway-error-paths#task#5), 400/400: $.__type expected 'InvalidParameterException', got 'ClusterNotFoundException'; $.message expected 'Request parameter(s) are invalid.', got 'Cluster not found.'
- DescribeDaemonRevisions (ecs-daemon-and-express-gateway-error-paths#task#6), 400/400: $.__type expected 'InvalidParameterException', got 'ClusterNotFoundException'; $.message expected 'The Daemon Revision ARN is invalid.', got 'Cluster not found.'
- UpdateDaemon (ecs-daemon-and-express-gateway-error-paths#task#7, also ecs-daemon-error-paths-and-empty-listings#task#9), 400/400: $.__type expected 'ClientException', got 'ClusterNotFoundException'; $.message expected 'DaemonTaskDefinition not found.', got 'Cluster not found.'
- DescribeDaemonDeployments (ecs-daemon-error-paths-and-empty-listings#task#6): status expected 400, got 200; $.__type expected 'InvalidParameterException', got absent; $.message expected 'Request parameter(s) are invalid.', got absent.
- DescribeDaemonRevisions (ecs-daemon-error-paths-and-empty-listings#task#7): status expected 400, got 200; $.__type expected 'InvalidParameterException', got absent; $.message expected 'The Daemon Revision ARN is invalid.', got absent.
- DeleteDaemon (ecs-daemon-error-paths-and-empty-listings#task#10), 400/400: $.message expected 'No daemon metadata found.', got 'Daemon not found: arn:aws:ecs:<region>:<account>:daemon/<cluster>/<daemon>' (the emulator interpolates the requested ARN; the recorded ARN value is per-run and not itself a requirement).

Smithy (ECS 2014-11-13, com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113):
- ListDaemonDeployments: readonly; input daemonArn @required; output daemonDeployments -> DaemonDeploymentSummaryList, nextToken. Errors: AccessDeniedException 403, ClientException 400, ClusterNotFoundException 400, InvalidParameterException 400, UnsupportedFeatureException 400, ServerException 500.
- DescribeDaemonDeployments: readonly; input daemonDeploymentArns @required (StringList); output daemonDeployments, failures. Errors include InvalidParameterException 400 and ClusterNotFoundException 400 as distinct classes.
- DescribeDaemonRevisions: readonly; input daemonRevisionArns @required (StringList); output daemonRevisions, failures. Same error set.
- UpdateDaemon: input daemonArn, daemonTaskDefinitionArn, capacityProviderArns all @required. Errors include ClientException 400, DaemonNotFoundException 400, DaemonNotActiveException 400, ClusterNotFoundException 400.
- DeleteDaemon: idempotent; input daemonArn @required. Errors include ClientException 400, DaemonNotFoundException 400, ClusterNotFoundException 400.

## Steps to Reproduce
1. git_clone, then replay_wire_corpus() and read the ecs daemon clusters (grep the divergence lines for '"operation": "ListDaemonDeployments"', 'DescribeDaemonDeployments', 'DescribeDaemonRevisions', 'UpdateDaemon', 'DeleteDaemon').
2. Cause 1 -- ListDaemonDeployments with a valid daemonArn that has no deployments (ecs-daemon-and-express-gateway-error-paths#task#4): emulator answers 400 instead of 200 with daemonDeployments: [].
3. Cause 2 -- DescribeDaemonDeployments with an unusable daemonDeploymentArns entry (same fixture, task#5): emulator answers ClusterNotFoundException / 'Cluster not found.' instead of InvalidParameterException / 'Request parameter(s) are invalid.'
4. Cause 3 -- DescribeDaemonRevisions with an invalid daemon-revision ARN (same fixture, task#6): emulator answers ClusterNotFoundException / 'Cluster not found.' instead of InvalidParameterException / 'The Daemon Revision ARN is invalid.'
5. Cause 4 -- UpdateDaemon referencing a daemon task definition that does not exist (same fixture, task#7): emulator answers ClusterNotFoundException / 'Cluster not found.' instead of ClientException / 'DaemonTaskDefinition not found.'
6. Cause 5 -- DescribeDaemonDeployments on the ecs-daemon-error-paths-and-empty-listings fixture, step task#6: emulator answers 200 where AWS refuses with 400 InvalidParameterException / 'Request parameter(s) are invalid.'
7. Cause 6 -- DescribeDaemonRevisions on the same fixture, step task#7: emulator answers 200 where AWS refuses with 400 InvalidParameterException / 'The Daemon Revision ARN is invalid.'
8. Cause 7 -- DeleteDaemon on the same fixture, step task#10: emulator's 400 message is 'Daemon not found: <arn>' instead of AWS's fixed 'No daemon metadata found.'
9. Re-replay all seven fixture steps after fixing; each must match the recorded status, error code and message.
