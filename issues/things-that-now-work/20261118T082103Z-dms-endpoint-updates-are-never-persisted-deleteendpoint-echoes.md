Issue-Source: trace-sweep-divergence
Issue-Trace: b1-endpoint-lifecycle
Issue-Step: teardown#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: b1-endpoint-lifecycle
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms|DeleteEndpoint|2xx->2xx|$.Endpoint.DatabaseName
Issue-Key: dms|DeleteEndpoint|2xx->2xx|$.Endpoint.MySQLSettings.DatabaseName
Issue-Commit: a0e7eb49813c70256d7072893bf6da53a4aacf65

# dms endpoint updates are never persisted: DeleteEndpoint echoes the creation-time DatabaseName

> _Note: this issue was identified against the code base at commit `a0e7eb49813c70256d7072893bf6da53a4aacf65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteEndpoint

## Description
Cause 1 of 1: a DMS endpoint's mutable configuration is never updated in the emulator's store, so the endpoint projection returned by DeleteEndpoint (and by every other operation that answers with an `Endpoint`) still carries the values the endpoint was CREATED with rather than the values it last held.

AWS: DeleteEndpoint is modelled as `DeleteEndpointMessage{EndpointArn required} -> DeleteEndpointResponse{Endpoint: Endpoint}` and its output member is documented as "The endpoint that was deleted" (Smithy: DeleteEndpoint, errors InvalidResourceStateFault/ResourceNotFoundFault, both HTTP 400). ModifyEndpoint is modelled as `ModifyEndpointMessage{EndpointArn required, DatabaseName, MySQLSettings, ...} -> ModifyEndpointResponse{Endpoint}`, output documented as "The modified endpoint"; `DatabaseName` is "The name of the endpoint database" and `MySQLSettings` carries the per-engine settings for a MySQL endpoint. So on AWS the endpoint resource holds the LAST ACCEPTED value of `DatabaseName` and of `MySQLSettings.DatabaseName`, and the endpoint echoed by the delete is that resource — the one the preceding successful ModifyEndpoint produced, not the one CreateEndpoint produced.

Emulator: the step replays DeleteEndpoint on the lifecycle fixture's MySQL endpoint and answers HTTP 200 (status matches) with an endpoint whose `$.Endpoint.DatabaseName` and `$.Endpoint.MySQLSettings.DatabaseName` are both the database the endpoint was created with, where the recording has both carrying the database the endpoint was last modified to. The two members diverge together, which is the signature of an endpoint record that was never mutated rather than of a serializer that drops one member.

Hypothesis: `services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/create_endpoint.rs` persists `database_name` and the engine settings faithfully, and `delete_endpoint.rs` answers through `endpoint.to_shape()` on the stored record, so neither of those loses the value. `modify_endpoint.rs` is still a SCAFFOLDED scaffold whose `handle` returns `Scaffolded::not_implemented("ModifyEndpoint")`, i.e. HTTP 501 `NotImplemented`, so no endpoint mutation is ever written to the store and every later read of that endpoint serves creation-time configuration.

Why: a client that reconfigures an endpoint and then reads it back — on the modify response itself, on DescribeEndpoints, or on the endpoint echoed by DeleteEndpoint — is told the endpoint still points at the old database. Migration tooling that keys a source/target database off the endpoint it just updated (or that asserts the delete returned the configuration it expected to tear down) silently acts on the wrong database, and nothing in the response signals that the update was lost.

Fix: implement the ModifyEndpoint handler (`services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/modify_endpoint.rs`) so it resolves `EndpointArn` against `store.endpoints`, applies every supplied scalar member (`DatabaseName`, `EndpointIdentifier`, `EndpointType`, `EngineName`, `Username`, `ServerName`, `Port`, `ExtraConnectionAttributes`, `SslMode`, `CertificateArn`, `ServiceAccessRoleArn`, `ExternalTableDefinition`) and the per-engine settings bags (`MySQLSettings` and siblings, honouring `ExactSettings`) to the stored `EndpointRecord`, keeps `endpoint_arn_by_identifier` consistent when the identifier changes, refuses an unknown ARN with 400 `ResourceNotFoundFault` and a duplicate identifier with 400 `ResourceAlreadyExistsFault`, and answers with the SAME `to_shape()` projection the other endpoint operations use — so DeleteEndpoint's echo then reports the endpoint as last modified. The endpoint state record, not DeleteEndpoint's serializer, is where the value is missing.

No issue in issues/things-that-do-not-work/ currently covers ModifyEndpoint or this stale-endpoint echo; the open DMS issues cover replication-task settings/lifecycle (20261109T161403Z, 20261117T133104Z), DescribeReplicationTasks filtering (20261117T085157Z), DescribeReplicationSubnetGroups filter validation (20261117T115243Z) and the ModifyReplicationInstance stub (20261117T115243Z).

## Evidence
Fixture b1-endpoint-lifecycle, step teardown#10, operation DeleteEndpoint (service dms):
  expected_status 200, actual_status 200 — the status and the response shape match; only the endpoint's configuration diverges.
  $.Endpoint.DatabaseName: value_mismatch (expected 'payroll', got 'hr')
  $.Endpoint.MySQLSettings.DatabaseName: value_mismatch (expected 'payroll', got 'hr')

The two names are this one recording's values, not requirements: the requirement is that both members equal the database name the endpoint LAST accepted (the one supplied to the preceding ModifyEndpoint), while the emulator returns the one supplied to CreateEndpoint.

Supporting code evidence in the checkout:
  services/.../src/ops/create_endpoint.rs:413 — `database_name: input.database_name` and :301-322 the engine-settings bag are stored as sent (so creation is faithful).
  services/.../src/ops/delete_endpoint.rs:204-206 — the delete answers `endpoint.to_shape()` from the stored record (so the echo is faithful to whatever the record holds).
  services/.../src/ops/modify_endpoint.rs:4 and :269 — the file still carries the `SCAFFOLDED, NOT IMPLEMENTED` marker and `handle` returns `Scaffolded::not_implemented("ModifyEndpoint")` (HTTP 501), so the record is never updated.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Fixture b1-endpoint-lifecycle: CreateEndpoint a MySQL endpoint with DatabaseName = D1 and MySQLSettings.DatabaseName = D1; note the returned EndpointArn.
3. Call ModifyEndpoint on that EndpointArn with DatabaseName = D2 and MySQLSettings.DatabaseName = D2 (AWS answers 200 with the modified endpoint; the emulator answers 501 NotImplemented).
4. Call DeleteEndpoint with that EndpointArn — step teardown#10.
5. Observe HTTP 200 whose $.Endpoint.DatabaseName and $.Endpoint.MySQLSettings.DatabaseName are D1; AWS returns D2 in both, since the deleted endpoint is the endpoint as last modified.
