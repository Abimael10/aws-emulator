Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.update-connectivity-server-enforced
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.update-connectivity-server-enforced|run#4|AssertionError|assert err == _NOT_FOUND, f"expected {_NOT_FOUND!r}, got {err!r}"
Issue-Commit: 6b54570bca2b9daa393bc9703bb0a096216f77cd

# UpdateConnectivity on a well-formed cluster ARN this region holds no cluster for is refused 403 AccessDeniedException instead of the modeled 404 NotFoundException

> _Note: this issue was identified against the code base at commit `6b54570bca2b9daa393bc9703bb0a096216f77cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateConnectivity

## Description
ONE net-new root cause on the kafka (Amazon MSK) control plane. Every other failing check in this behavioral replay was reported as OWNED by an open issue and is skipped; this is the only failure with no owner.

Cause 1 of 1 — UpdateConnectivity against a syntactically valid MSK cluster ARN in the CALLER'S OWN account/region that names no registered cluster is answered as an authorization failure (HTTP 403, AccessDeniedException) rather than as the modeled not-found refusal (HTTP 404, NotFoundException). Failing check: kafka.update-connectivity-server-enforced.

AWS: Smithy com.amazonaws.kafka#UpdateConnectivity (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json) — PUT /v1/clusters/{ClusterArn}/connectivity, ClusterArn @required @httpLabel, CurrentVersion @required (jsonName currentVersion), ConnectivityInfo (jsonName connectivityInfo); success 200 -> UpdateConnectivityResponse{clusterArn, clusterOperationArn}. Its modeled errors are exactly BadRequestException 400, ForbiddenException 403, InternalServerErrorException 500, NotFoundException 404, ServiceUnavailableException 503, UnauthorizedException 401 — the model declares NO AccessDeniedException for this operation at all, so the code the emulator returns is not in the operation's error set. Which refusal an absent cluster draws is recorded behavior only; the recording this check replays shows MSK answering NotFoundException with message "The requested resource doesn\u2019t exist." (note the U+2019 right single quotation mark, not an ASCII apostrophe). In that message every character is FIXED text: it carries no name, id, ARN or value from the request or from state, so the requirement is that exact string plus error code NotFoundException at HTTP 404. NotFoundException also models an invalidParameter member, which on this surface names the offending member in the service's wire vocabulary (clusterArn). The same check then asserts that DescribeCluster on that same ghost ARN reports the identical NotFoundException/"The requested resource doesn\u2019t exist." pair — that second assertion was never reached because the first one failed, so the whole not-found class for a ghost cluster ARN must be re-verified once this is fixed.

Emulator: HTTP 403 with error code AccessDeniedException and message of the form "User: arn:aws:iam::<account>:user/<user> is not authorized to perform: kafka:UpdateConnectivity on resource: <the submitted cluster ARN> because no resource-based policy allows the kafka:UpdateConnectivity action". The recorded instance names account 000000000000, user 'emulator' and the probe's own generated cluster ARN (arn:aws:kafka:us-west-2:000000000000:cluster/sim2real-behav-nonexistent-<suffix>/00000000-0000-0000-0000-000000000000-1) — those are per-run values, NOT part of the divergence; the divergence is the status class (403 vs 404), the error code (AccessDeniedException vs NotFoundException), the message text class, and the absent invalidParameter member. This is deliberate in the handler: services/service-com-amazonaws-kafka-kafka/src/ops/update_connectivity.rs calls crate::state::authorization::authorize_caller_operation (lines 191-196) and then crate::state::authorization::authorize_registered_cluster (lines 230-236) over the ARN AS SUBMITTED, before any registry resolution, and its module doc (lines 27-37) states the choice to conflate "you may not" with "it is not there". The miss branch at lines 271-278 likewise returns crate::state::authorization::no_resource_policy_for_cluster instead of a NotFoundException, so no code path on this operation can ever emit the modeled 404 for an unknown cluster.

Why: the status class and error code are what a client branches on. A caller enabling public access / multi-VPC connectivity on a cluster it believes it created — a reconciler, a Terraform-style provider, or a retry loop around an update — receives a credentials/permissions signal (403 AccessDenied), which SDK and IaC error handling escalate as a mis-permissioned principal needing policy remediation, where AWS returns a plain 404 NotFoundException that the same code handles as "this cluster is gone, recreate or stop polling". Deletion-reconciliation and idempotent-create flows therefore hard-fail as an IAM problem instead of converging, and code that reads invalidParameter to learn which member was at fault gets nothing. Because AccessDeniedException is not even modeled for this operation, strictly typed SDKs surface it as an unmodeled/unknown error rather than a handled one.

Fix: the refusal ordering and refusal class in the operation's handler — services/service-com-amazonaws-kafka-kafka/src/ops/update_connectivity.rs. Resolve cluster existence in the request's region before taking the resource-scoped authorization decision (or scope that decision so a well-formed cluster ARN naming no cluster of this region/account falls through), and answer that case with UpdateConnectivityErrorModeled::NotFoundException carrying invalidParameter 'clusterArn' and the service's single fixed not-found message. The shared helpers crate::state::authorization::authorize_registered_cluster and crate::state::authorization::no_resource_policy_for_cluster are the seam; DescribeCluster's ghost-ARN answer must land on the same class, since the check asserts both.

Related open issues that do NOT cover this check: 20261118T222834Z-kafka-msk-control-plane-answers-diverge-published-version-matrix.md reports the SAME authorization-before-existence inversion on DescribeClusterOperation (a different operation and handler, describe_cluster_operation.rs); 20261029T175822Z-kafka-client-vpc-connection-surface-on-a-nonexistent-cluster.md covers the client-VPC-connection reads on a nonexistent cluster; 20261116T025242Z-kafka-msk-scram-batch-operations-validate-secretarnlist-before-cluster.md covers the SCRAM batch ordering. None names UpdateConnectivity or this check, and the replay reported this failure with no owner.

Traces: behavioral check kafka.update-connectivity-server-enforced (assertion at check_kafka_update_connectivity_server_enforced.py line 57: expected ('NotFoundException', 'The requested resource doesn\u2019t exist.'), got ('AccessDeniedException', 'User: ... is not authorized to perform: kafka:UpdateConnectivity ...')).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout (branch issues/finder-wi-061269691a2a4c2ba00771c09c206475-1795519895177): 182/238 passed (76.5%), 43 mismatched, 13 errored, 0 skipped, 0 unloadable. kafka 178/228 (78%), 50 failed. 55 of the 56 failures are marked OWNED by an open issue; the one below is the only failure with no owner.

[mismatch] kafka.update-connectivity-server-enforced
  File "check_kafka_update_connectivity_server_enforced.py", line 57, in run
    assert err == _NOT_FOUND, f"expected {_NOT_FOUND!r}, got {err!r}"
  AssertionError: expected ('NotFoundException', 'The requested resource doesn\u2019t exist.'), got ('AccessDeniedException', 'User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: kafka:UpdateConnectivity on resource: arn:aws:kafka:us-west-2:000000000000:cluster/sim2real-behav-nonexistent-5e44f5999f8e/00000000-0000-0000-0000-000000000000-1 because no resource-based policy allows the kafka:UpdateConnectivity action')

The check builds the ghost ARN from the caller's OWN account id (sts:GetCallerIdentity) and the endpoint's region, so cross-account ownership is not what triggers the refusal — the ARN is simply well-formed and unregistered:
    def _bogus_cluster_arn(ctx):
        return (f"arn:aws:kafka:{ctx.region}:{_account_id(ctx)}:cluster/"
                f"{ctx.unique_name('sim2real-behav-nonexistent-')}/"
                "00000000-0000-0000-0000-000000000000-1")
and then calls:
    msk.update_connectivity(ClusterArn=ghost, CurrentVersion="1",
                            ConnectivityInfo={"PublicAccess": {"Type": "SERVICE_PROVIDED_EIPS"}})

Facets that diverge:
  HTTP status:   expected 404, got 403
  Error.Code:    expected 'NotFoundException', got 'AccessDeniedException' (a code the operation's Smithy error set does not contain)
  Error.Message: expected 'The requested resource doesn\u2019t exist.' — entirely FIXED text, no request- or state-derived part — got the "User: ... is not authorized to perform: kafka:UpdateConnectivity on resource: ... because no resource-based policy allows the kafka:UpdateConnectivity action" template
  invalidParameter: expected 'clusterArn', got no member at all
The account id (000000000000), user name ('emulator'), region and generated cluster name/UUID in the served message are per-run values and are NOT the finding.

Source corroboration that no path can emit the modeled 404 here: services/service-com-amazonaws-kafka-kafka/src/ops/update_connectivity.rs lines 191-196 (authorize_caller_operation on the submitted ARN, before resolution), lines 230-236 (authorize_registered_cluster, documented at lines 27-37 as intentionally answering an unregistered cluster as unauthorized), and lines 271-278 (the registry-miss branch returns no_resource_policy_for_cluster rather than NotFoundException).

The second assertion of the same check — DescribeCluster(ghost) must also answer ('NotFoundException', 'The requested resource doesn\u2019t exist.') — was never reached, so its verdict is unknown and must be confirmed by re-replay.

## Steps to Reproduce
Boot the emulator built from this checkout and replay behavioral suite simparity-behavioral-aws-v1 (check kafka.update-connectivity-server-enforced), or reproduce directly:

1. Point an AWS SDK/CLI client at the emulator's kafka endpoint and read the caller's account id: `aws sts get-caller-identity`.
2. Build a well-formed MSK cluster ARN in THAT account and the endpoint's region that no cluster exists for, e.g.
   arn:aws:kafka:<region>:<that-account>:cluster/<a-name-never-created>/00000000-0000-0000-0000-000000000000-1
3. Call UpdateConnectivity on it:
   aws kafka update-connectivity --cluster-arn <that ARN> --current-version 1 \
       --connectivity-info '{"PublicAccess":{"Type":"SERVICE_PROVIDED_EIPS"}}'
   (PUT /v1/clusters/{ClusterArn}/connectivity)
4. Expect HTTP 404, Error.Code 'NotFoundException' (x-amzn-errortype: NotFoundException), body message exactly "The requested resource doesn\u2019t exist." (U+2019 apostrophe, all fixed text) and invalidParameter 'clusterArn'.
5. Observe instead HTTP 403, Error.Code 'AccessDeniedException', no invalidParameter member, and the message "User: arn:aws:iam::<account>:user/<user> is not authorized to perform: kafka:UpdateConnectivity on resource: <the submitted ARN> because no resource-based policy allows the kafka:UpdateConnectivity action" — i.e. authorize_registered_cluster in update_connectivity.rs fires before any registry lookup.
6. Then call `aws kafka describe-cluster --cluster-arn <that same ARN>` and confirm it answers the same 404/NotFoundException/"The requested resource doesn\u2019t exist." pair; the check asserts this immediately after step 4 and never reached it.
7. Re-replay kafka.update-connectivity-server-enforced to confirm the fix.
