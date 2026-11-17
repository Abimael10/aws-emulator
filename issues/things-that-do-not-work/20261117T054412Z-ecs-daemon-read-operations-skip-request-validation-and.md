Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-daemon-error-paths-and-empty-listings
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-daemon-error-paths-and-empty-listings
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.__type
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|$.message
Issue-Key: ecs|DescribeDaemonDeployments|4xx->2xx|status
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|$.__type
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|$.message
Issue-Key: ecs|DescribeDaemonRevisions|4xx->2xx|status
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# ECS daemon read operations skip request validation and answer 200 where AWS refuses with InvalidParameterException

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDaemonDeployments

## Description
Two net-new root causes on the ECS daemon read plane: both DescribeDaemonDeployments and DescribeDaemonRevisions perform no validation of the ARN list they are handed, so a request AWS refuses outright is answered 200 with an empty/benign body. Both causes must be closed before submitting.

Cause 1 of 2: DescribeDaemonDeployments (fixture ecs-daemon-error-paths-and-empty-listings, step task#6).
AWS: a DescribeDaemonDeployments request whose daemonDeploymentArns member does not hold usable daemon-deployment ARNs is refused before any lookup — HTTP 400 with error code InvalidParameterException and message 'Request parameter(s) are invalid.' The whole message is fixed text for this error class; nothing in it is taken from the request. Smithy (ECS 2014-11-13, DescribeDaemonDeployments): input member daemonDeploymentArns -> StringList is @required, and the operation models InvalidParameterException (client error, HTTP 400) alongside ClientException / ClusterNotFoundException / UnsupportedFeatureException; the exact message text is recorded behavior only.
Emulator: HTTP 200 with no __type and no message — the malformed input is accepted and reported as a successful describe.
Why: a client relying on AWS's behavior cannot distinguish 'these ARNs are not valid' from 'these deployments do not exist'. Negative tests of the parameter contract pass wrongly, the InvalidParameterException branch a caller wrote is never taken, and a caller that would have corrected the ARN it built instead treats a 200 (with no deployments and no failures) as proof the deployment is gone and proceeds down the wrong path.
Fix: DescribeDaemonDeployments' validation stage in the ECS operation handler — validate the daemonDeploymentArns members before the state lookup and raise InvalidParameterException with AWS's fixed text.

Cause 2 of 2: DescribeDaemonRevisions (fixture ecs-daemon-error-paths-and-empty-listings, step task#7).
AWS: a DescribeDaemonRevisions request carrying a daemon-revision ARN that is not a well-formed daemon-revision ARN is refused with HTTP 400, error code InvalidParameterException and message 'The Daemon Revision ARN is invalid.' — the whole message is fixed text for this error class; no part of it is interpolated from the request. Smithy (ECS 2014-11-13, DescribeDaemonRevisions): input member daemonRevisionArns -> StringList is @required, errors include InvalidParameterException (client, HTTP 400); the exact message text is recorded behavior only.
Emulator: HTTP 200 with no __type and no message — the bad ARN is silently accepted.
Why: same client consequence as cause 1, on the revision-addressing path: an ARN-construction bug in the caller is invisible, because the emulator answers success for input AWS rejects, so the caller never learns its ARN is malformed and the failure surfaces much later as 'revision not found'.
Fix: DescribeDaemonRevisions' validation stage — parse/validate each daemonRevisionArns member (ARN syntax and target type) ahead of the lookup and raise InvalidParameterException with AWS's fixed text.

Related open issue: "ECS express-gateway and daemon operations skip validation and misclassify not-found errors" (20261029T220901Z-ecs-express-gateway-and-daemon-operations-skip-validation.md) describes the same class of gap on CreateDaemon / ListDaemons / DescribeDaemonTaskDefinition, but it does not cover DescribeDaemonDeployments or DescribeDaemonRevisions and carries neither of these fixture steps' replay keys, so these two causes are filed here.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout:
- ecs / DescribeDaemonDeployments, fixture ecs-daemon-error-paths-and-empty-listings step task#6 (verdict mismatch, divergence_key service=ecs|DescribeDaemonDeployments|status=400->200|keys=$.__type,$.message,status): status expected 400, got 200; $.__type expected 'InvalidParameterException', got absent; $.message expected 'Request parameter(s) are invalid.', got absent.
- ecs / DescribeDaemonRevisions, fixture ecs-daemon-error-paths-and-empty-listings step task#7 (verdict mismatch, divergence_key service=ecs|DescribeDaemonRevisions|status=400->200|keys=$.__type,$.message,status): status expected 400, got 200; $.__type expected 'InvalidParameterException', got absent; $.message expected 'The Daemon Revision ARN is invalid.', got absent.
Model support: smithy_query(operation=DescribeDaemonDeployments,DescribeDaemonRevisions) on services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json — each has a @required ARN-list input member and models InvalidParameterException with httpError 400.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Read the ecs divergence clusters for fixture ecs-daemon-error-paths-and-empty-listings.
3. Call DescribeDaemonDeployments with a daemonDeploymentArns entry that is not a usable daemon-deployment ARN -> emulator answers 200 with no failures instead of 400 InvalidParameterException 'Request parameter(s) are invalid.'
4. Call DescribeDaemonRevisions with a malformed daemon-revision ARN -> emulator answers 200 instead of 400 InvalidParameterException 'The Daemon Revision ARN is invalid.'
5. Re-replay fixture ecs-daemon-error-paths-and-empty-listings steps task#6 and task#7 to confirm both refusals.
