Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 8f01260b6810a633689b77ca6b278b248e498535

# Collapse the four prose copies of the delivery-status rule: one already states the opposite rule

_Filed: 2026-11-25T03:33:28Z_
_Operation: DescribeReportDefinitions | Class: maintainability_

> _Note: this issue was identified against the code base at commit `8f01260b6810a633689b77ca6b278b248e498535`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The read path's delivery-status rule — `ReportStatus` is stated UNCONDITIONALLY for every report definition a read serves, with both keys empty for a report no delivery has been recorded against — is written out in prose in four places in this crate (the state helper, and three test-suite docs). One of those copies has already DRIFTED and now states the opposite rule: `tests/all/service_populated_read_back.rs` says the delivery status is "left out while nothing has delivered the report", which is neither what `StoredReport::delivery_status` computes nor what that same file's own test asserts two dozen lines below it. A reader who takes the drifted copy as the rule would make the read path omit `ReportStatus` for a never-delivered report, which is the behaviour three other copies of the rule — and two test suites — forbid. The fix is to state the rule once (or correct the drifted copy to match the helper that implements it); no response and no existing test changes.

## Evidence

Drifted copy — services/.../tests/all/service_populated_read_back.rs:11-13: "the status of the report's last delivery\n//!    (stated for a report a delivery has been recorded against, and left out while\n//!    nothing has delivered the report)".

The implementation it describes — services/.../src/state.rs:386-391: "The dictionary itself is UNCONDITIONAL on this read path — the recorded service / states both keys on every report definition it serves", and src/state.rs:401-411: `match self.last_delivery.as_ref() { ... None => ReportStatus { last_delivery: Some(NO_DELIVERY.to_string()), last_status: Some(NO_DELIVERY.to_string()) } }` (so `wire_definition`, src/state.rs:375, always fills `report_status: Some(self.delivery_status())`).

The same file's own test contradicts its module doc — tests/all/service_populated_read_back.rs:108-111: "beside the report nothing has / delivered, whose read-back states the same dictionary with both keys EMPTY — the / dictionary is unconditional on this read path" and :113-117 `vec![("no delivery recorded".to_string(), None, Some(never_delivered()))]`, asserted at :144-148 with `assert_eq!(first.get("ReportStatus").cloned(), expected, ...)`; `never_delivered()` is `json!({ "lastDelivery": "", "lastStatus": "" })` (tests/all/report_definition_lifecycle.rs:223-225).

The other two copies of the rule — tests/all/delivery_status_on_every_read.rs:5-12 ("the dictionary is unconditional and only its values / report the delivery") and tests/all/reported_definition_round_trip.rs:12-14 ("a report none has been / recorded against carries both keys EMPTY").

Tests that cover the behaviour and would stay unchanged: `a_read_back_carries_the_status_of_the_report_last_delivery` (service_populated_read_back.rs:105), `every_definition_a_read_serves_states_the_delivery_recorded_against_it` and `a_recorded_delivery_is_what_the_status_values_come_from` (delivery_status_on_every_read.rs:64, :278).

## Steps to Reproduce

1. Read services/service-com-amazonaws-costandusagereportservice-awsorigamiservicegatewayservice/tests/all/service_populated_read_back.rs lines 8-15: rule 1 states the delivery status is "left out while nothing has delivered the report".
2. Read src/state.rs lines 386-412: `delivery_status` returns the two-key dictionary with NO_DELIVERY (the empty string) in both keys when `last_delivery` is `None`, and `wire_definition` (line 375) always sets `report_status: Some(...)` — the member is never left out.
3. Read the same test file's lines 108-148: the test's own comment and assertion require `ReportStatus` to be PRESENT as `{"lastDelivery": "", "lastStatus": ""}` for a never-delivered report — the opposite of its module doc.
4. Note the rule is restated in two further suites (delivery_status_on_every_read.rs:5-12, reported_definition_round_trip.rs:12-14), so four prose copies exist of one rule and one has already drifted.
