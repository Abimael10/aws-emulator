Issue-Source: trace-sweep-divergence
Issue-Trace: lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete
Issue-Shape-Id: com.amazonaws.lambdacore#LambdaCoreApiService
Issue-Key: lambda-core|ListNetworkConnectorVersions|2xx->2xx|$.NetworkConnectorVersions[*]
Issue-Key: lambda-core|ListNetworkConnectorVersions|2xx->2xx|$.NetworkConnectorVersions[*].StateReason
Issue-Key: lambda-core|DeleteNetworkConnector|2xx->2xx|$.State
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# lambda-core network-connector lifecycle transitions are not reflected in version and delete response state fields (3 causes)

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNetworkConnectorVersions

## Description
Three net-new root causes, all on 2xx responses, all the same underlying fault: the emulator's network-connector state model does not re-state the lifecycle narrative/state when a lifecycle transition is performed, so the version listing and the delete acknowledgement describe the connector as it was before the operation rather than as the operation left it. An existing open issue, 20261030T090527Z-lambda-core-updatenetworkconnector-never-reports-lastupdatestatusreason-and-leaves.md, covers the SAME fault as seen through UpdateNetworkConnector's and GetNetworkConnector's own projections (LastUpdateStatusReason / StateReason); it does NOT cover the three projections below (ListNetworkConnectorVersions version summaries and DeleteNetworkConnector), which replay reports as unowned.

Cause 1 of 3 - ListNetworkConnectorVersions reports the update-created version's StateReason as a generic configuration text instead of the update narrative.
AWS: Smithy ListNetworkConnectorVersions (GET /2026-04-04/network-connectors/{Identifier}/versions, success code 200) returns NetworkConnectorVersions -> NetworkConnectorVersionsList of NetworkConnectorVersionSummary, whose members are Arn, VersionArn, Name, Id, Version (long, required), State (NetworkConnectorState enum [PENDING, ACTIVE, INACTIVE, FAILED, DELETING, DELETE_FAILED], required), StateReason (String), StateReasonCode (NetworkConnectorStateReasonCode) and LastModified (CoreTimestamp, date-time). The model documents StateReason as 'A human-readable explanation of the current state'; recorded behavior: for the version a configuration update created, the summary's StateReason carries the fixed text 'Update initiated' - a status narrative naming the lifecycle event that produced the version, with no request- or state-derived variable part. The version number, ARNs, name, id and LastModified are environment values, not part of the requirement.
Emulator: answers 200 with the version summary present and otherwise correct, but StateReason set to the fixed text 'Configuration update' instead of 'Update initiated'. The only diverging member in the entry is StateReason (the comparator reports the whole element as missing/extra because one member differs; LastModified is not asserted).
Why: a client enumerating a connector's versions cannot tell which lifecycle event produced each version. Code that matches the state narrative to decide whether a version came from an update, and operators reading the narrative in a listing, both see a generic text AWS never emits for this transition, so update auditing and convergence checks keyed on the version listing never recognise the update.
Fix: the network-connector state model must record the update transition's state reason on the version it creates, and ListNetworkConnectorVersions' projection must emit that recorded reason rather than a hard-coded 'Configuration update' literal.
Traces: lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete#task#13 (and the same cluster's task#14).

Cause 2 of 3 - the same wrong version StateReason persists on a later listing, after the version-scoped delete.
AWS: same operation and shape as Cause 1. Recorded behavior: on the listing taken after the version-scoped delete, the remaining version summary at index 0 still reports StateReason with the fixed text 'Update initiated'; the narrative is a property of the version, so it survives unrelated lifecycle activity on the connector.
Emulator: answers 200 with $.NetworkConnectorVersions[0].StateReason = 'Configuration update' instead of 'Update initiated'.
Why: the wrong narrative is not a transient of the moment the update was accepted - it is what the emulator has stored for that version, so every later read of the listing is wrong too and a client that re-reads to confirm a version's provenance never converges on AWS's answer.
Fix: same component as Cause 1 - the stored per-version state reason in the connector state model, read by ListNetworkConnectorVersions' projection; fixing the literal at write time fixes both reads.
Traces: lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete#task#17.

