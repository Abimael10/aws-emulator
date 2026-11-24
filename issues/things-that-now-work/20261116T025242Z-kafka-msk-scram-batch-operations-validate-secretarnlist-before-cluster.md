Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-batch-associate-malformed-secret-arn
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-batch-associate-malformed-secret-arn
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|BatchAssociateScramSecret|4xx->4xx|$.invalidParameter
Issue-Key: kafka|BatchAssociateScramSecret|4xx->4xx|$.message
Issue-Key: kafka|BatchDisassociateScramSecret|4xx->4xx|$.invalidParameter
Issue-Key: kafka|BatchDisassociateScramSecret|4xx->4xx|$.message
Issue-Related: 20261029T202809Z-kafka-msk-not-found-already-exists-and-tagging-refusals.md
Issue-Commit: 7c1794e3c2e68241d47645a3c2b0490f00d69a5a

# MSK SCRAM batch operations validate SecretArnList before cluster existence, answering BadRequestException instead of NotFoundException

> _Note: this issue was identified against the code base at commit `7c1794e3c2e68241d47645a3c2b0490f00d69a5a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchAssociateScramSecret

## Description
Both SCRAM batch operations take the cluster to act on as an HTTP label (`/v1/clusters/{ClusterArn}/scram-secrets`) and the secret ARNs as a body member. The emulator runs the body-member format check first, so when the request names a cluster that does not exist AND carries a malformed secret ARN, it answers the secret-ARN refusal and the caller never learns the cluster is absent. MSK resolves the path resource first: a request whose ClusterArn does not resolve is refused as NotFoundException regardless of what the body contains. This is a validation-ORDER defect (which refusal wins), not merely a wording defect, so the HTTP status and error class are both wrong.

This bundle has 2 causes, both of which must be closed.

Cause 1 of 2: BatchAssociateScramSecret (fixture kafka-batch-associate-malformed-secret-arn, step task#1).
AWS: Smithy (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json) models BatchAssociateScramSecret as POST /v1/clusters/{ClusterArn}/scram-secrets with ClusterArn @required @httpLabel and SecretArnList @required (jsonName secretArnList), and binds both NotFoundException (httpError 404) and BadRequestException (httpError 400). NotFoundException carries members invalidParameter and message ("The parameter that caused the error" / "The description of the error"). The recording shows MSK answering HTTP 404, x-amzn-errortype `NotFoundException`, `invalidParameter` = `clusterArn`, `message` = `The requested resource doesn’t exist.` for a request against a nonexistent cluster whose SecretArnList is also malformed. Message anatomy: `The requested resource doesn’t exist.` is entirely FIXED text (note the U+2019 right single quote, not an ASCII apostrophe) and has no variable parts; the variable part of this refusal is `invalidParameter`, which names the modeled wire member of the parameter at fault — here the path member, lower-camel `clusterArn`.
Emulator: answers HTTP 400, x-amzn-errortype `BadRequestException`, `invalidParameter` = `SecretArnList`, `message` = `The provided secret ARN is invalid.` — i.e. it validated the body's secret ARNs and never checked whether the path cluster exists, and it names the parameter by its Smithy member name (`SecretArnList`) rather than the modeled wire name (`secretArnList`).
Why: a client probing whether a cluster exists by attaching secrets, or retrying on a 404, sees a 400 BadRequestException instead. SDK error-type dispatch selects the wrong handler (NotFoundException branches, which are usually non-retryable-but-expected "create the cluster first" paths, never run), and the operator-facing diagnostic blames the secret list for a missing cluster.
Fix: BatchAssociateScramSecret's handler/validation in the kafka service crate — resolve and existence-check the ClusterArn httpLabel BEFORE validating SecretArnList members, and emit NotFoundException(404) with invalidParameter `clusterArn` and the fixed message above when it does not resolve.

Cause 2 of 2: BatchDisassociateScramSecret (fixture kafka-batch-associate-malformed-secret-arn, step task#2).
AWS: Smithy models BatchDisassociateScramSecret as PATCH /v1/clusters/{ClusterArn}/scram-secrets with the same required ClusterArn @httpLabel + SecretArnList body member and the same error set including NotFoundException (httpError 404). The recording shows HTTP 404, x-amzn-errortype `NotFoundException`, `invalidParameter` = `clusterArn`, `message` = `The requested resource doesn’t exist.` — same fixed text, same single variable (the offending parameter's wire name).
Emulator: answers HTTP 400, x-amzn-errortype `BadRequestException`, `invalidParameter` = `SecretArnList`, `message` = `The provided secret ARN is invalid.`
Why: identical client impact on the detach path — cleanup code that treats 404 as "already gone, nothing to detach" instead sees a 400 validation failure and aborts.
Fix: same ordering correction in BatchDisassociateScramSecret's handler/validation; both operations share the cluster-ARN resolution step, so the existence check belongs ahead of body validation in that shared path.

Related open issues that do NOT cover these causes: 20261029T202809Z-kafka-msk-not-found-already-exists-and-tagging-refusals.md and 20261029T202809Z-kafka-msk-error-payloads-name-the-smithy-member-instead.md cover not-found class/wording and invalidParameter naming for other operations (DescribeCluster, DescribeReplicator, Tag/UntagResource, VPC-connection ops) and carry no replay key for the SCRAM batch pair; 20261104T171616Z-kafka-scram-batch-operations-report-no-per-secret-failures.md is about empty UnprocessedScramSecrets on a SUCCESSFUL call; 20261108T094801Z-kafka-batchassociatescramsecret-reports-the-default-kms-key-rejection-under.md is about the per-secret KMS-key ErrorCode. None of them is the validation-order defect reported here.

## Evidence
Fixture kafka-batch-associate-malformed-secret-arn (one request per step: a nonexistent cluster ARN in the path plus a malformed entry in secretArnList).

Step task#1 — BatchAssociateScramSecret:
  expected: HTTP 404; x-amzn-errortype: NotFoundException; $.invalidParameter = "clusterArn"; $.message = "The requested resource doesn’t exist."
  actual:   HTTP 400; x-amzn-errortype: BadRequestException; $.invalidParameter = "SecretArnList"; $.message = "The provided secret ARN is invalid."
  replay detail: status: expected 404, got 400; $.invalidParameter: value_mismatch (expected 'clusterArn', got 'SecretArnList'); $.message: value_mismatch (expected 'The requested resource doesn’t exist.', got 'The provided secret ARN is invalid.'); $headers.x-amzn-errortype: value_mismatch (expected 'NotFoundException', got 'BadRequestException')

Step task#2 — BatchDisassociateScramSecret:
  expected: HTTP 404; x-amzn-errortype: NotFoundException; $.invalidParameter = "clusterArn"; $.message = "The requested resource doesn’t exist."
  actual:   HTTP 400; x-amzn-errortype: BadRequestException; $.invalidParameter = "SecretArnList"; $.message = "The provided secret ARN is invalid."
  replay detail: status: expected 404, got 400; $.invalidParameter: value_mismatch (expected 'clusterArn', got 'SecretArnList'); $.message: value_mismatch (expected 'The requested resource doesn’t exist.', got 'The provided secret ARN is invalid.'); $headers.x-amzn-errortype: value_mismatch (expected 'NotFoundException', got 'BadRequestException')

Model evidence (kafka Smithy): BatchAssociateScramSecret POST /v1/clusters/{ClusterArn}/scram-secrets, BatchDisassociateScramSecret PATCH /v1/clusters/{ClusterArn}/scram-secrets; input ClusterArn (httpLabel, required), SecretArnList (jsonName secretArnList, required); errors include NotFoundException (httpError 404) and BadRequestException (httpError 400); NotFoundException members invalidParameter + message.

## Steps to Reproduce
1. Against the emulator's kafka endpoint, POST /v1/clusters/<a syntactically valid but nonexistent MSK cluster ARN>/scram-secrets with body {"secretArnList": ["<a malformed secret ARN>"]}.
2. Observe HTTP 400 with x-amzn-errortype: BadRequestException, invalidParameter "SecretArnList", message "The provided secret ARN is invalid."; MSK answers HTTP 404 NotFoundException with invalidParameter "clusterArn" and message "The requested resource doesn’t exist." (cause 1).
3. Repeat the same URI and body with PATCH (BatchDisassociateScramSecret) and observe the same 400 BadRequestException instead of the 404 NotFoundException (cause 2).
4. Equivalently, replay fixture kafka-batch-associate-malformed-secret-arn and inspect steps task#1 and task#2.
