Issue-Source: trace-sweep-divergence
Issue-Trace: agent-registry-discoverable-record-filter-validation
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: agent-registry-discoverable-record-filter-validation
Issue-Shape-Id: com.amazonaws.agentregistry#AgentRegistry
Issue-Key: agent-registry|ListDiscoverableRegistryRecords|4xx->2xx|$.message
Issue-Key: agent-registry|ListDiscoverableRegistryRecords|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: agent-registry|ListDiscoverableRegistryRecords|4xx->2xx|status
Issue-Commit: 0cc1593284dc57d1fa665d25f671e4cff118eb06

# ListDiscoverableRegistryRecords never validates filter VALUES: a filter carrying more than one value is served 200 instead of 400 ValidationException

> _Note: this issue was identified against the code base at commit `0cc1593284dc57d1fa665d25f671e4cff118eb06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDiscoverableRegistryRecords

## Description
ONE net-new root cause on service `agent-registry` (AgentRegistry, restJson1, sigv4), grounded in a single wire-replay divergence cluster of 3 steps in fixture agent-registry-discoverable-record-filter-validation (steps task#4, task#5, task#6 — the representative is task#4). The service is bootstrapped in this build (crate services/service-com-amazonaws-agentregistry-agentregistry present; get_bootstrap_status reports nothing pending) and answers the corpus (45/49 steps pass, score 91.8%), so this is a handler/validation gap, not a bootstrap or routing gap.

Cause 1 of 1: ListDiscoverableRegistryRecords accepts a registry-record filter whose `values` list carries more than one value (and, on the same code path, a value outside the filtered dimension's value set) and serves it as a successful read instead of refusing the request with the modeled ValidationException.

AWS: Smithy ListDiscoverableRegistryRecords — POST /registries/{registryId}/discoverable-records-list, success code 200, @readonly, @paginated; input member `filters` -> RegistryRecordFilterList @length max 10 of RegistryRecordFilter{name: REQUIRED enum RegistryRecordFilterName [recordType, descriptorType], values: REQUIRED DiscoverableFilterValues @length 1..10 of FilterValue @length 1..255}; modeled error ValidationException (errorKind client, httpError 400; members message REQUIRED, reason REQUIRED enum ValidationExceptionReason default FieldValidationFailed, fieldList). Because the list shape itself admits up to 10 values, the one-value-per-filter rule is recorded behavior only: the recording shows AWS refusing a multi-value filter with HTTP 400, x-amzn-errortype ValidationException, and message "Filter recordType must specify exactly one value." — fixed text "Filter " ... " must specify exactly one value.", whose ONLY variable part is the filter NAME taken verbatim from the request's filters[].name (here `recordType`; `descriptorType` fills the same template). The recorded refusal of a value that is not a member of the filtered field's value set is worded "Invalid value for filter <name>: <value>" — fixed text "Invalid value for filter " ... ": " with the filter name and the offending value both taken from the request. No id, generated name or timestamp is part of either template.

Emulator: the request is served as a successful listing — HTTP 200 with a `registryRecords` body and NO error members at all. Replay reports for the representative step: status expected 400, got 200; $.message missing (expected 'Filter recordType must specify exactly one value.', got None); $headers.x-amzn-errortype missing (expected the shaped ValidationException errortype, got None). All three steps of the cluster diverge the same way (status=400->200, same missing keys $.message and $headers.x-amzn-errortype), i.e. no filter-VALUE validation exists on this path: the handler validates only `filters.len()` against the 10-filter cap and `filters[].name` against the RegistryRecordFilterName value set, then applies the values as an OR-ed match set.

Why: a client that mis-spells a record/descriptor type, or passes a list where exactly one value is accepted, is silently served a WIDER page than it asked for instead of being told its input is invalid. A filter typo degrades into what looks like a legitimate result set, and discovery logic that trusts the filter to narrow the page then acts on records it explicitly excluded. Error handling never sees the ValidationException branch either: no 400, no x-amzn-errortype header, no required `message`/`reason`, no `fieldList`, so a client's error classifier cannot distinguish a bad filter from a successful read and will not retry or surface it.

Fix: the ListDiscoverableRegistryRecords input-validation component in the agent-registry crate — after the existing filter-name check and before any record is read or any authorization/registry resolution happens, enforce exactly one value per filter and membership of each value in the filtered dimension's value set, returning the modeled ValidationException (HTTP 400, x-amzn-errortype: ValidationException, required `message` and `reason`) using the two message templates above with the name/value taken from the request.

Relation to already-open issues (none covers this wire divergence; the replay marked this cluster with an empty owned_by and an empty stub_path): 20261116T002027Z-agent-registry-3-net-new-causes-searchdiscoverableregistryrecords-never.md is a BEHAVIORAL issue whose Cause 1 describes the same missing filter-value validation but is grounded in behavioral check ids and carries NO wire replay key, so a fixer replaying it never exercises fixture agent-registry-discoverable-record-filter-validation; this issue supplies that binding. 20261107T045822Z-agent-registry-listdiscoverableregistryrecords-validationexception-enumerates-the-filter-name-enum-value.md concerns only the ORDER of the enum value set inside the filter-NAME ValidationException message (and owns its own cluster in this same replay), not filter VALUES. 20261111T095408Z-agent-registry-discovery-reads-answer-404-registry-not-found-for.md described these reads being pre-empted with 404 'Registry not found'; that pre-emption no longer reproduces (the call now returns 200 with records), so what remains is the absence of the validation itself.

Traces: fixture agent-registry-discoverable-record-filter-validation, steps task#4 (representative), task#5, task#6 — one cluster, one cause. Source: services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs lines 157-178 (the filter loop checks `filter.name` against REGISTRY_RECORD_FILTER_NAME_VALUES only; `filter.values` is never checked for count or membership).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against this checkout (emulator_up true, bootstrap_signal false; 6 fixtures replayed, 49 steps, 45 passed, 4 mismatched, score 91.8%).

Divergence cluster (verdict mismatch), divergence_key service=agent-registry|ListDiscoverableRegistryRecords|status=400->200|keys=$.message,$headers.x-amzn-errortype,status:
  fixture_id: agent-registry-discoverable-record-filter-validation
  step_key:   task#4  (representative)
  affected (3): agent-registry-discoverable-record-filter-validation#task#4, agent-registry-discoverable-record-filter-validation#task#5, agent-registry-discoverable-record-filter-validation#task#6
  expected_status 400, actual_status 200
  detail:
    status: expected 400, got 200
    $.message: missing (expected 'Filter recordType must specify exactly one value.', got None)
    $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  stub_path: "" (not blocked by an unimplemented operation), owned_by: [] (not owned by an open issue)

Expected per the recording: HTTP 400 with header x-amzn-errortype naming ValidationException and a JSON body whose required `message` is "Filter recordType must specify exactly one value." (fixed text with the filter name from the request as its only variable part) and whose required `reason` is the ValidationExceptionReason default FieldValidationFailed.
Observed: HTTP 200 with a successful `registryRecords` listing; no x-amzn-errortype header and no `message` member present in the response at all.

Model evidence (smithy_query on services/service-com-amazonaws-agentregistry-agentregistry/smithy-model/com-amazonaws-agentregistry-agentregistry.json):
  ListDiscoverableRegistryRecords: http POST /registries/{registryId}/discoverable-records-list code 200, readonly, paginated {inputToken nextToken, outputToken nextToken, pageSize maxResults, items registryRecords}; errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), UnauthorizedException(401), ValidationException(400).
  RegistryRecordFilter.name REQUIRED -> RegistryRecordFilterName enum values [recordType, descriptorType]; RegistryRecordFilter.values REQUIRED -> DiscoverableFilterValues list @length 1..10 of FilterValue @length 1..255.
  ValidationException: errorKind client, httpError 400, members message (required), reason (required, enum, default FieldValidationFailed), fieldList.

Code evidence: services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs:157-178 — `let filters = input.filters.unwrap_or_default();` then a length check against MAX_FILTERS and, per filter, only `REGISTRY_RECORD_FILTER_NAME_VALUES.contains(&filter.name.as_str())`. There is no inspection of `filter.values` anywhere before the authorization/registry-resolution and record-read path that follows (lines 180-209+).

## Steps to Reproduce
1. Clone the repo and let the wire harness build and boot the emulator from this checkout; confirm emulator_up true and bootstrap_signal false.
2. Replay fixture `agent-registry-discoverable-record-filter-validation` and watch steps `task#4`, `task#5` and `task#6` (the three members of this cluster; task#4 is the representative named in Issue-Trace/Issue-Step).
3. Equivalent manual reproduction: create a READY registry and some APPROVED discoverable records via AgentRegistryControl, then call ListDiscoverableRegistryRecords on it with filters=[{name: 'recordType', values: ['AGENT','CUSTOM']}].
   Expect: HTTP 400, header x-amzn-errortype: ValidationException, body message "Filter recordType must specify exactly one value." and reason FieldValidationFailed.
   Observe: HTTP 200 with a registryRecords list containing records of BOTH types and no error members.
4. Repeat with a value outside the filtered dimension's value set, e.g. filters=[{name: 'recordType', values: ['NOTATYPE']}]: expect HTTP 400 ValidationException with message "Invalid value for filter recordType: NOTATYPE" (fixed text "Invalid value for filter " ... ": " plus the request's filter name and value); observe a 200 listing.
5. Confirm the single-value positive filters still work after the fix (filters=[{name:'recordType',values:['MCP']}] returns only the MCP record; filters=[{name:'descriptorType',values:['a2aAgentCard']}] returns only the record with that descriptor type) — filter APPLICATION already passes; only filter-value VALIDATION is missing.
6. Re-replay all three steps of the fixture; all must pass before closing.
