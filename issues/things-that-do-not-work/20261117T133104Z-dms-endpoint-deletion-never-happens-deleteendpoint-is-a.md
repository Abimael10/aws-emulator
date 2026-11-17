Issue-Source: trace-sweep-divergence
Issue-Trace: endpoint-delete-identifier-release
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endpoint-delete-identifier-release
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms|DescribeEndpoints|4xx->2xx|$
Issue-Key: dms|DescribeEndpoints|4xx->2xx|$.__type
Issue-Key: dms|DescribeEndpoints|4xx->2xx|$.message
Issue-Key: dms|DescribeEndpoints|4xx->2xx|status
Issue-Key: dms|CreateEndpoint|2xx->4xx|$.Endpoint
Issue-Key: dms|CreateEndpoint|2xx->4xx|status
Issue-Key: dms|DescribeEndpoints|2xx->2xx|$.Endpoints[*].EndpointArn
Issue-Key: dms|DescribeEndpoints|2xx->2xx|$.Endpoints[*].PostgreSQLSettings.ServerName
Issue-Key: dms|DescribeEndpoints|2xx->2xx|$.Endpoints[*].ServerName
Issue-Commit: 36e91b0a54420c8643974af3d3528aa1f799565b

# DMS endpoint deletion never happens: DeleteEndpoint is a 501 scaffold, so a deleted endpoint is still described and its identifier is never released

