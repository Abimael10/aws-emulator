Issue-Source: trace-sweep-divergence
Issue-Trace: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|TagResource|2xx->2xx|$headers.content-length
Issue-Key: eks|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# eks tagging 204 responses omit the Content-Length framing header (2 causes)

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
The EKS tag plane answers its empty-bodied success responses without the HTTP framing header AWS sends with them. Two causes, one per tagging operation.

Cause 1 of 2: TagResource (fixture eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero, step task#12).
AWS: Smithy: EKS TagResource, http {method: POST, uri: /tags/{resourceArn}}, output shape TagResourceResponse has NO members, so the success reply carries no body. AWS serves that bodyless success with an explicit framing header, 'content-length: 0'; the recording asserts the header is present (expected '<shaped-not-asserted>' i.e. header present, value not pinned). Status recorded and served: 204.
Emulator: replies 204 with the header absent entirely ($headers.content-length: missing, got None). Status and body match; only the framing header is dropped.
Why: a client or proxy that reads response framing from Content-Length on a bodyless reply has no declared length and must fall back to connection-close/chunked inference. Strict HTTP clients, SDK middleware that logs or validates content length, and intermediaries that re-frame the response see a different message than AWS sends, and a client that blocks waiting for a body boundary can stall on an otherwise successful tag mutation.
Fix: the response serializer for the EKS tag plane — the component that writes an empty-output operation's HTTP response must emit 'content-length: 0' for a bodyless success rather than omitting the header.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#12; eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#14.

Cause 2 of 2: UntagResource (fixture eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero, step task#14).
AWS: Smithy: EKS UntagResource, http {method: DELETE, uri: /tags/{resourceArn}}, output shape UntagResourceResponse has NO members. Same rule: the bodyless 204 success carries 'content-length: 0'.
Emulator: replies 204 with no content-length header at all ($headers.content-length: missing, got None).
Why: same as cause 1 — the untag success is not framed as AWS frames it.
Fix: the same response serializer path for empty-output EKS operations; fixing one generically should close both, but both must be re-replayed.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#14 and #task#16; eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#16 and #task#18.

No open eks issue covers this: the open 204/Content-Length issues on file belong to other services (e.g. apigateway, mediapackage, schemas, mq, braket), not to eks.

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 against the emulator built from this checkout:

TagResource — expected status 204, actual status 204; divergence '$headers.content-length: missing (expected <shaped-not-asserted>, got None)'. Cluster key service=eks|TagResource|status=204->204|keys=$headers.content-length, 2 affected steps.

UntagResource — expected status 204, actual status 204; divergence '$headers.content-length: missing (expected <shaped-not-asserted>, got None)'. Cluster key service=eks|UntagResource|status=204->204|keys=$headers.content-length, 4 affected steps.

Expected (AWS): HTTP/1.1 204 No Content with 'content-length: 0' and no body.
Actual (emulator): HTTP/1.1 204 No Content with no content-length header and no body.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus.
2. TagResource: POST /tags/<cluster-or-nodegroup-arn> with a tags map on an existing EKS resource (fixture eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero, step task#12). Inspect the raw response headers: 204 arrives with no content-length.
3. UntagResource: DELETE /tags/<arn>?tagKeys=<key> on the same resource (same fixture, step task#14). Inspect the raw response headers: 204 arrives with no content-length.
4. Compare against AWS, which answers both with 'content-length: 0'.
