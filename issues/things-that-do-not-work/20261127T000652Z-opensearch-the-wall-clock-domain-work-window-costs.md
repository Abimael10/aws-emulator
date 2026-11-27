Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.domain-maintenance-status-window-expiry
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.domain-maintenance-status-window-expiry|run#10|AssertionError|assert final is not None, (
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# opensearch: the wall-clock DOMAIN_WORK window costs a polling client its whole first poll interval, so a RESTART_SEARCH_PROCESS maintenance action is never observed reaching its terminal Status (SUCCESS)

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDomainMaintenanceStatus

## Description
One net-new root cause on the opensearch control plane, grounded in the one failing behavioral check the replay report marked neither ALREADY FILED nor OWNED.

Cause 1 of 1: every transient lifecycle state this control plane publishes is paced in WALL-CLOCK time and only materialised to its next value once a fixed span has elapsed, so a client that polls a domain always reads the transient value on its first probe and must burn a full poll interval before the settled value can be observed. For a freshly created domain that span is `state::DOMAIN_WORK` (3 s), and `MAINTENANCE_STEP` is defined as the same span, applied twice (PENDING -> IN_PROGRESS -> terminal). The consequence is that a client which creates a domain, waits for it to settle, starts a node maintenance action and then polls for its outcome has already spent its budget on the domain wait and never reaches the terminal maintenance status. Failing check: opensearch.domain-maintenance-status-window-expiry.

