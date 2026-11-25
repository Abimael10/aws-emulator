Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.get-on-cluster-app-ui-presigned-url-inactive-cluster-rejected
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr|GetOnClusterAppUIPresignedURL|InvalidRequestException
Issue-Commit: 47208604cde63241f79c213c4b593a23b3ff78ba

# emr: GetOnClusterAppUIPresignedURL refuses a STARTING cluster with InvalidRequestException instead of minting a URL — the call is gated on "not terminated", not on "running"

> _Note: this issue was identified against the code base at commit `47208604cde63241f79c213c4b593a23b3ff78ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOnClusterAppUIPresignedURL

## Description
REOPENS 20261105T111420Z-emr-three-cluster-scoped-refusal-paths-are-unreachable.md: its fix left the call refused.

That issue stood GetOnClusterAppUIPresignedURL up out of its 501 scaffold and gave it an unknown-ClusterId refusal, but it settled the state gate the wrong way round: the handler now refuses every cluster that is not RUNNING/WAITING, so the operation is refused for an active cluster and the terminated-cluster refusal it is supposed to make is unreachable.

Cause 1 of 1: the state gate on GetOnClusterAppUIPresignedURL is inverted.

AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce) GetOnClusterAppUIPresignedURL takes ClusterId (@required, XmlStringMaxLen256) plus OnClusterAppUIType (enum ApplicationMaster | JobHistoryServer | ResourceManager | SparkHistoryServer | TezUI | YarnTimelineService), ApplicationId, DryRun and ExecutionRoleArn, and returns PresignedURLReady (Boolean) + PresignedURL (XmlString, len 0..10280). It models InvalidRequestException (HTTP 400) and InternalServerError (HTTP 500). Recorded behavior for the state gate: the operation is gated on the cluster NOT being terminated, not on it being up. A cluster still in ClusterState STARTING (ClusterState enum: STARTING | BOOTSTRAPPING | RUNNING | WAITING | TERMINATING | TERMINATED | TERMINATED_WITH_ERRORS) is answered HTTP 200 with PresignedURLReady=true and a PresignedURL on the https scheme; only after the cluster has reached TERMINATED/TERMINATED_WITH_ERRORS is the call refused, with error code `ValidationException` and no URL in the response.
Emulator:  the same call against a cluster in STARTING is refused HTTP 400 with error code `InvalidRequestException` and the message `Cluster '<ClusterId>' is in state <ClusterState>; an application user interface is only served by a cluster in the RUNNING or WAITING state.` — the recorded replay shows `Cluster 'j-0000000000001' is in state STARTING; ...`, where the quoted id is the request's ClusterId and the quoted state is the cluster's current ClusterState, and the rest of the sentence is emulator-authored fixed text that AWS does not emit for this operation at all. Because the active-cluster arm is refused, the check never reaches the terminated-cluster arm, so the required `ValidationException` refusal for a terminated cluster is also unverified/absent.
Why:       a console or client that probes app-UI availability while a cluster is coming up gets a 400 refusal where AWS hands back a usable (or at least ready-flagged) URL, so the normal "open the Spark History Server as soon as the cluster exists" flow fails; and because the only refusal the emulator makes is `InvalidRequestException` for a not-yet-running cluster, a client cannot distinguish a cluster that is gone (fail fast, `ValidationException`) from one that is still starting (retry).
Fix:       the GetOnClusterAppUIPresignedURL handler's state gate — services/service-com-amazonaws-emr-elasticmapreduce/src/ops/get_on_cluster_app_ui_presigned_url.rs uses `crate::state::serves_app_ui_in_state(&cluster.status.state)` to refuse anything outside RUNNING/WAITING via `crate::state::cluster_not_running_for_app_ui`. The predicate belongs in state.rs and must admit every non-terminated ClusterState (STARTING, BOOTSTRAPPING, RUNNING, WAITING, TERMINATING) and refuse only TERMINATED / TERMINATED_WITH_ERRORS, through the ValidationException channel the handler already has (`crate::scaffolded::Scaffolded::validation`).
Traces:    emr.get-on-cluster-app-ui-presigned-url-inactive-cluster-rejected

## Evidence
Behavioral replay against the booted emulator, service heading `### emr`.

Check emr.get-on-cluster-app-ui-presigned-url-inactive-cluster-rejected [error]:
  expected: RunJobFlow a cluster, then immediately GetOnClusterAppUIPresignedURL(ClusterId=<id>, OnClusterAppUIType="ResourceManager") -> HTTP 200, PresignedURLReady=true, PresignedURL starting with "https://"; then TerminateJobFlows + wait for TERMINATED, and the same call -> error code ValidationException with no URL.
  actual:   botocore.errorfactory.InvalidRequestException: An error occurred (InvalidRequestException) when calling the GetOnClusterAppUIPresignedURL operation: Cluster 'j-0000000000001' is in state STARTING; an application user interface is only served by a cluster in the RUNNING or WAITING state.
  The check aborts on that first call, so the terminated-cluster ValidationException arm is never exercised.

Source of the inverted gate: services/service-com-amazonaws-emr-elasticmapreduce/src/ops/get_on_cluster_app_ui_presigned_url.rs lines 217-232 (`if !crate::state::serves_app_ui_in_state(&cluster.status.state) { return Err(... cluster_not_running_for_app_ui ...) }`).

## Steps to Reproduce
1. Boot the emulator and point an EMR client at it.
2. Provision the EMR service role and EC2 instance profile, then RunJobFlow a keep-alive cluster with release emr-6.15.0.
3. WITHOUT waiting for the cluster to leave STARTING, call GetOnClusterAppUIPresignedURL(ClusterId=<id>, OnClusterAppUIType="ResourceManager"). Observe HTTP 400 InvalidRequestException "Cluster '<id>' is in state STARTING; an application user interface is only served by a cluster in the RUNNING or WAITING state." instead of HTTP 200 with PresignedURLReady=true and an https PresignedURL.
4. TerminateJobFlows(JobFlowIds=[<id>]) and wait until DescribeCluster reports TERMINATED. Call GetOnClusterAppUIPresignedURL again and confirm the refusal is error code ValidationException (not InvalidRequestException) and carries no PresignedURL.
5. Re-replay emr.get-on-cluster-app-ui-presigned-url-inactive-cluster-rejected to confirm the fix.
