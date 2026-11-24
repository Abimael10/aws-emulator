Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9b16df0f9437cb7a382af96a3692aa1aca7a1f7d

# A managed (PRIMARY) billing view is served HTTP 200 with an empty required sourceViews list, outside the modelled @length(min: 1) the crate itself cites

_Filed: 2026-11-24T14:29:53Z_
_Operation: ListSourceViewsForBillingView | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9b16df0f9437cb7a382af96a3692aa1aca7a1f7d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListSourceViewsForBillingView` resolves its `arn` against the whole view store and reports whatever source list the record holds, with no restriction to the `CUSTOM` views that have one. The account's default `PRIMARY` view is seeded with `source_views: Vec::new()`, so a request naming it (or any other managed `BillingViewType`) is answered HTTP 200 with `sourceViews: []`.

That response is outside the model: `ListSourceViewsForBillingViewResponse$sourceViews` is `@required` and targets `BillingViewSourceViewsList`, whose `@length` is `min: 1, max: 10` — the very trait this handler's own doc comment cites when it explains its page size. An empty list is not a value the member admits, so a client that validates the response against the shape (or an SDK that enforces output constraints) is handed a body the service's own model forbids, and the member's doc describes the list as the sources "for the custom billing view", not for a view that has none.

The crate already draws exactly this line elsewhere, and already owns the helper for it. `state::custom_view` exists precisely to collapse "no view stored" and "not a CUSTOM view" into one refusal; `DisassociateSourceViews` uses `custom_view_mut` and refuses a managed target 400 with "Only custom billing views can have source views disassociated"; `AssociateSourceViews` refuses the same target 400 with "Managed billing views cannot be modified"; and the sibling read `GetBillingView` deliberately reports NO `sourceViewCount` at all for a managed view, on the stated ground that such a view "is not a resource a caller created, and this surface reports none of the lifecycle metadata that belongs to a caller-created one". So this crate says in three places that a managed view has no customer-owned source list — and then this one read answers with an empty one instead of either refusing the ARN as the writers do or declining to report a list it cannot fill legally.

The existing test `a_view_holding_no_source_views_answers_an_empty_list` pins the empty 200, but its stated reason is only about page sizes agreeing ("every page size must agree that there are none"); it says nothing about the `@length(min: 1)` the quoted model text states, which is what the answer breaks.

## Evidence

The model forbids an empty list on a required member — `smithy_query(query="shape", shape="BillingViewSourceViewsList,ListSourceViewsForBillingViewResponse")`:
```
"BillingViewSourceViewsList": {"type": "list", "length": {"min": 1, "max": 10}, "member": {"target": "BillingViewArn", ...}}
"ListSourceViewsForBillingViewResponse": {"members": {"sourceViews": {"target": "BillingViewSourceViewsList", "required": true, "length": {"min": 1, "max": 10}, "type": "list", "doc": "<p>A list of billing views used as the data source for the custom billing view. </p>"}, ...}}
```

The handler resolves the ARN with no CUSTOM/managed gate — services/service-com-amazonaws-billing-awsbilling/src/ops/list_source_views_for_billing_view.rs:390-397:
```
        let view = state.billing_views.get(&input.arn).ok_or_else(|| {
            // A read that resolves one `BillingViewArn` reports the ARN it could
            // not resolve, in this service's one form for that case
            ListSourceViewsForBillingViewErrorModeled::ResourceNotFoundException(
                crate::state::unresolved_billing_view_arn(&input.arn),
            )
        })?;
```
...and emits whatever that record holds — list_source_views_for_billing_view.rs:441-447:
```
        Ok(ListSourceViewsForBillingViewOutput {
            // Derived on every read from the one stored copy of the view, so a
            // list cannot disagree with what `GetBillingView` reports as
            // `sourceViewCount`.
            source_views: page.iter().map(|arn| (*arn).to_string()).collect(),
            next_token,
        })
