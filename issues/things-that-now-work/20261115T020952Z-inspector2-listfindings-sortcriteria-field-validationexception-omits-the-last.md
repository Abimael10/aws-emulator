Issue-Source: trace-sweep-divergence
Issue-Trace: inspector2-vscan-traj-findings-paging-and-sort-mistakes
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: inspector2-vscan-traj-findings-paging-and-sort-mistakes
Issue-Shape-Id: com.amazonaws.inspector2#Inspector2
Issue-Key: inspector2|ListFindings|4xx->4xx|$.message
Issue-Commit: ecb7c9e75f06daa929a50368dc1947c6627941cd

# inspector2 ListFindings sortCriteria.field ValidationException omits the LAST_IN_USE_AT and IN_USE_COUNT enum values

> _Note: this issue was identified against the code base at commit `ecb7c9e75f06daa929a50368dc1947c6627941cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListFindings

## Description
Cause 1 of 1: ListFindings rejecting an unmodelled sortCriteria.field value reports an enum value set that is missing two members.

AWS: Smithy (services/service-com-amazonaws-inspector2-inspector2/smithy-model/com-amazonaws-inspector2-inspector2.json): ListFindings, http POST /findings/list, input member sortCriteria -> SortCriteria, whose required member field targets the SortField enum; modeled error ValidationException (httpError 400). Recorded behavior: HTTP 400, error code ValidationException, message "1 validation error detected: Value at 'sortCriteria.field' failed to satisfy constraint: Member must satisfy enum value set: [LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, INSPECTOR_SCORE, ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, FINDING_TYPE, ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, LAST_IN_USE_AT, COMPONENT_TYPE, IN_USE_COUNT, SEVERITY, EPSS_SCORE, VULNERABILITY_SOURCE, VULNERABILITY_ID]". The fixed text of this message class is "1 validation error detected: Value at '<member path>' failed to satisfy constraint: Member must satisfy enum value set: [<values>]". Its variable parts are the member path, taken from the request shape (here the nested path 'sortCriteria.field'), and <values>, the service's SortField enum value set in AWS's emitted ordering. The rejected request value is not part of this message class at all.

Emulator: answers HTTP 400 with error code ValidationException and the correct member path and value ordering, but its value set carries only 17 members: "1 validation error detected: Value at 'sortCriteria.field' failed to satisfy constraint: Member must satisfy enum value set: [LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, INSPECTOR_SCORE, ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, FINDING_TYPE, ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, COMPONENT_TYPE, SEVERITY, EPSS_SCORE, VULNERABILITY_SOURCE, VULNERABILITY_ID]". LAST_IN_USE_AT (between FIRST_OBSERVED_AT and COMPONENT_TYPE) and IN_USE_COUNT (between COMPONENT_TYPE and SEVERITY) are absent. The checked-in Smithy SortField enum likewise declares only those 17 values, so the emulator's accepted-value set, not just the message, is behind the service: a client sorting findings by LAST_IN_USE_AT or IN_USE_COUNT would be refused rather than served.

Why: a client that sorts ListFindings by LAST_IN_USE_AT or IN_USE_COUNT works against AWS but is rejected with ValidationException by the emulator, and any harness matching AWS's ValidationException wording for a bad sortCriteria.field never matches because the advertised value set is short by two members. The message is also self-contradicting guidance: it tells the caller which values are legal, and the list it prints omits legal ones.

Fix: the inspector2 SortField enum value set (and the accepted-value check in ListFindings request validation that consumes it) must include LAST_IN_USE_AT and IN_USE_COUNT in AWS's positions; the shared constraint-violation ValidationException message builder then renders the full set. This is a DIFFERENT fault from the already-open 20261029T171819Z-inspector2-constraint-violation-validationexception-messages-add-the-rejected.md / 20261029T202306Z-inspector2-enum-validationexception-messages-interpolate-the-rejected-value.md / 20261030T014339Z-inspector2-listfindingaggregations-enum-validationexception-message-interpolates-the-rejected.md: this step does NOT interpolate the rejected value and does NOT misorder the set — it is missing enum members — and the replay report did not mark it owned by any of them.

Traces: inspector2-vscan-traj-findings-paging-and-sort-mistakes#task#3.

## Evidence
Fixture inspector2-vscan-traj-findings-paging-and-sort-mistakes, step task#3, operation ListFindings, verdict mismatch. Expected status 400, actual status 400; error code ValidationException agrees; only $.message diverges.

Expected (recorded AWS) $.message: "1 validation error detected: Value at 'sortCriteria.field' failed to satisfy constraint: Member must satisfy enum value set: [LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, INSPECTOR_SCORE, ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, FINDING_TYPE, ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, LAST_IN_USE_AT, COMPONENT_TYPE, IN_USE_COUNT, SEVERITY, EPSS_SCORE, VULNERABILITY_SOURCE, VULNERABILITY_ID]"

Actual (emulator) $.message: "1 validation error detected: Value at 'sortCriteria.field' failed to satisfy constraint: Member must satisfy enum value set: [LAST_OBSERVED_AT, FINDING_STATUS, NETWORK_PROTOCOL, INSPECTOR_SCORE, ECR_IMAGE_PUSHED_AT, RESOURCE_TYPE, ECR_IMAGE_REGISTRY, AWS_ACCOUNT_ID, FINDING_TYPE, ECR_IMAGE_REPOSITORY_NAME, VENDOR_SEVERITY, FIRST_OBSERVED_AT, COMPONENT_TYPE, SEVERITY, EPSS_SCORE, VULNERABILITY_SOURCE, VULNERABILITY_ID]"

Delta: LAST_IN_USE_AT and IN_USE_COUNT missing; every other value and the relative ordering match.

Message anatomy: fixed text = "1 validation error detected: Value at '" + "' failed to satisfy constraint: Member must satisfy enum value set: [" + "]". Values from the request/model = the member path 'sortCriteria.field' (derived from the request shape, correct today) and the SortField enum value set in AWS's emitted order (short by two members).

Smithy model: smithy_query shape=SortField on services/service-com-amazonaws-inspector2-inspector2/smithy-model/com-amazonaws-inspector2-inspector2.json returns exactly 17 values (AWS_ACCOUNT_ID, FINDING_TYPE, SEVERITY, FIRST_OBSERVED_AT, LAST_OBSERVED_AT, FINDING_STATUS, RESOURCE_TYPE, ECR_IMAGE_PUSHED_AT, ECR_IMAGE_REPOSITORY_NAME, ECR_IMAGE_REGISTRY, NETWORK_PROTOCOL, COMPONENT_TYPE, VULNERABILITY_ID, VULNERABILITY_SOURCE, INSPECTOR_SCORE, VENDOR_SEVERITY, EPSS_SCORE) — LAST_IN_USE_AT and IN_USE_COUNT are absent from the checked-in model, confirming the gap is in the enum definition rather than only the message formatter. SortCriteria.field is required and targets SortField; ListFindings errors: ValidationException (400), ThrottlingException (429), InternalServerException (500).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture inspector2-vscan-traj-findings-paging-and-sort-mistakes, step task#3: POST /findings/list (ListFindings) with a sortCriteria whose field is a value outside the SortField enum.
3. Expect HTTP 400, error code ValidationException, and $.message "1 validation error detected: Value at 'sortCriteria.field' failed to satisfy constraint: Member must satisfy enum value set: [... FIRST_OBSERVED_AT, LAST_IN_USE_AT, COMPONENT_TYPE, IN_USE_COUNT, SEVERITY ...]" — i.e. a 19-member value set. Compare $.message exactly.
4. Observed instead: the same sentence with a 17-member value set, omitting LAST_IN_USE_AT and IN_USE_COUNT.
5. Positive control for the same gap: POST /findings/list with sortCriteria={"field":"LAST_IN_USE_AT","sortOrder":"DESC"} and again with "IN_USE_COUNT". AWS sorts and returns 200; the emulator refuses with the same ValidationException.
6. Re-replay inspector2-vscan-traj-findings-paging-and-sort-mistakes#task#3 to confirm the fix.
