Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.domain-maintenance-status-window-expiry
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.domain-maintenance-status-window-expiry|run#10|AssertionError|assert final is not None, (
Issue-Commit: 971d79feed149d4c16efbd611c6c26ef8e404349

# opensearch: an accepted RESTART_SEARCH_PROCESS maintenance action never reaches a terminal GetDomainMaintenanceStatus Status (1 cause)

> _Note: this issue was identified against the code base at commit `971d79feed149d4c16efbd611c6c26ef8e404349`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDomainMaintenanceStatus

## Description
This issue carries 1 root cause, in the opensearch service. It must be closed before submitting.

Cause 1 of 1: a node maintenance action the emulator ACCEPTED never advances to a terminal status on the reads a polling client makes. GetDomainMaintenanceStatus keeps answering a non-terminal Status for the whole execution window, so the action's completion — and the terminal row ListDomainMaintenances must then show — is unobservable end to end. Failing check: opensearch.domain-maintenance-status-window-expiry.

Note on prior filings: the resolved issue 20261109T160051Z-opensearch-a-restart-search-process-maintenance-action-never.md carries exactly this check's replay key and is marked fixed, yet the check still fails on the same non-terminal-forever behavior, so that fix did not land this behavior (the calls are not refused — they answer 200 with a non-terminal Status, so this is not a refusal regression). The open issue 20261030T115510Z-opensearch-the-startdomainmaintenance-getdomainmaintenancestatus-lifecycle-never-reaches-a.md describes the same maintenance state machine but carries only the replay key of opensearch.domain-maintenance-lifecycle; it does not carry this check, so this failure is filed here and should be closed together with it.

