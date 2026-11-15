Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023

# Page token fingerprint omits the Region the database selection is taken in

_Filed: 2026-11-15T11:55:41Z_
_Operation: GetRDSDatabaseRecommendations | Class: pagination_

> _Note: this issue was identified against the code base at commit `ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Like `DescribeRecommendationExportJobs`, this reader binds a selection fingerprint into its `nextToken` and refuses a token whose fingerprint differs, stating that a walk resumed with a different selection must be refused rather than answered from a different result set. The selection is region-scoped — `analysed_databases` walks only the current Region's range of the Amazon RDS inventory, and the ARNs reported are built for that Region — but the Region is read AFTER the fingerprint is computed and never bound into it. A token issued at one Region's endpoint is therefore accepted at another and applied as a raw offset into that Region's different database list, skipping databases or returning a short/empty page. The sibling `GetLicenseRecommendations`, region-scoped in the same way, binds `&region` into its fingerprint.

## Evidence

`src/ops/get_rds_database_recommendations.rs:241-250` — the fingerprint, with no region:

```
        // The page token carries this operation's name and the selection it was taken with,
        // so resuming a walk with a different selection, or with a token another operation
        // issued, is refused rather than answered with a page of a different result set.
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &resource_arns,
            &account_id,
            &filters,
            &input.recommendation_preferences,
        ));
```

`src/ops/get_rds_database_recommendations.rs:297-321` — the region, read later, is what selects the rows:

```
        let region = crate::state::request_region(ctx);
...
        let analysed = crate::state::analysed_databases(
            &*self.engine,
            store,
            &region,
            &account_id,
            crate::state::now_secs(),
        );
```

`src/state/rds_inventory.rs:188-195` — the selection really is one Region's:

```
    // One region's ordered range, never the whole inventory, so the work under the lock does
    // not grow with what other regions hold: Compute Optimizer is regional, and the databases
    // this endpoint analyses are its own region's.
...
        .range((region.to_string(), String::new())..)
...
        if key.0 != region {
```

The sibling that binds the region, `src/ops/get_license_recommendations.rs:235-241`: `crate::state::fingerprint(&(Self::NAME, &resource_arns, &account_id, &filters, &region))`.

Test coverage: `tests/all/rds_database_recommendations.rs:1113-1230` pins paging and token refusal within a single Region only.

## Steps to Reproduce

1. Opt in and let the analysis become due.
2. Create DB instances in the Amazon RDS emulator in `us-east-1` and a different number in `eu-west-1`, with published utilization in both.
3. Call `GetRDSDatabaseRecommendations {maxResults: 1}` signed for `us-east-1` and keep the returned `nextToken`.
4. Call `GetRDSDatabaseRecommendations {maxResults: 1, nextToken: <that token>}` signed for `eu-west-1`.
5. Observe the token is accepted and the page is an offset into the eu-west-1 database list rather than the modelled `InvalidParameterValueException` the handler's own comment promises.