AWS: Smithy (com.amazonaws.opensearch#AmazonOpenSearchService) — `StartDomainMaintenance` (POST /2021-01-01/opensearch/domain/{DomainName}/domainMaintenance) takes `Action` (MaintenanceType, required) and `NodeId` (@length 10..40) and answers `MaintenanceId` (RequestId, @length 1..100); `GetDomainMaintenanceStatus` (GET .../domainMaintenance?maintenanceId=) answers `Status` (MaintenanceStatus), `StatusMessage`, `NodeId`, `Action`, `CreatedAt`, `UpdatedAt`; `ListDomainMaintenances` answers the same projection per action as `DomainMaintenanceDetails`. The modelled `MaintenanceStatus` enum lists COMPLETED | FAILED | IN_PROGRESS | PENDING | TIMED_OUT, and recorded behavior shows AWS reports a FINISHED action as `SUCCESS` — a value outside the modelled enum — and `ListDomainMaintenances` then lists that same `MaintenanceId` with the same `SUCCESS` status. The model says nothing about how long the walk takes, so the pacing is recorded behavior only: what the recording shows is that each state of the walk is reachable by the NEXT poll a client issues, i.e. the number of a client's own poll intervals a conforming implementation consumes is not fixed by the service's wall clock. No refusal is involved — every call in this sequence succeeds with HTTP 200 on AWS.

Emulator: every individual answer is right — `StartDomainMaintenance` mints the id and writes the record `PENDING`, `Maintenance::settle` walks it to `IN_PROGRESS` and then to `SUCCESS` (the emulator deliberately stores the unmodelled `SUCCESS`), and `GetDomainMaintenanceStatus`/`ListDomainMaintenances` project it consistently. What diverges is WHEN those states become readable: the due instants are fixed wall-clock offsets, so the first `DescribeDomain` issued immediately after `CreateDomain` necessarily reports `Processing=true` (the domain's work window has not elapsed), the client sleeps one whole poll interval (30 s in the replayed check) before it can read `Processing=false`, and by then the check's own remaining-time guard has collapsed its maintenance-poll deadline to zero, so `GetDomainMaintenanceStatus` is polled zero times and the action is never read terminal. The replay ends at the check's own assertion, never at a wrong value: `AssertionError: maintenance action never reached a terminal Status within its execution window, with no additional client-driven step issued`. No wrong HTTP status, error code or error message was observed for any operation in the sequence.

Why: a client that starts a node maintenance action and polls for its outcome pays one full poll interval per transition no matter how tightly it polls, because each transition is gated on wall clock rather than on progress/observation. Any caller whose own deadline is in the tens of seconds (a test harness, an orchestrator step, an interactive CLI wait loop) concludes the action is hung in a non-terminal status on a sequence real AWS completes, and in this suite it means the terminal `SUCCESS` status and the post-completion `ListDomainMaintenances` view are not exercised at all — the emulator's answers for them are unverified.

Fix: the pacing of transient state in the opensearch control plane's domain/maintenance state module — the window a newly created domain holds before it reports settled, and the per-step maintenance windows the maintenance record settles against — so that a state a client must observe is observable on the next call it issues rather than only after a fixed wall-clock span. The projections themselves need no change.

Traces: opensearch.domain-maintenance-status-window-expiry. Code: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state.rs lines 794-806 (`DOMAIN_WORK` = 3 s, the domain work window) and src/state/domains.rs lines 542-575 (`Domain::begin_maintenance`, which sets `starts_at = now + MAINTENANCE_STEP` and `completes_at = now + 2 * MAINTENANCE_STEP`), 1673-1690 (`MAINTENANCE_STEP = DOMAIN_WORK`), 1726-1737 (`Maintenance::settle`), 764-774 (`Domain::settle` driving it).

Note: two open issues describe the same FAMILY and are named here so a fixer can land them together, but neither carries this check's replay key, so this cause is filed separately: 20261121T121321Z-opensearch-wall-clock-transient-windows-cost-a-client.md (six chained-wait checks on the package/VPC-endpoint/service-software windows) and 20261126T065055Z-opensearch-2-net-new-causes-the-domain-data.md (whose Cause 2 is the SIBLING check opensearch.domain-maintenance-lifecycle on the same maintenance walk). This check is the second, independent grounded example of the maintenance walk and is not listed in either issue's Issue-Trace-Ids.

## Evidence
Behavioral suite replay against the emulator built from this checkout: 310/364 passed (85.2%), 46 mismatched, 8 errored, 0 skipped. Of the 54 failures, 52 are marked OWNED by an already-filed issue. This issue covers the one remaining emulator-attributable failure.

### opensearch
[mismatch] opensearch.domain-maintenance-status-window-expiry
  File "check_opensearch_domain_maintenance_status_window_expiry.py", line 63, in run
    assert final is not None, (
  AssertionError: maintenance action never reached a terminal Status within its execution window,
  with no additional client-driven step issued

Sequence the check drives: CreateDomain (ClusterConfig t3.small.search x 3, EBS gp3 10 GiB, AccessPolicies) -> poll DescribeDomain every 30 s until Processing=false -> poll DescribeDomainNodes until a NodeId is listed -> StartDomainMaintenance(Action="RESTART_SEARCH_PROCESS", NodeId=<listed data node>) -> poll GetDomainMaintenanceStatus every 5 s for a terminal Status -> ListDomainMaintenances.

Expected (AWS, recorded behavior): GetDomainMaintenanceStatus walks the action PENDING -> IN_PROGRESS -> Status "SUCCESS" (fixed text; AWS sends SUCCESS, a value outside the modelled MaintenanceStatus enum COMPLETED|FAILED|IN_PROGRESS|PENDING|TIMED_OUT), reporting back the Action the request named and the NodeId it named, and ListDomainMaintenances then lists the same MaintenanceId with that same terminal status. The variable parts of that projection are the MaintenanceId the service minted, the NodeId taken from the request and the CreatedAt/UpdatedAt instants; the fixed text is the status value SUCCESS and the Action value RESTART_SEARCH_PROCESS echoed from the request. No error code or message is involved — every call answers 200.

Actual (emulator): each call answers correctly, but the states are only readable after fixed wall-clock spans, so the client loses its whole first poll interval to the domain window and the terminal-status poll never happens:
  - domain provisioning: DescribeDomain reports Processing=true until state::DOMAIN_WORK (3 s, src/state.rs:806) has elapsed, so the first probe after CreateDomain always reads the transient value and the client sleeps its full 30 s interval.
  - maintenance walk: begin_maintenance writes Status="PENDING" with starts_at = now + MAINTENANCE_STEP and completes_at = now + 2 * MAINTENANCE_STEP, where MAINTENANCE_STEP = DOMAIN_WORK (src/state/domains.rs:1690), so IN_PROGRESS and SUCCESS are each withheld for a further fixed span rather than being reachable on the client's next poll.
The terminal-status assertion is therefore never evaluated and the post-completion ListDomainMaintenances read is never issued. The divergence is the wall-clock gating of observability, not a wrong status, error code or message.

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator from it.
2. CreateDomain with ClusterConfig {InstanceType: t3.small.search, InstanceCount: 3} (three data nodes, so RESTART_SEARCH_PROCESS is not refused as a disabled operation) and EBSOptions {EBSEnabled: true, VolumeType: gp3, VolumeSize: 10}.
3. Immediately call DescribeDomain. It reports Processing=true because the domain's work window (state::DOMAIN_WORK = 3 s, services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state.rs:794-806) has not elapsed. A client polling on a realistic interval (the check uses 30 s) therefore loses a whole interval before it can read Processing=false.
4. Poll DescribeDomainNodes until a NodeId is listed, then call StartDomainMaintenance(DomainName, Action="RESTART_SEARCH_PROCESS", NodeId=<that node>) and keep the MaintenanceId.
5. Poll GetDomainMaintenanceStatus for a terminal Status. Confirm the cause by observation rather than by sleeping: re-read in a tight loop and note that Status leaves "PENDING" only once MAINTENANCE_STEP has elapsed and reaches "SUCCESS" only after a second MAINTENANCE_STEP (src/state/domains.rs:552-575 and 1726-1737), instead of being reachable on the next read.
6. After the fix, GetDomainMaintenanceStatus must be observable walking PENDING -> IN_PROGRESS -> SUCCESS on successive client calls, and ListDomainMaintenances must then list that MaintenanceId with Status SUCCESS.
7. Re-replay exactly the check id in trace_ids: opensearch.domain-maintenance-status-window-expiry.
