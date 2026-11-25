Issue-Source: trace-sweep-divergence
Issue-Trace: dax-error-paths
Issue-Step: task#15
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dax-error-paths
Issue-Shape-Id: com.amazonaws.dax#AmazonDAXV3
Issue-Key: dax|ListTags|4xx->4xx|$.message
Issue-Key: dax|UntagResource|4xx->4xx|$.message
Issue-Commit: e0ec1c1edefac5aae54b076e6361615d0195bedd

# dax: ClusterNotFoundFault message is a fixed "Cluster not found." instead of AWS's named-cluster wording

> _Note: this issue was identified against the code base at commit `e0ec1c1edefac5aae54b076e6361615d0195bedd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTags

## Description
The emulator's DAX cluster-lookup failure path emits a single hardcoded error message string for ClusterNotFoundFault rather than AWS's templated message, which names the cluster that could not be resolved. Every DAX operation that resolves a ResourceName/ClusterName and misses is affected; the replay corpus exercises two of them, and both diverge identically.

BUNDLE: 2 causes.

Cause 1 of 2: ListTags on a non-existent cluster (fixture dax-error-paths, step task#15).
  AWS: Smithy (com-amazonaws-dax-amazondaxv3): ListTags input ResourceName (String, required); modeled error ClusterNotFoundFault, errorKind client, documented as "The requested cluster ID does not refer to an existing DAX cluster." Recorded response: HTTP 400 with error shape ClusterNotFoundFault and message '<clusterName> is either not present or not available'. The fixed text of that message is the literal suffix " is either not present or not available"; the single variable part is the cluster identifier taken verbatim from the request's ResourceName/cluster-name argument. Status (400) and error code (ClusterNotFoundFault) already match.
  Emulator: returns HTTP 400 with the correct error code but the message 'Cluster not found.' — a constant with no variable part, so the requested cluster identifier never appears.
  Why: a client that parses or logs the message to learn WHICH cluster was missing (common when a call resolves several cluster names, or when the message is surfaced to an operator) gets no identifier at all. Assertions and test suites written against AWS's wording fail, and error-message-driven retry/branching logic cannot distinguish one missing cluster from another.
  Fix: the dax ClusterNotFoundFault construction in the shared cluster-lookup/validation path used by the tagging operations — the message must be formatted from the request's cluster identifier, not a constant.

Cause 2 of 2: UntagResource on a non-existent cluster (fixture dax-error-paths, step task#16).
  AWS: Smithy (com-amazonaws-dax-amazondaxv3): UntagResource input ResourceName (String, required) and TagKeys (KeyList, required); modeled errors include ClusterNotFoundFault (errorKind client) and TagNotFoundFault. Recorded response: HTTP 400, ClusterNotFoundFault, message '<clusterName> is either not present or not available' — same template as Cause 1: fixed suffix " is either not present or not available", variable cluster identifier from the request. The cluster-not-found check correctly precedes any tag-key validation.
  Emulator: returns HTTP 400 with the correct error code but the same constant message 'Cluster not found.'.
  Why: identical client impact to Cause 1; additionally, because the message is byte-identical for ListTags and UntagResource and carries no identifier, a client cannot tell from the message which resource argument it got wrong.
  Fix: same component — the dax ClusterNotFoundFault message construction shared by the tagging operations' handlers/validation; fixing it once should close both causes, but both steps must be re-replayed to confirm.

No open dax issue covers this: the existing open dax issues concern a DeleteSubnetGroup stub, CloudFormation DAX resource stub provisioning, and replication-factor status-transition timing — none touch ClusterNotFoundFault message text.

## Evidence
Replay of trace set simparity-wire-aws-v1 (6 fixtures, 42 steps, 40 passed, 2 mismatched, score 95.2%).

Cause 1 — dax-error-paths#task#15, service dax, operation ListTags, verdict mismatch:
  expected_status 400, actual_status 400
  detail: $.message: value_mismatch (expected 'sim2real-probe-missing-cluster-23bbaef0 is either not present or not available', got 'Cluster not found.')
  divergence_key: service=dax|ListTags|status=400->400|keys=$.message

Cause 2 — dax-error-paths#task#16, service dax, operation UntagResource, verdict mismatch:
  expected_status 400, actual_status 400
  detail: $.message: value_mismatch (expected 'sim2real-probe-missing-cluster-23bbaef0 is either not present or not available', got 'Cluster not found.')
  divergence_key: service=dax|UntagResource|status=400->400|keys=$.message

In both recorded messages 'sim2real-probe-missing-cluster-23bbaef0' is the per-run cluster name supplied by that recording's request and is NOT part of the requirement; the requirement is the template '<requested cluster identifier> is either not present or not available'. The emulator emits no identifier whatsoever.

Status and error code are already correct in both cases (400 / ClusterNotFoundFault); only $.message diverges. Neither cluster carries a stub_path or owned_by, so neither is already filed.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus().
2. Cause 1: fixture dax-error-paths, step task#15 — call dax ListTags with ResourceName referring to a cluster that does not exist in emulator state. Observe HTTP 400 ClusterNotFoundFault with message 'Cluster not found.'; AWS returns '<that cluster identifier> is either not present or not available'.
3. Cause 2: fixture dax-error-paths, step task#16 — call dax UntagResource with ResourceName referring to a cluster that does not exist and any TagKeys list. Observe the same HTTP 400 ClusterNotFoundFault with the constant message 'Cluster not found.'; AWS returns '<that cluster identifier> is either not present or not available'.
4. Re-replay BOTH dax-error-paths#task#15 and dax-error-paths#task#16 to confirm a fix; a fix verified on only one step leaves the other cause unexercised.