```

The same file cites the very `@length` it then breaks — list_source_views_for_billing_view.rs:160-167:
```
/// This `maxResults` doc ("The number of entries a paginated response
/// contains.") names no default, so an omitted page size is the largest page
/// this read serves at all — [`MAX_MAX_RESULTS`] — rather than every entry the
/// view holds. A view's source views are capped well below it
/// ([`crate::state::MAX_SOURCE_VIEWS_PER_VIEW`], the modelled `@length` of
/// `BillingViewSourceViewsList`), so this bound is what keeps the omitted case
/// a page rather than the page size a caller sees.
```

The record that makes the empty answer reachable over the wire — services/service-com-amazonaws-billing-awsbilling/src/state.rs:805-811:
```
        let primary = BillingView {
            arn: billing_view_arn(PRIMARY_VIEW_ID),
            name: PRIMARY_VIEW_NAME.to_string(),
            description: Some(primary_view_description(&owner_account_id)),
            billing_view_type: BILLING_VIEW_TYPE_PRIMARY.to_string(),
            owner_account_id,
            source_views: Vec::new(),
```

The gate the crate already owns, and the siblings that apply it — state.rs:1255-1265:
```
    /// `None` covers both inputs AWS collapses into one refusal — an ARN no
    /// view is stored under, and one naming a view that is not `CUSTOM` (the
    /// account's `PRIMARY` view) — so eligibility is decided without revealing
    /// which of the two it was.
    pub fn custom_view(&self, arn: &str) -> Option<&BillingView> {
        self.billing_views
            .get(arn)
            .filter(|view| view.billing_view_type == BILLING_VIEW_TYPE_CUSTOM)
    }
```
src/ops/disassociate_source_views.rs:169-172 and 237-239:
```
/// The message this operation answers when the view the request named is not a
/// `CUSTOM` billing view: fixed text for this refusal.
const NOT_A_CUSTOM_VIEW_MESSAGE: &str =
    "Only custom billing views can have source views disassociated";
...
        let Some(view) = state.custom_view_mut(&input.arn) else {
            return Err(not_a_custom_view());
        };
```
src/ops/associate_source_views.rs:265-272:
```
        // A managed view's definition — which views it aggregates — is the
        // service's to maintain, not the customer's: see `BillingView::is_managed`.
        if target_is_managed {
            return Err(invalid_field(
                "arn",
                "Managed billing views cannot be modified",
            ));
        }
```

The sibling READ that refuses to report source metadata for a managed view at all — src/ops/get_billing_view.rs:200-210:
```
        // A view the service manages on the account's behalf (its default
        // PRIMARY view — see `BillingView::is_managed`) is not a resource a
        // caller created, and this surface reports none of the lifecycle
        // metadata that belongs to a caller-created one: no creation time and
        // neither count. So a client can tell the two apart by whether the
        // record carries that metadata at all.
        let caller_created = !view.is_managed();
        let source_view_count = caller_created
            .then(|| i32::try_from(view.source_views.len()).unwrap_or(i32::MAX));
```

Test coverage: the empty 200 is pinned, but only for "every page size must agree" — tests/all/source_views.rs:238-263:
```
async fn a_view_holding_no_source_views_answers_an_empty_list() {
    let router = emulator();

    // The account's PRIMARY view draws on no other view, so it holds no source
    // views; every page size must agree that there are none.
    for request in [
        json!({ "arn": PRIMARY_ARN }),
        json!({ "arn": PRIMARY_ARN, "maxResults": 1 }),
        json!({ "arn": PRIMARY_ARN, "maxResults": 100 }),
    ] {
        ...
        assert!(
            source_views(&listed).is_empty(),
```
and tests/all/source_views_tags_and_policy.rs:386-390 reads the same empty answer back after a refused associate:
```
    let primary_sources = list_source_views(&router, PRIMARY_ARN, None).await;
    assert!(
        primary_sources.is_empty(),
        "case \"managed target\": the managed view must keep drawing from nothing, got {primary_sources:?}"
    );
```
Neither assertion mentions the `@length(min: 1)` the quoted model text states for the member.

## Steps to Reproduce

1. Read `smithy_query(query="shape", shape="BillingViewSourceViewsList")`: `"length": {"min": 1, "max": 10}`, and `smithy_query(query="shape", shape="ListSourceViewsForBillingViewResponse")`: `sourceViews` is `"required": true` and targets that list — an empty list is not a value the member admits.
2. Read services/service-com-amazonaws-billing-awsbilling/src/state.rs:805-811 — the account's default `PRIMARY` view is seeded with `source_views: Vec::new()`.
3. Read src/ops/list_source_views_for_billing_view.rs:390-397 — the handler resolves the `arn` against `state.billing_views` with no `custom_view`/`is_managed` gate, unlike `DisassociateSourceViews` (disassociate_source_views.rs:237-239) and `AssociateSourceViews` (associate_source_views.rs:265-272).
4. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/billing_views.rs), then call ListSourceViewsForBillingView(arn="arn:aws:billing::000000000000:billingview/primary").
5. Observe HTTP 200 with `sourceViews: []` — a required member served outside its modelled `@length(min: 1)` — instead of either the 400 ValidationException the two sibling writers answer for a managed view, or the managed-view treatment GetBillingView states at get_billing_view.rs:200-210 (no source metadata reported at all).
