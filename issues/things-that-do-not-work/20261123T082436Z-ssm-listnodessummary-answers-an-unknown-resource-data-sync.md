Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-list-nodes-summary-aggregators-and-errors
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-list-nodes-summary-aggregators-and-errors ssm-resource-data-sync-not-found-errors
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|ListNodesSummary|4xx->4xx|$.SyncName
Issue-Key: ssm|ListNodesSummary|4xx->4xx|$.__type
Issue-Commit: 797ef3390a04ded8e23f4c61e40679f301e400ed

# SSM ListNodesSummary answers an unknown resource data sync with UnsupportedOperationException instead of ResourceDataSyncNotFoundException

> _Note: this issue was identified against the code base at commit `797ef3390a04ded8e23f4c61e40679f301e400ed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNodesSummary

## Description
One cause.

Cause 1 of 1: ListNodesSummary called with a `SyncName` that names no registered resource data sync is refused with the wrong modeled error shape, and the shape it does serve omits the member that tells the client which sync was missing.

AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json) binds ListNodesSummary with input member `SyncName` (target ResourceDataSyncName, @length min 1 max 64) and models, among its errors, BOTH `ResourceDataSyncNotFoundException` (client error, httpError 400 defaulted, awsQueryError code ResourceDataSyncNotFound) and `UnsupportedOperationException` (client error, httpError 400 defaulted, awsQueryError code UnsupportedOperation). These are two distinct conditions and the model documents each: ResourceDataSyncNotFoundException is "The specified sync name wasn't found.", while UnsupportedOperationException is "This operation is not supported for the current account. You must first enable the Systems Manager integrated experience in your account." ResourceDataSyncNotFoundException additionally carries the members `SyncName` (ResourceDataSyncName, @length 1..64), `SyncType` (ResourceDataSyncType, @length 1..64) and `Message`; UnsupportedOperationException carries only `Message`. So for a request that names a sync which does not exist in an account that CAN serve the operation, the requirement is: HTTP 400 with `__type` = `ResourceDataSyncNotFoundException` and the exception's `SyncName` member echoing the sync name taken from the request — the recorded responses confirm exactly this shape. The fixed text of the error class is the "sync name wasn't found" sentence; the only variable part is the sync name itself, which comes from the request (the recordings' `sim2real-probe-ssm-nosync-2e10b68b` / `sim2real-probe-ssm-probe-missing-sync-08591348` are per-run probe names that fill the template, not part of it).

Emulator: the status is right (400) but the error identity is wrong. Replay reports `$.__type: value_mismatch (expected 'ResourceDataSyncNotFoundException', got 'UnsupportedOperationException')` and `$.SyncName: missing (expected <the sync name from the request>, got None)`. The emulator appears to have no resource-data-sync registry lookup on this path at all: rather than resolving `SyncName` and failing it as not-found, it short-circuits every call to the account-level "integrated experience not enabled" refusal, which by its own model carries no `SyncName`/`SyncType` member to populate.

Why: a client cannot distinguish "this sync does not exist" from "this account cannot use node management at all". The two errors drive opposite remediation — the first is retried after CreateResourceDataSync for the named sync, the second is a one-time account enablement — so SDK/CLI error handling and any catch on ResourceDataSyncNotFoundException never fires, and tooling that reads the exception's `SyncName` to report which sync is missing gets nothing. Because UnsupportedOperationException reads as an account-wide capability failure, callers will abandon the whole node-management flow instead of fixing one sync name.

Fix: the ListNodesSummary handler in the ssm crate — resolve the request's `SyncName` against the resource-data-sync state before (or instead of) the account-capability check, and when no sync of that name is registered, raise ResourceDataSyncNotFoundException with its `SyncName` member set from the request and its `Message` worded as AWS words that error class. Reserve UnsupportedOperationException for the account-not-enabled condition it is modeled for. The same defect on the sibling ListNodes operation is already filed as 20261116T061000Z-ssm-ops-center-maintenance-window-and-association-reads.md (its ListNodes cluster is owned by that issue) and the fix most likely belongs in one shared sync-resolution helper used by both operations; the open issue 20261123T021656Z-ssm-listnodessummary-validationexception-message-lists-only-8-of-aws.md touches ListNodesSummary too but concerns the NodeAttributeName enum wording and does not cover this error-shape substitution.

Traces: ssm-list-nodes-summary-aggregators-and-errors#task#2 (representative) and ssm-resource-data-sync-not-found-errors#task#3 in the same cluster.

## Evidence
Fixture ssm-list-nodes-summary-aggregators-and-errors, step task#2 (cluster also covers ssm-resource-data-sync-not-found-errors#task#3), operation ListNodesSummary, verdict mismatch.

Expected status 400, actual status 400.

Divergent keys:
  $.__type: value_mismatch — expected 'ResourceDataSyncNotFoundException', got 'UnsupportedOperationException'
  $.SyncName: missing — expected the sync name supplied in the request (recorded as 'sim2real-probe-ssm-nosync-2e10b68b'), got None

So the emulator returns the modeled error shape UnsupportedOperationException (members: Message only) where AWS returns ResourceDataSyncNotFoundException (members: SyncName, SyncType, Message) with SyncName echoed from the request. Smithy: ListNodesSummary errors include both ResourceDataSyncNotFoundException (awsQueryError ResourceDataSyncNotFound) and UnsupportedOperationException (awsQueryError UnsupportedOperation); ResourceDataSyncNotFoundException.SyncName targets ResourceDataSyncName @length 1..64.

The identical substitution is observed on ListNodes (fixture ssm-resource-data-sync-not-found-errors#task#2), which is already owned by an open issue — evidence that the sync-not-found path is missing across the node-management reads rather than being specific to one operation.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Register no resource data sync, then call ssm ListNodesSummary with a required `Aggregators` list and `SyncName` set to a name that is not registered (1..64 chars), e.g. the probe name used by fixture ssm-list-nodes-summary-aggregators-and-errors.
3. Observe HTTP 400 whose body has "__type": "UnsupportedOperationException" and no "SyncName" member.
4. AWS answers HTTP 400 with "__type": "ResourceDataSyncNotFoundException" and "SyncName" echoing the requested sync name.
5. Re-replay ssm-list-nodes-summary-aggregators-and-errors#task#2 and ssm-resource-data-sync-not-found-errors#task#3 to confirm the fix.