Cause 3 of 3 - DeleteNetworkConnector acknowledges the delete with State ACTIVE instead of DELETING.
AWS: Smithy DeleteNetworkConnector (DELETE /2026-04-04/network-connectors/{Identifier}, success code 202, idempotent) outputs Arn, Name, Id, VersionArn, Version, Configuration, OperatorRole and State -> NetworkConnectorState, whose enum documents DELETING as 'Being deleted, cleaning up resources' and ACTIVE as 'Active and ready for use'. Recorded behavior: the accepted delete answers 202 with State = 'DELETING' - the 202 means the deletion was accepted and resource cleanup is in flight, so the acknowledged state is the deleting state, not the pre-delete state.
Emulator: answers 202 but with $.State = 'ACTIVE'.
Why: the delete acknowledgement contradicts itself - a client is told the request was accepted while the resource reports it is active and ready for use. A client that polls State on the delete response (or branches on it before polling GetNetworkConnector) concludes the deletion did not take, so it retries or reports a failed teardown; nothing in the emulator's response ever shows a resource leaving the ACTIVE state on delete.
Fix: DeleteNetworkConnector's handler must transition the connector (or the targeted version) into DELETING in the state model before building its response, and its serializer must project that state rather than the pre-delete one.
Traces: lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete#task#15.

## Evidence
Replay of corpus simparity-wire-aws-v1 (emulator_up=true, bootstrap_signal=false, steps_total=88, steps_mismatched=9, score_pct=89.8). All three causes come from fixture lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete, service lambda-core, and are reported with empty stub_path and empty owned_by.

Cause 1 - step task#13, ListNetworkConnectorVersions: status 200 expected and 200 actual. detail: $.NetworkConnectorVersions[1] missing/extra - expected element {'Version': 2, 'State': 'ACTIVE', 'StateReason': 'Update initiated', ...}, actual element identical except 'StateReason': 'Configuration update' (LastModified is <shaped-not-asserted>). divergence_key service=lambda-core|ListNetworkConnectorVersions|status=200->200|keys=$.NetworkConnectorVersions[1]; affected_count=2, the same divergence recurring at task#14 of the same fixture.

Cause 2 - step task#17, ListNetworkConnectorVersions: status 200 expected and 200 actual. detail: $.NetworkConnectorVersions[0].StateReason value_mismatch (expected 'Update initiated', got 'Configuration update'). divergence_key service=lambda-core|ListNetworkConnectorVersions|status=200->200|keys=$.NetworkConnectorVersions[0].StateReason; affected_count=1.

Cause 3 - step task#15, DeleteNetworkConnector: status 202 expected and 202 actual. detail: $.State value_mismatch (expected 'DELETING', got 'ACTIVE'). divergence_key service=lambda-core|DeleteNetworkConnector|status=202->202|keys=$.State; affected_count=1.

Model references (services/service-com-amazonaws-lambdacore-lambdacoreapiservice/smithy-model/com-amazonaws-lambdacore-lambdacoreapiservice.json): ListNetworkConnectorVersions http GET /2026-04-04/network-connectors/{Identifier}/versions code 200, output NetworkConnectorVersions (NetworkConnectorVersionsList, required, length 0..100) of NetworkConnectorVersionSummary {Arn, VersionArn, Name, Id, Version, State required, StateReason (String), StateReasonCode, LastModified}; DeleteNetworkConnector http DELETE /2026-04-04/network-connectors/{Identifier} code 202, output member State -> NetworkConnectorState; NetworkConnectorState enum values PENDING, ACTIVE, INACTIVE, FAILED, DELETING, DELETE_FAILED with DELETING documented 'Being deleted, cleaning up resources'.

## Steps to Reproduce
1. git_clone(), then replay_wire_corpus() to replay simparity-wire-aws-v1 against the emulator built from the checkout.
2. Cause 1: fixture lambdacore-list-network-connector-versions-after-update-and-version-scoped-delete, step task#13 (and task#14) - after a configuration update has created version 2, GET /2026-04-04/network-connectors/{Identifier}/versions and inspect the version-2 summary's StateReason: the emulator reports 'Configuration update' where AWS reports 'Update initiated'.
3. Cause 2: same fixture, step task#17 - repeat the versions listing after the version-scoped delete and inspect $.NetworkConnectorVersions[0].StateReason: still 'Configuration update' instead of 'Update initiated'.
4. Cause 3: same fixture, step task#15 - DELETE /2026-04-04/network-connectors/{Identifier} (version-scoped) and inspect the 202 body's State: the emulator reports 'ACTIVE' where AWS reports 'DELETING'.