AWS: Smithy (com.amazonaws.opensearch#AmazonOpenSearchService): StartDomainMaintenance POST /2021-01-01/opensearch/domain/{DomainName}/domainMaintenance — DomainName @httpLabel required @length 3..28 @pattern ^[a-z][a-z0-9\-]+$, Action MaintenanceType required (REBOOT_NODE|RESTART_SEARCH_PROCESS|RESTART_DASHBOARD), NodeId @length 10..40 — answers 200 {MaintenanceId: RequestId @length 1..100}. GetDomainMaintenanceStatus GET /2021-01-01/opensearch/domain/{DomainName}/domainMaintenance?maintenanceId=<id> answers 200 {Status: MaintenanceStatus enum PENDING|IN_PROGRESS|COMPLETED|FAILED|TIMED_OUT, StatusMessage, NodeId, Action, CreatedAt, UpdatedAt}. ListDomainMaintenances GET /2021-01-01/opensearch/domain/{DomainName}/domainMaintenances answers DomainMaintenances. Recorded behavior only for the progression itself: on a 3-data-node domain, once DescribeDomainNodes lists a node id, StartDomainMaintenance(Action='RESTART_SEARCH_PROCESS', NodeId=<a listed data node>) is accepted and the action then advances to a TERMINAL status within its execution window on ordinary client polls with no further client-driven step; the terminal value a successful action reports is the unmodelled string 'SUCCESS' (outside the modelled MaintenanceStatus enum), and ListDomainMaintenances lists the same MaintenanceId with that status. The requirement exercised here is that the accepted action becomes terminal on the reads a client actually makes.

Emulator: the sequence CreateDomain(ClusterConfig.InstanceCount=3) -> poll DescribeDomain until Processing=False -> poll DescribeDomainNodes for a NodeId -> StartDomainMaintenance(RESTART_SEARCH_PROCESS, NodeId=<first listed node>) -> GetDomainMaintenanceStatus polled at a client-realistic interval never yields a Status in {SUCCESS, COMPLETED, FAILED, TIMED_OUT}. The poll loop exhausts the whole execution window and the check fails with AssertionError 'maintenance action never reached a terminal Status within its execution window, with no additional client-driven step issued'. The downstream assertions (final Status == 'SUCCESS', and the started MaintenanceId listed by ListDomainMaintenances with that status) are therefore never reached.

Why: a client that starts node maintenance has no way to learn that it finished. Every AWS-targeting automation around RESTART_SEARCH_PROCESS / REBOOT_NODE / RESTART_DASHBOARD starts the action, takes the MaintenanceId and polls GetDomainMaintenanceStatus until the status is terminal; against the emulator that loop never exits, so any subsequent step gated on completion blocks forever, and the negative outcomes (FAILED/TIMED_OUT) are equally unreachable.

Fix: the maintenance state machine behind the StartDomainMaintenance / GetDomainMaintenanceStatus / ListDomainMaintenances handlers and the domain state they read — an accepted action must progress through its non-terminal statuses to a terminal one over the reads a client actually makes (not only after wall-clock spans that each consume a client poll interval), and the terminal value a successful action reports must be the 'SUCCESS' AWS reports, carried identically by ListDomainMaintenances for the same MaintenanceId. Only the fixed text of a status value is a requirement here; the domain name, node id, maintenance id and poll timings are per-run values.

Traces: opensearch.domain-maintenance-status-window-expiry.

## Evidence
Failing check: [mismatch] opensearch.domain-maintenance-status-window-expiry
AssertionError: maintenance action never reached a terminal Status within its execution window, with no additional client-driven step issued (check_opensearch_domain_maintenance_status_window_expiry.py line 63).

Expected (AWS, recorded): start_domain_maintenance(DomainName=<d>, Action='RESTART_SEARCH_PROCESS', NodeId=<node id read from describe_domain_nodes>) -> 200 {MaintenanceId}; get_domain_maintenance_status(DomainName=<d>, MaintenanceId=<id>) polled at a client-realistic interval with no other call in between returns a terminal Status within the action's execution window, with final Status == 'SUCCESS' (a value outside the modelled MaintenanceStatus enum PENDING|IN_PROGRESS|COMPLETED|FAILED|TIMED_OUT) and Action/NodeId echoing the request; list_domain_maintenances(DomainName=<d>) then contains an entry whose MaintenanceId equals the started action's id with Status 'SUCCESS'.

Actual (emulator): every poll inside the window answers 200 with a non-terminal Status — never one of SUCCESS/COMPLETED/FAILED/TIMED_OUT — so no terminal status and no 'SUCCESS' row is ever observable, and the check's terminal-status and ListDomainMaintenances assertions are never reached.

Fixed text vs variable parts: the status token ('SUCCESS', and the enum values PENDING/IN_PROGRESS/COMPLETED/FAILED/TIMED_OUT) is fixed text and is the requirement; the domain name, node id, MaintenanceId, timestamps and the concrete poll timings are per-run values taken from the request and emulator state and are not part of it.

## Steps to Reproduce
1. opensearch.create_domain(DomainName=<lowercase name matching ^[a-z][a-z0-9\-]+$, 3..28 chars>, ClusterConfig={'InstanceType':'t3.small.search','InstanceCount':3}, EBSOptions={'EBSEnabled':True,'VolumeType':'gp3','VolumeSize':10}, AccessPolicies=<es:* for the account root on the domain>).
2. Poll opensearch.describe_domain(DomainName=<d>) until DomainStatus.Processing is False.
3. Poll opensearch.describe_domain_nodes(DomainName=<d>) until DomainNodesStatusList contains an entry with a NodeId; node_id = that NodeId (retry on DependencyFailureException).
4. opensearch.start_domain_maintenance(DomainName=<d>, Action='RESTART_SEARCH_PROCESS', NodeId=node_id) -> MaintenanceId (retry on DependencyFailureException/ThrottlingException and on ResourceNotFoundException 'The requested node ID is invalid').
5. Poll opensearch.get_domain_maintenance_status(DomainName=<d>, MaintenanceId=<id>) every 5 s with no other call in between: the emulator never returns a Status in {SUCCESS, COMPLETED, FAILED, TIMED_OUT}. On AWS the Status becomes 'SUCCESS' within the execution window.
6. opensearch.list_domain_maintenances(DomainName=<d>) and assert the started MaintenanceId is listed with Status 'SUCCESS' — unreachable against the emulator.
7. Re-replay opensearch.domain-maintenance-status-window-expiry to confirm.
