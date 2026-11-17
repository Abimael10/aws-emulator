Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: supplychain.namespace-create-get-list-roundtrip
Issue-Shape-Id: com.amazonaws.supplychain#GalaxyPublicAPIGateway
Issue-Key: supplychain.namespace-create-get-list-roundtrip|run#23|AssertionError|assert code_update_after_delete == "ResourceNotFoundException", (
Issue-Commit: 88a557bb4c4d0767d5b593edab63aa6c5b4e0556

# supplychain UpdateDataLakeNamespace answers 501 NotImplemented instead of 404 ResourceNotFoundException for a deleted namespace

> _Note: this issue was identified against the code base at commit `88a557bb4c4d0767d5b593edab63aa6c5b4e0556`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateDataLakeNamespace

## Description
Cause 1 of 1: UpdateDataLakeNamespace is an unimplemented scaffold stub, so every call to it — including one that must be REFUSED because the target namespace no longer exists — is answered with the emulator's 501 NotImplemented scaffold error instead of the modeled outcome. Failing check: supplychain.namespace-create-get-list-roundtrip.

AWS: Smithy model (services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json): UpdateDataLakeNamespace, PATCH /api/datalake/instance/{instanceId}/namespaces/{name}, success 200 with required member `namespace` (DataLakeNamespace); input `instanceId` (UUID, @length 36..36, pattern ^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$, required, httpLabel), `name` (DataLakeNamespaceName, @length 1..50, pattern ^[a-z0-9_]+$, required, httpLabel), `description` (@length 1..500). Modeled errors include ResourceNotFoundException (client, HTTP 404, "Request references a resource which does not exist."). AWS therefore refuses a mutation aimed at a namespace that has been deleted with HTTP 404 / error code ResourceNotFoundException. Recorded behavior for the message: the fixed text is "Namespace " ... " not exist", with the namespace name taken from the request's `name` label (the same message the already-passing GetDataLakeNamespace-after-delete assertion in this check expects); the name in any single run is a generated value filling that template, not part of it.

Emulator: the handler at services/service-com-amazonaws-supplychain-galaxypublicapigateway/src/ops/update_data_lake_namespace.rs returns crate::scaffolded::Scaffolded::not_implemented("UpdateDataLakeNamespace") unconditionally (the file still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker). The replay shows error code `NotImplemented` (HTTP 501, message "operation UpdateDataLakeNamespace is scaffolded but not implemented in this emulator") where the check asserted `ResourceNotFoundException`: "AssertionError: expected ResourceNotFoundException updating the deleted namespace, got NotImplemented".

Why: a client cannot update a namespace description at all, and cannot distinguish a gone resource from a broken service. 501 is not retryable or mappable by the SDK's error handling, so code that branches on ResourceNotFoundException (recreate-on-missing, idempotent cleanup) takes its unexpected-error path, and code that merely updates a description fails outright even for a live namespace.

Fix: implement the operation's handler (update_data_lake_namespace.rs `handle`): look the namespace up in the service state by (instanceId, name); when absent return Err(UpdateDataLakeNamespaceErrorModeled::ResourceNotFoundException(..)) carrying the "Namespace <name> not exist" message so it serializes as HTTP 404 / ResourceNotFoundException; when present persist the new `description`, advance lastModifiedTime, and return the updated DataLakeNamespace in the required `namespace` output member. Remove the SCAFFOLDED marker line once implemented. Note: an open issue exists for the sibling stubs DeleteDataLakeNamespace (20261117T012817Z-supplychain-deletedatalakenamespace-stub.md) and other supplychain operations; none of them names UpdateDataLakeNamespace, so none covers this failure.

Traces: supplychain.namespace-create-get-list-roundtrip (assertion at check_supplychain_namespace_create_get_list_roundtrip.py line 85).

## Evidence
Check supplychain.namespace-create-get-list-roundtrip [mismatch].
Expected (AWS / modeled): HTTP 404, Error.Code = ResourceNotFoundException, Error.Message = "Namespace <name> not exist" (fixed text "Namespace "/" not exist"; <name> is the request's `name` path label) when UpdateDataLakeNamespace targets a namespace already deleted by DeleteDataLakeNamespace.
Actual (emulator): HTTP 501, Error.Code = NotImplemented, Error.Message = "operation UpdateDataLakeNamespace is scaffolded but not implemented in this emulator".
Replay output: File "check_supplychain_namespace_create_get_list_roundtrip.py", line 85, in run — AssertionError: expected ResourceNotFoundException updating the deleted namespace, got NotImplemented.
Handler evidence: services/service-com-amazonaws-supplychain-galaxypublicapigateway/src/ops/update_data_lake_namespace.rs lines 3-6 (`SCAFFOLDED, NOT IMPLEMENTED` marker) and line 169 (`Err(crate::scaffolded::Scaffolded::not_implemented("UpdateDataLakeNamespace"))`).

## Steps to Reproduce
1. CreateInstance in us-east-1 and poll GetInstance until state == Active.
2. CreateDataLakeNamespace(instanceId, name=<^[a-z0-9_]+$ name>, description="sim2real namespace").
3. DeleteDataLakeNamespace(instanceId, name) — succeeds; GetDataLakeNamespace then correctly returns 404 ResourceNotFoundException "Namespace <name> not exist" and the name is gone from ListDataLakeNamespaces.
4. Call UpdateDataLakeNamespace(instanceId=instanceId, name=<the deleted name>, description="post-delete-update").
5. Observe HTTP 501 / NotImplemented instead of HTTP 404 / ResourceNotFoundException with message "Namespace <name> not exist".
6. Also call UpdateDataLakeNamespace against a LIVE namespace: it likewise returns 501 instead of 200 with the updated `namespace`.
