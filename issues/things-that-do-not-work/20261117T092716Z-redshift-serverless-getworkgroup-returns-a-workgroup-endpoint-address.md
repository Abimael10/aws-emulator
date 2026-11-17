Issue-Source: trace-sweep-divergence
Issue-Trace: redshiftserverless-data-api-batch-sql-round-trip
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: redshiftserverless-data-api-batch-sql-round-trip
Issue-Shape-Id: com.amazonaws.redshiftserverless#RedshiftServerless
Issue-Key: redshift-serverless|GetWorkgroup|2xx->2xx|$.workgroup.endpoint.address
Issue-Commit: e6ddac3093017cf938fcd5a77fd81a3fbc39c217

# redshift-serverless GetWorkgroup returns a workgroup endpoint address the emulator does not answer on

> _Note: this issue was identified against the code base at commit `e6ddac3093017cf938fcd5a77fd81a3fbc39c217`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetWorkgroup

## Description
Cause 1 of 1: GetWorkgroup advertises an endpoint host that is not an address the emulator serves, so the workgroup's connection endpoint is unusable.

AWS: GetWorkgroup (redshift-serverless, REST-JSON) returns 200 with a required `workgroup` member of shape Workgroup; `workgroup.endpoint` is an `Endpoint` structure whose `address` member is documented as "The DNS address of the VPC endpoint" and whose `port` is "The port that Amazon Redshift Serverless listens on" (Smithy: GetWorkgroup -> GetWorkgroupResponse.workgroup (required) -> Workgroup.endpoint -> Endpoint.address/Endpoint.port). On AWS the value in `address` is a host that actually resolves and accepts a connection for that workgroup; the DNS label pattern (workgroup name, account id, region, service DNS suffix) is the shape, not the requirement. The requirement is that `address` names the host serving that workgroup and `port` the port it listens on.

Emulator: the replayed step returns 200 and emits `$.workgroup.endpoint.address` as a hostname in the recorded `<workgroup>-<account>-<region>.redshift-serverless.amazonaws.com` form — a public AWS DNS name — rather than the host/port on which the emulator itself is reachable. The replay check classes this as `host_not_own`: the emulator announced an address other than the one it is reached on (detail: `$.workgroup.endpoint.address: host_not_own`). The generated name/account/region in the recorded value are per-environment and are not themselves the defect; announcing a host the emulator does not serve is.

Why: a client does exactly what the API tells it to — it reads `workgroup.endpoint.address` (plus `endpoint.port`) and opens a Redshift connection to it. Against the emulator that connection leaves for a public AWS DNS name (or fails to resolve at all), so any workflow that provisions a serverless workgroup and then connects to its endpoint cannot reach the emulator, even though every control-plane call appears to succeed with 200. Downstream data-API/JDBC steps that depend on the endpoint silently target the wrong host.

Fix: the GetWorkgroup handler / the workgroup state that materialises the Endpoint structure must populate `endpoint.address` and `endpoint.port` from the emulator's own externally reachable data listener (the same host:port the request arrived on), instead of synthesising an `*.amazonaws.com` DNS name. CreateWorkgroup/ListWorkgroups/UpdateWorkgroup must produce the same self-owned address so the value is stable across operations.

Traces: redshiftserverless-data-api-batch-sql-round-trip#task#1 (GetWorkgroup, expected 200 / actual 200, diverging key $.workgroup.endpoint.address, verdict mismatch / host_not_own).

## Evidence
Replay of fixture `redshiftserverless-data-api-batch-sql-round-trip`, step `task#1`, operation GetWorkgroup (service redshift-serverless):
  expected_status 200, actual_status 200
  detail: $.workgroup.endpoint.address: host_not_own (expected 'sim2real-probe-rss-wg-4fa464ab.000000000000.us-west-2.redshift-serverless.amazonaws.com', got 'sim2real-probe-rss-wg-4fa464ab.000000000000.us-west-2.redshift-serverless.amazonaws.com')
  divergence_key: service=redshift-serverless|GetWorkgroup|status=200->200|keys=$.workgroup.endpoint.address
  verdict: mismatch; stub_path: (none); owned_by: (none)

Expected behavior: `workgroup.endpoint.address` + `workgroup.endpoint.port` name a host/port that answers for this workgroup — against the emulator, its own reachable data endpoint.
Actual behavior: the emulator echoes a public `*.redshift-serverless.amazonaws.com` DNS name it does not serve, so the advertised endpoint is not reachable on the emulator (host_not_own).

Smithy evidence: Endpoint.address doc "The DNS address of the VPC endpoint."; Endpoint.port doc "The port that Amazon Redshift Serverless listens on."; GetWorkgroupResponse.workgroup is required; modeled errors InternalServerException(500), ResourceNotFoundException(404), ValidationException(400) — none apply here, the status is correctly 200 and only the endpoint address diverges.

Note: no currently open issue covers this. The nearest open redshift-serverless issues are `20261114T171103Z-redshift-serverless-listtagsforresource-answers-an-empty-tag-list.md` (tag listing) and `20261114T150622Z-redshift-serverless-tagresource-stub.md` (TagResource stub); neither touches the workgroup endpoint.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus `simparity-wire-aws-v1`.
2. Replay fixture `redshiftserverless-data-api-batch-sql-round-trip`, step `task#1`: a redshift-serverless GetWorkgroup call for the workgroup created earlier in the fixture.
3. Inspect the 200 response body at JSON path `$.workgroup.endpoint.address` (and `$.workgroup.endpoint.port`).
4. Observe the address is a public `*.redshift-serverless.amazonaws.com` DNS name rather than the host:port the emulator is reachable on; attempting to connect to the advertised address does not reach the emulator (replay flags `host_not_own`).
