Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: b182a47fe4222897ea5ff61fa3dd31f69a735d61

# Nothing in src/ ever moves a billing view out of HEALTHY, so every BillingViewHealthStatusException gate is unreachable and healthStatus is a constant

_Filed: 2026-11-23T18:56:21Z_
_Operation: AssociateSourceViews | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `b182a47fe4222897ea5ff61fa3dd31f69a735d61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate carries a complete health-status plane for billing views: a stored `health_status_code`/`health_status_reasons` pair, a shared refusal builder (`state::billing_view_not_healthy`), four handler gates that consult it (AssociateSourceViews, DisassociateSourceViews, UpdateBillingView, ListBillingViewSegments) and a shared projection (`state::health_status`) that both reads report. But no code path under `src/` ever assigns `health_status_code` anything other than `HEALTHY`, and nothing ever pushes a `health_status_reasons` entry: the seeded PRIMARY view is built with `HEALTHY` and an empty reason list, `CreateBillingView` mints every CUSTOM view the same way, and no other handler or state helper touches either field.

Two consequences, both unreachable over the wire:
1. The modelled `BillingViewHealthStatusException` — declared by five operations of this service — can never be answered to any client, whatever sequence of API calls it makes. All four gates are dead branches.
2. `BillingViewHealthStatus` is a constant on the wire: `GetBillingView` and `ListBillingViews` always report `statusCode: "HEALTHY"` with `statusReasons: []`, so the `UNHEALTHY`/`CREATING`/`UPDATING` values of the modelled `BillingViewStatus` enum and every value of `BillingViewStatusReason` are unobservable. A view that is created, re-pointed at a source view that is later force-deleted, or updated, reports exactly the same health as a freshly seeded PRIMARY view.

This is the same unreachable-gate shape already filed for this crate's resource-policy, feature-access, credit-allocation and Support-spend stores: a modelled surface with no production writer behind it. The crate's own test helpers say so in as many words.

## Evidence

The model declares four statuses and an error whose documented trigger is "a state other than HEALTHY" — smithy_query(query="shape", shape="BillingViewStatus,BillingViewHealthStatusException"):
```
"BillingViewStatus": {"type": "enum", "values": ["HEALTHY", "UNHEALTHY", "CREATING", "UPDATING"]}
"BillingViewHealthStatusException": {"errorKind": "client", "httpError": 400, "doc": "Exception thrown when a billing view's health status prevents an operation from being performed. This may occur if the billing view is in a state other than HEALTHY."}
```

The only two assignments in src/ both write HEALTHY — services/service-com-amazonaws-billing-awsbilling/src/state.rs:811-812 (the seeded PRIMARY view):
```
            health_status_code: BILLING_VIEW_STATUS_HEALTHY.to_string(),
            health_status_reasons: Vec::new(),
```
and src/ops/create_billing_view.rs:348-351 (the only creator):
```
            health_status_code: crate::state::BILLING_VIEW_STATUS_HEALTHY.to_string(),
            // A view that is HEALTHY has nothing to explain, so it carries the
            // empty reason list: see `BillingView::health_status_reasons`.
            health_status_reasons: Vec::new(),
```
`grep -n 'health_status_code|health_status_reasons' src/` finds only: state.rs:452/460 (the field declarations), state.rs:508 (`is_healthy`), state.rs:811-812, state.rs:1891-1892 (the projection), create_billing_view.rs:348/351, and the four gates' reads (associate_source_views.rs:227, disassociate_source_views.rs:252, update_billing_view.rs:218, list_billing_view_segments.rs:435). No writer of a non-HEALTHY value exists.

The gate this issue is filed against — src/ops/associate_source_views.rs:270-283:
```
        // `BillingViewHealthStatusException`: "a billing view's health status
        // prevents an operation from being performed … if the billing view is in
        // a state other than HEALTHY". The refusal is the one every writer of a
        // stored view shares — see `crate::state::billing_view_not_healthy`.
        if target_health != crate::state::BILLING_VIEW_STATUS_HEALTHY {
            return Err(AssociateSourceViewsErrorModeled::BillingViewHealthStatusException(
                crate::state::billing_view_not_healthy(
                    &input.arn,
                    &target_health,
                    "associate source views",
                ),
            )
            .into());
        }
```
the same gate in src/ops/disassociate_source_views.rs:247-258, src/ops/update_billing_view.rs:213-224 and src/ops/list_billing_view_segments.rs:430-441, and the shared builder they all use — src/state.rs:1701-1712:
```
pub fn billing_view_not_healthy(
    arn: &str,
    status: &str,
    action: &str,
) -> BillingViewHealthStatusException {
```

The projection that therefore always answers HEALTHY/[] — src/state.rs:1889-1894:
```
pub fn health_status(view: &BillingView) -> BillingViewHealthStatus {
    BillingViewHealthStatus {
        status_code: Some(view.health_status_code.clone()),
        status_reasons: Some(view.health_status_reasons.clone()),
    }
}
```

The crate admits the gap in its own test helpers — tests/all/source_view_disassociation.rs:128-142:
```
/// Put the stored view's `healthStatus.statusCode` at `status`, the one way a
/// view's health moves in this emulator: no modelled operation sets it.
pub(crate) fn set_health_status(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    arn: &str,
    status: &str,
) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    let view = state
        .billing_views
        .get_mut(arn)
        .expect("the harness created this view");
    view.health_status_code = status.to_string();
}
```
and tests/all/billing_view_wire_fidelity.rs:76-87:
```
/// Put the stored view's health status at `code` with `reasons`, the one way a
/// view's health moves in this emulator: no modelled operation sets it.
fn set_health(engine: &Arc<engine::InMemoryEngine>, arn: &str, code: &str, reasons: &[String]) {
```
Test coverage today: tests/all/billing_view_health_gate.rs, source_view_disassociation.rs and billing_view_wire_fidelity.rs all exercise the gates and the projection, but only by writing `health_status_code` directly into the engine's state — no test reaches a non-HEALTHY view through the API, because no API request can produce one.

## Steps to Reproduce

1. Boot the emulator with the harness in tests/all/billing_views.rs::emulator_with_engine (which records the caller as an organization management account so CreateBillingView is reachable).
2. CreateBillingView(name="v1", sourceViews=["arn:aws:billing::000000000000:billingview/primary"]) -> 200 with an ARN.
3. GetBillingView(arn=<that ARN>) -> `billingView.healthStatus` = {"statusCode":"HEALTHY","statusReasons":[]}.
4. Exercise every state-changing operation the service models against that view in any order — UpdateBillingView, AssociateSourceViews, DisassociateSourceViews, TagResource/UntagResource, and DeleteBillingView(force=true) of a view it draws from — then call GetBillingView and ListBillingViews again: `healthStatus` is still {"statusCode":"HEALTHY","statusReasons":[]} every time.
5. Conclude that no request can make any of the four `if !view.is_healthy()` gates fire, so the modelled BillingViewHealthStatusException (declared by AssociateSourceViews, CreateBillingView, DisassociateSourceViews, UpdateBillingView and ListBillingViewSegments) is unanswerable over the wire.
6. Confirm by grep: the only assignments of `health_status_code` outside tests are `BILLING_VIEW_STATUS_HEALTHY` at src/state.rs:811 and src/ops/create_billing_view.rs:348, and the only writers of a different value are the two test helpers that state "no modelled operation sets it".
