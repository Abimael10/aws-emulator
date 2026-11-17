Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: e974643efe5559269bccc30718b5c19c98ab99de

# The read-back fills ReportStatus.lastDelivery/lastStatus with values outside their modelled constraints, so a describe -> modify round-trip of any report is refused 400

_Filed: 2026-11-17T04:18:20Z_
_Operation: DescribeReportDefinitions | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e974643efe5559269bccc30718b5c19c98ab99de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every report definition this service serves is projected through `StoredReport::wire_definition`, which unconditionally fills `ReportStatus` from `StoredReport::delivery_status`. Nothing in this service's modelled surface records a delivery, so for EVERY report the emulator holds, both keys come back as the empty string.

The empty string is outside the constraints the model declares on those two members: `ReportStatus$lastDelivery` is `length 16..20` with pattern `^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$`, and `ReportStatus$lastStatus` is the `LastStatus` enum whose only values are SUCCESS, ERROR_PERMISSIONS, ERROR_NO_BUCKET. `ReportStatus` is a member of `ReportDefinition`, the SAME shape `ModifyReportDefinition` and `PutReportDefinition` take as input, and this crate's own boundary validator enforces those constraints on the nested path `reportDefinition.reportStatus.lastDelivery` (pinned by its own test, quoted below).

The consequence is a value this service emits that it will not accept back: the ordinary read-modify-write flow (DescribeReportDefinitions -> change one member -> ModifyReportDefinition with the definition that came back) is refused with 400 ValidationException carrying three clauses (lastDelivery too short, lastDelivery pattern, lastStatus off-catalog), for every report in the store. `state.rs` acknowledges the two sides of this in one doc comment — it calls the empty string "an output-only value" while stating in the same sentence that "the boundary validator holds every request to them" — but nothing reconciles the read path with the validator, and no test exercises a read-back being sent back in.

## Evidence

Model (smithy_query(shape="ReportStatus")): `"lastDelivery": {"target": "LastDelivery", "length": {"min": 16, "max": 20}, "pattern": "^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$"}`, `"lastStatus": {"target": "LastStatus", "type": "enum"}`; smithy_query(shape="LastStatus") -> `"values": ["SUCCESS", "ERROR_PERMISSIONS", "ERROR_NO_BUCKET"]`.

What the read path emits — services/.../src/state.rs:275: `pub const NO_DELIVERY: &str = "";` and services/.../src/state.rs:373-377:
```
            None => ReportStatus {
                last_delivery: Some(NO_DELIVERY.to_string()),
                last_status: Some(NO_DELIVERY.to_string()),
            },
```
reached unconditionally from services/.../src/state.rs:349 (`report_status: Some(self.delivery_status()),` inside `wire_definition`), which services/.../src/ops/describe_report_definitions.rs:193-196 maps over every element of every page:
```
        let page: Vec<ReportDefinition> = store.reports[start..end]
            .iter()
            .map(crate::state::StoredReport::wire_definition)
```

The crate's own admission that those constraints are enforced on requests — services/.../src/state.rs:270-274: "It is an output-only value: the modelled constraints on the two members — `lastDelivery` length 16..20 with the pattern `^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$`, `lastStatus` the `LastStatus` catalog ... — describe the values a DELIVERY has, and the boundary validator holds every request to them, as it does for every other constrained member."

The crate's own test proving the nested member IS validated on the way in — services/.../tests/all/refusal_message_text.rs:156-168:
```
            case: "a nested member: the last delivery is an ISO-8601 instant",
            operation: "PutReportDefinition",
            request: put_body(definition_with(
                "ReportStatus",
                json!({ "lastDelivery": "2024-01-01T00:00:00Z" }),
            )),
            clauses: vec![clause(
                "2024-01-01T00:00:00Z",
                "reportDefinition.reportStatus.lastDelivery",
                &pattern_requirement(last_delivery),
            )],
```

The value is pinned as the read-back for every API-created report — services/.../tests/all/report_definition_lifecycle.rs:217,227-229: `object.insert("ReportStatus".to_string(), never_delivered());` with `pub(crate) fn never_delivered() -> Value { json!({ "lastDelivery": "", "lastStatus": "" }) }`. No test in `tests/all/` sends a read-back definition back to `ModifyReportDefinition`/`PutReportDefinition`, so the round-trip is uncovered.

## Steps to Reproduce

1. POST / with X-Amz-Target: AWSOrigamiServiceGatewayService.PutReportDefinition and a minimal valid ReportDefinition named "round-trip-report" -> 200.
2. POST / with X-Amz-Target: AWSOrigamiServiceGatewayService.DescribeReportDefinitions, body {} -> 200; the single element carries "ReportStatus": {"lastDelivery": "", "lastStatus": ""} (services/.../src/state.rs:373-377).
3. Take that element verbatim, change one member (e.g. "TimeUnit": "MONTHLY"), and POST it as ModifyReportDefinition with {"ReportName": "round-trip-report", "ReportDefinition": <the element>}.
4. Observe 400 ValidationException instead of 200: the boundary validator refuses reportDefinition.reportStatus.lastDelivery for the length 16..20 bound and the ^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$ pattern, and reportDefinition.reportStatus.lastStatus for the LastStatus enum catalog — i.e. the service will not accept the definition it just served.