> _Note: this issue was identified against the code base at commit `36e91b0a54420c8643974af3d3528aa1f799565b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoints

## Description
Three net-new wire divergences, all from fixture endpoint-delete-identifier-release, service 'dms'. The replay is a fidelity replay, not a bootstrap gap (emulator_up=true, bootstrap_signal=false, 14 fixtures / 180 steps replayed). None of the three clusters carries stub_path or owned_by. They share ONE root cause: DeleteEndpoint is still an unimplemented scaffold (services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/delete_endpoint.rs lines 4-12 carry the SCAFFOLDED marker and line 154 returns Scaffolded::not_implemented("DeleteEndpoint")), so an endpoint is never removed from the store and its EndpointIdentifier is never freed. That operation's own stub issue is filed by the replay harness and is NOT this issue; the three causes below are the OBSERVABLE wire divergences the gap produces on three other operations, and each must be re-replayed to close this issue. Open issue 20261109T161403Z-dms-replication-task-lifecycle-never-completes-task-settings.md describes the task/instance side of DMS teardown and does not cover endpoints at all.

Cause 1 of 3: after the endpoint has been deleted, DescribeEndpoints filtered on that endpoint still answers HTTP 200 with the endpoint, where AWS refuses with HTTP 400 ResourceNotFoundFault. Fixture endpoint-delete-identifier-release step task#3 (DescribeEndpoints).
AWS: Smithy DescribeEndpoints models exactly one error, ResourceNotFoundFault (HTTP 400, 'The resource could not be found'); DeleteEndpoint 'Deletes the specified endpoint' and returns the deleted Endpoint at HTTP 200. Once the delete has been applied, a describe whose Filters narrow to that endpoint matches nothing and is refused with ResourceNotFoundFault. The recorded refusal body is {"__type": "ResourceNotFoundFault", "message": "No Endpoints found matching provided filters"} — the message is fixed text for this error class with NO variable part (the request's filter values are not quoted back).
Emulator: the step polls DescribeEndpoints and gets HTTP 200 with Endpoints[0] = the supposedly deleted endpoint (Status 'active', its original EndpointArn and ServerName) on all 13 attempts until the 60s poll cap; the harness reports 'poll: error code None not in [ResourceNotFoundFault] after 13 attempt(s)' and '$.__type: missing', '$.message: missing'. The handler's own narrows-to-nothing branch is correct (src/ops/describe_endpoints.rs lines 168-179 raise ResourceNotFoundFault with exactly the message 'No Endpoints found matching provided filters', src/ops/describe_endpoints.rs line 203); it is unreachable here only because the record was never removed.
Why: a client that waits for its endpoint teardown to finish — the documented poll-until-ResourceNotFoundFault pattern — never observes the endpoint leave, so teardown loops until its own timeout and the client believes a deleted resource still exists.
Fix: implement the DeleteEndpoint handler (remove the EndpointRecord from the store and drop its endpoint_arn_by_identifier entry, answering HTTP 200 with the deleted Endpoint; refuse an endpoint still referenced by a replication task with the modeled InvalidResourceStateFault). The DescribeEndpoints handler needs no change.
Traces: endpoint-delete-identifier-release#task#3. Code anchors: src/ops/delete_endpoint.rs lines 137-155; src/ops/describe_endpoints.rs lines 162-179; src/state.rs lines 993-996 (endpoints / endpoint_arn_by_identifier) and 1014-1057 (settle, which has no endpoint-removal branch).

Cause 2 of 3: re-creating an endpoint under the identifier the delete should have released is refused with HTTP 400 ResourceAlreadyExistsFault, where AWS accepts it with HTTP 200 and a NEW endpoint. Fixture step task#5 (CreateEndpoint).
AWS: Smithy CreateEndpoint returns HTTP 200 with the created Endpoint and models ResourceAlreadyExistsFault (HTTP 400) only for an EndpointIdentifier that is in use at that moment. An EndpointIdentifier is unique only among the endpoints the account HOLDS, so after the endpoint carrying it is deleted the same identifier may be used again, and the create mints a fresh EndpointArn. The recorded response is a new TARGET postgres endpoint with the same EndpointIdentifier, Status 'active', ServerName/PostgreSQLSettings.ServerName naming the second server the request carried, and a newly minted EndpointArn.
Emulator: the step answers HTTP 400 and no Endpoint member ('status: expected 200, got 400'; '$.Endpoint: missing'), because store.endpoint_arn_by_identifier still holds the undeleted endpoint's ARN under that identifier and the handler refuses with ResourceAlreadyExistsFault and the message 'ReplicationEndpoint "<EndpointIdentifier>" already in use.' (src/ops/create_endpoint.rs lines 355-369) — fixed text with the caller's EndpointIdentifier as its only variable part, plus the existing ARN in the modeled resourceArn member. That refusal RULE and its wording are AWS's and must NOT be loosened; it is wrong here only because the identifier should already be free.
Why: an identifier is not reusable for the life of the emulator process, so any create/delete/re-create flow — the normal way a client re-points an endpoint at a different server, and the normal way a test re-runs — fails permanently with an error naming a resource the client already deleted.
Fix: the DeleteEndpoint handler's removal path (it must free the endpoint_arn_by_identifier entry as well as the endpoints entry), in the service's state module's endpoint store. Do not change CreateEndpoint's duplicate gate.
Traces: endpoint-delete-identifier-release#task#5. Code anchors: src/ops/create_endpoint.rs lines 352-369; src/ops/delete_endpoint.rs line 154; src/state.rs lines 993-996.

Cause 3 of 3: a describe by endpoint identifier after the re-create reports the STALE endpoint — the original ARN and the original ServerName — instead of the endpoint that now carries that identifier. Fixture step task#6 (DescribeEndpoints).
AWS: DescribeEndpoints reports the endpoints the account currently holds that match the request's Filters; after a delete plus re-create under the same identifier, a filter on that identifier resolves to the NEW endpoint, so EndpointArn is the newly minted one and ServerName / PostgreSQLSettings.ServerName are the second server the re-create named. Smithy: Endpoint$EndpointArn 'The Amazon Resource Name (ARN) string that uniquely identifies the endpoint'; Endpoint$ServerName 'The name of the server at the endpoint'; the engine-settings copy is the same connection parameter (PostgreSQLSettings$ServerName).
Emulator: the step answers HTTP 200 but with the first endpoint: '$.Endpoints[0].EndpointArn: value_mismatch (expected the re-created endpoint's ARN, got the original ARN)', '$.Endpoints[0].ServerName' and '$.Endpoints[0].PostgreSQLSettings.ServerName' both still report the FIRST server name instead of the second. This follows from Causes 1 and 2: the old record is still in the store and the re-create never happened.
Why: a client that re-creates an endpoint to change its server reads back the old server and the old ARN, so a replication task built from that describe is pointed at the wrong database and the client cannot detect it.
Fix: the same DeleteEndpoint removal path; once the deleted record leaves the store and the identifier is freed, this step's describe resolves to the re-created endpoint through the single EndpointRecord::to_shape projection (src/state.rs lines 764-795), which needs no change.
Traces: endpoint-delete-identifier-release#task#6. Code anchors: src/ops/delete_endpoint.rs lines 137-155; src/ops/describe_endpoints.rs lines 162-198.

## Evidence
Replay of trace set simparity-wire-aws-v1: 14 fixtures replayed, 180 steps (75 passed, 102 mismatched, 0 errored, 3 blocked), score 41.7%, divergence_count 8. emulator_up=true, bootstrap_signal=false, emulator_detail: '/__admin/health answered 200 and the :37587 data listener accepts'. All three clusters below are service 'dms', fixture endpoint-delete-identifier-release, with stub_path empty and owned_by empty.

[mismatch] task#3 DescribeEndpoints — status: expected 400, got 200.
  Expected (recording): HTTP 400, body {"__type": "ResourceNotFoundFault", "message": "No Endpoints found matching provided filters"}.
  Actual: HTTP 200 with Endpoints[0] = {EndpointIdentifier: <the deleted endpoint's identifier>, EndpointType 'TARGET', EngineName 'postgres', Status 'active', its original EndpointArn, ServerName = the FIRST server}; '$.__type: missing', '$.message: missing', 'poll capped at 60s', 'poll: error code None not in [ResourceNotFoundFault] after 13 attempt(s)'.
  divergence_key: service=dms|DescribeEndpoints|status=400->200|keys=$,$.__type,$.message,status

[mismatch] task#5 CreateEndpoint — status: expected 200, got 400.
  Expected (recording): HTTP 200 with Endpoint = {EndpointIdentifier <same identifier>, EndpointType 'TARGET', EngineName 'postgres', EngineDisplayName 'PostgreSQL', Status 'active', DatabaseName 'inventory', Port 5432, ServerName and PostgreSQLSettings.ServerName = the SECOND server, a freshly minted EndpointArn, SslMode 'none', IsReadOnly false}.
  Actual: HTTP 400 with no Endpoint member ('$.Endpoint: missing'); the handler's refusal is ResourceAlreadyExistsFault with message 'ReplicationEndpoint "<EndpointIdentifier>" already in use.' (src/ops/create_endpoint.rs lines 355-369).
  divergence_key: service=dms|CreateEndpoint|status=200->400|keys=$.Endpoint,status

[mismatch] task#6 DescribeEndpoints — expected 200, got 200, body diverges:
  $.Endpoints[0].EndpointArn: value_mismatch (expected the re-created endpoint's ARN, got the ORIGINAL endpoint's ARN).
  $.Endpoints[0].ServerName and $.Endpoints[0].PostgreSQLSettings.ServerName: value_mismatch (expected the SECOND server name the re-create carried, got the FIRST one).
  divergence_key: service=dms|DescribeEndpoints|status=200->200|keys=$.Endpoints[0].EndpointArn,$.Endpoints[0].PostgreSQLSettings.ServerName,$.Endpoints[0].ServerName

Model consulted: smithy_query query=operation operation=CreateEndpoint,DeleteEndpoint,DescribeEndpoints,DescribeTableStatistics and query=shape shape=Endpoint,ResourceNotFoundFault,ResourceAlreadyExistsFault on services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/smithy-model/com-amazonaws-databasemigrationservice-amazondmsv20160101.json. DeleteEndpoint: input EndpointArn (required), output Endpoint, errors InvalidResourceStateFault / ResourceNotFoundFault (both HTTP 400), success 200. DescribeEndpoints models only ResourceNotFoundFault (HTTP 400). CreateEndpoint models ResourceAlreadyExistsFault (HTTP 400, with a resourceArn member) and returns Endpoint at 200.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay wire fixture endpoint-delete-identifier-release from trace set simparity-wire-aws-v1; steps task#3, task#5 and task#6 diverge.
3. Shorter repro against DMS alone:
   a. CreateEndpoint(EndpointIdentifier='X', EndpointType='target', EngineName='postgres', ServerName='pg1.example.invalid', Port=5432, DatabaseName='inventory', Username='u'): HTTP 200, note EndpointArn.
   b. DeleteEndpoint(EndpointArn=<that ARN>): HTTP 501 NotImplemented (the scaffold), where AWS answers HTTP 200 with the deleted Endpoint.
   c. DescribeEndpoints(Filters=[{Name:'endpoint-id', Values:['X']}]): HTTP 200 with the endpoint still present, where AWS answers HTTP 400 ResourceNotFoundFault with message 'No Endpoints found matching provided filters' (Cause 1).
   d. CreateEndpoint again with EndpointIdentifier='X' and ServerName='pg2.example.invalid': HTTP 400 ResourceAlreadyExistsFault 'ReplicationEndpoint "X" already in use.', where AWS answers HTTP 200 with a new endpoint (Cause 2).
   e. DescribeEndpoints(Filters=[{Name:'endpoint-id', Values:['X']}]) once more: the reported EndpointArn and ServerName/PostgreSQLSettings.ServerName are the FIRST endpoint's, not the re-created one's (Cause 3).
