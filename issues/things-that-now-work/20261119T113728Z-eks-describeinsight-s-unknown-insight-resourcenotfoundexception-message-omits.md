Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.cluster-insights-read-path
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks.cluster-insights-read-path|run#9|AssertionError|assert unknown_id_msg.startswith(f"Cannot find insight insightId={unknown_id} clusterId=") and (
Issue-Commit: b448d9011382618e887f073932b66b1aa6876ff6

# eks: DescribeInsight's unknown-insight ResourceNotFoundException message omits AWS's wrapped EksClusterInsights service suffix

> _Note: this issue was identified against the code base at commit `b448d9011382618e887f073932b66b1aa6876ff6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInsight

## Description
Replaying simparity-behavioral-aws-v1 against the eks emulator (crate services/service-com-amazonaws-eks-awswesleyfrontend) leaves exactly ONE failing check that the suite marks neither ALREADY FILED nor OWNED: eks.cluster-insights-read-path. It has ONE root cause.

Cause 1 of 1 — DescribeInsight refuses an unknown insight id with only the bare sentence, dropping the wrapped service/status/request-id trailer AWS appends.

AWS: Smithy: DescribeInsight (GET /clusters/{clusterName}/insights/{id}; clusterName and id are required httpLabel members) models ResourceNotFoundException (httpError 404). Recorded behavior: cluster insights are served by a separate Amazon backend (EksClusterInsights) and EKS surfaces that backend's exception verbatim, so the ResourceNotFoundException message for an id that is not one of the cluster's insights is a bare sentence FOLLOWED BY AWS's wrapped suffix. The whole message has the shape:
  "Cannot find insight insightId=<id> clusterId=<clusterId> (Service: EksClusterInsights, Status Code: 404, Request ID: <uuid>) (SDK Attempt Count: 1)"
Fixed text (the requirement): "Cannot find insight insightId=", " clusterId=", " (Service: EksClusterInsights, Status Code: 404, Request ID: ", ") (SDK Attempt Count: 1)". Variable parts, taken from the request and from the emulator's state: <id> is copied verbatim from the request's id httpLabel; <clusterId> is the cluster's own internal identifier (the id the cluster publishes for itself); <uuid> is the backend request id, a per-run value that is NOT part of the requirement. Note the trailer is the same wrapped form the already-open issue 20261103T150548Z-eks-5-net-new-root-causes-in-refusal-0ae4aa.md requires for StartInsightsRefresh (its cause 3) — that issue is grounded in other check ids and does not carry this check's replay key, and it names StartInsightsRefresh's in-progress guard, not DescribeInsight. Hypothesis: the suffix is the AWS SDK's wrapped-service-exception rendering, which EKS passes through for every insights-plane refusal.

Emulator: the HTTP status (404) and the error code (ResourceNotFoundException) are correct — the check's assertion on the code passes — but the message stops at the bare sentence: "Cannot find insight insightId=<id> clusterId=<clusterId>". It contains no " (Service: EksClusterInsights, Status Code: 404, Request ID: " clause and does not end with ") (SDK Attempt Count: 1)".

Why: the wrapped suffix is how a client tells an insights-plane refusal (served by EksClusterInsights, with its own status code and request id for support correlation) from an EKS control-plane 404. Any client or test that matches AWS's full message — or that parses the embedded service name/status/request id out of it to log or correlate the failure — never matches, and the backend request id needed to escalate a real failure is simply absent. The divergence is invisible on the happy path and only appears on the refusal, so it silently breaks error-handling branches.

Fix: the DescribeInsight handler's missing-insight refusal in the eks crate's ops module — the message it hands to the not-found error for a child resource must carry the wrapped EksClusterInsights trailer (service name, the 404 status, a generated request id, and the SDK attempt count), consistently with the other insights-plane refusals of the same service.

Traces: eks.cluster-insights-read-path.

## Evidence
Suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 266/364 passed (73.1%); eks 249/318. eks.cluster-insights-read-path is the only failing check in the report marked neither ALREADY FILED nor OWNED.

[mismatch] eks.cluster-insights-read-path
  File "check_eks_cluster_insights_read_path.py", line 173, in run
    assert unknown_id_msg.startswith(f"Cannot find insight insightId={unknown_id} clusterId=") and (
  AssertionError: Cannot find insight insightId=0adfd9bb-e085-4d4a-9e62-5cd2a96a90c6 clusterId=95C95521EEA39E10CC1020344B5FDDBF

The asserted condition is a conjunction of three clauses; the emulator's message satisfies the first (the "Cannot find insight insightId=<id> clusterId=" prefix) and fails the other two:
  - " (Service: EksClusterInsights, Status Code: 404, Request ID: " must appear in the message — absent;
  - the message must end with ") (SDK Attempt Count: 1)" — it ends at the clusterId value.
The immediately preceding assertion, that the error code is "ResourceNotFoundException", PASSED, and list_insights(clusterName=<cluster>) returned a list, so setup and the success path are fine; only the refusal's message text diverges. The id and clusterId shown above are one run's values and are not part of the requirement.

Emulator source: services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/describe_insight.rs lines 205-211 build the refusal as state::child_resource_not_found(format!("Cannot find insight insightId={} clusterId={cluster_id}", input.id)) — the format string terminates after the clusterId, so no wrapped service/status/request-id suffix is ever appended.

Smithy model consulted: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json — DescribeInsight: GET /clusters/{clusterName}/insights/{id}, input {clusterName (required, httpLabel), id (required, httpLabel)}, output {insight: Insight}, errors InvalidParameterException 400 / InvalidRequestException 400 / ResourceNotFoundException 404 / ServerException 500. The model states the error class and status only; the message text is recorded behavior.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator, and point a boto3 eks client at it.
2. Create an EKS cluster (create_cluster with two subnets in two distinct EKS-supported AZs, accessConfig authenticationMode API_AND_CONFIG_MAP) and wait until describe_cluster reports status ACTIVE.
3. Confirm the read path works: list_insights(clusterName=<cluster>) must return a list in "insights".
4. Call describe_insight(clusterName=<cluster>, id=<a freshly generated uuid4 that is not in that listing>).
5. Expect: ClientError with HTTP status 404 and code ResourceNotFoundException, whose Message is "Cannot find insight insightId=<the uuid from step 4> clusterId=<the cluster's internal id> (Service: EksClusterInsights, Status Code: 404, Request ID: <some uuid>) (SDK Attempt Count: 1)".
6. Observe: the code and status are right, but the Message ends at "...clusterId=<the cluster's internal id>" — it neither contains " (Service: EksClusterInsights, Status Code: 404, Request ID: " nor ends with ") (SDK Attempt Count: 1)".
