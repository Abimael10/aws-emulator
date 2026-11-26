Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 4ed63421c2a299f1b98968b719f9a71bb0543024

# No path ever records a report delivery, so every report's ReportStatus.lastStatus stays the empty string — contradicting the delivery-grant module's own deferral to it

_Filed: 2026-11-26T09:13:27Z_
_Operation: PutReportDefinition | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `4ed63421c2a299f1b98968b719f9a71bb0543024`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutReportDefinition` inserts every report with `last_delivery: None`, and NO code path in this crate (handler, state helper, or cross-service hook) ever assigns `StoredReport::last_delivery`. Only the test suites construct a `ReportDelivery`. The consequence is that `StoredReport::delivery_status()` takes its `None` branch for every report the API can create, forever, so every `DescribeReportDefinitions` read reports `ReportStatus = {lastDelivery: "", lastStatus: ""}` and the modelled `LastStatus` values `ERROR_NO_BUCKET` and `ERROR_PERMISSIONS` can never appear on the wire.

That contradicts this crate's own delivery-grant module, which explicitly DEFERS the missing-destination-bucket case to that same delivery status instead of refusing the call: it declines to judge whether the named bucket exists on the stated grounds that the later delivery reports the problem through the report's delivery status. Because nothing in the crate ever records a delivery, the deferral is empty — a definition pointed at a bucket this emulator's object store does not hold is accepted and then reports "never delivered" indefinitely, never the failure the comment promises. The machinery the comment defers to already exists in the crate (`ReportDelivery`, `StoredReport::last_delivery`, `delivery_status()`), so this is a path that skips a helper the crate already has, not absent functionality.

## Evidence

src/state/delivery_grant.rs:30-32 (the deferral): "What this module does NOT judge is whether the named bucket is THERE: the bucket lives in Amazon S3, not in this service's state, and a refusal here has to be grounded in a policy that was actually read — so a bucket this emulator's object store does not hold is left to the delivery that would later find it missing, which this service reports as the delivery status of the report."

src/state.rs:319-325 (the only production construction of a stored report, which hard-codes no delivery):
```
    pub fn new(definition: ReportDefinition, tags: &[Tag]) -> Result<Self, TagLimitExceeded> {
        let mut stored = Self {
            definition,
            tags: BTreeMap::new(),
            created_at: SystemTime::now(),
            last_delivery: None,
        };
```
src/state.rs:401-412: `delivery_status()` answers `ReportStatus { last_delivery: Some(NO_DELIVERY), last_status: Some(NO_DELIVERY) }` whenever `last_delivery` is `None`, and src/state.rs:294 defines `pub const NO_DELIVERY: &str = ""`.

No production writer exists: grep for `last_delivery|ReportDelivery` across the crate matches only `src/state.rs` (the declaration, the `None` initialiser and the read in `delivery_status`), `src/ops/types/shared_types.rs:48` (the wire member) and the test files `tests/all/report_definition_lifecycle.rs:26,240,293,317-324`, `tests/all/delivery_status_on_every_read.rs:25,37-56`, `tests/all/service_populated_read_back.rs:39,113-121`, `tests/all/reported_definition_round_trip.rs:51,85,107` — every one of which SEEDS the delivery directly into the store (`cur.seed(...)` / `stored_report_at(name, at, last_delivery)`) rather than reaching it through an operation.

src/ops/types/shared_types.rs:78: `pub const LAST_STATUS_VALUES: &[&str] = &["ERROR_NO_BUCKET", "ERROR_PERMISSIONS", "SUCCESS"];` — none of the three is reachable through any API call.

Test coverage: `tests/all/delivery_status_on_every_read.rs` pins the read-back projection of a delivery, but only for stores seeded in-process (lines 27-50 import `stored_report_at` / `recorded_deliveries` and build `StoredReport`s directly), so no test exercises a delivery produced by the service itself.

## Steps to Reproduce

1. Start the emulator with both this service and Amazon S3 mounted on the shared engine.
2. PutReportDefinition with a ReportDefinition whose S3Bucket names a bucket that does not exist in the object store (the delivery-grant check returns Ok for it — src/state/delivery_grant.rs:116-121 `None => return Ok(())`), so the definition is accepted with HTTP 200.
3. Call DescribeReportDefinitions and read the definition's ReportStatus: it is `{"lastDelivery": "", "lastStatus": ""}`.
4. Repeat the read any number of times, at any later instant: the status never changes, so the missing destination bucket that src/state/delivery_grant.rs:30-32 says "this service reports as the delivery status of the report" is never reported at all (ERROR_NO_BUCKET is unreachable).
