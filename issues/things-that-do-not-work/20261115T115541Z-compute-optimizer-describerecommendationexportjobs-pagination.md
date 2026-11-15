Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023

# Page token fingerprint omits the Region the job selection filters on, so a token resumes another Region's walk

_Filed: 2026-11-15T11:55:41Z_
_Operation: DescribeRecommendationExportJobs | Class: pagination_

> _Note: this issue was identified against the code base at commit `ee8860a66886d6dfcfd5e6a9cf8e58fb6efb9023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler binds the operation name, `jobIds` and `filters` into the page token's selection fingerprint and refuses a token whose fingerprint differs, with a comment stating that resuming a walk with a different selection must be refused rather than answered from a different result set. But the selection ALSO filters on the request's Region (`job.region == region`), and the Region is not in the fingerprint. A `nextToken` issued at one Region's endpoint therefore validates at another Region's endpoint and is applied as a raw offset into that Region's — different — job list: the second page silently skips jobs (when the other Region holds more) or returns an empty page a caller reads as "no more jobs" (when it holds fewer). The sibling reader `GetLicenseRecommendations`, whose selection is region-scoped in exactly the same way, does bind `&region` into its fingerprint.

## Evidence

The rule the handler states for its token, `src/ops/describe_recommendation_export_jobs.rs:204-208`:

```
        // The page token carries this operation's name and the selection it was taken with,
        // so resuming a walk with a different selection, or with a token another operation
        // issued, is refused rather than answered with a page of a different result set.
        let selection_fingerprint =
            crate::state::fingerprint(&(Self::NAME, &job_ids, &filters));
```

The Region that is part of the selection but not of the fingerprint, `src/ops/describe_recommendation_export_jobs.rs:237-239` and `:265-270`:

```
        // Compute Optimizer is regional: a request reports the export jobs created at the
        // Region's own endpoint, which is also where the one-job-in-progress limit applies.
        let region = crate::state::request_region(ctx);
...
        let reportable: Vec<&crate::state::ExportJob> = guard
            .export_jobs
            .iter_in_order()
            .filter(|job| job.region == region)
```

The sibling that does bind it, `src/ops/get_license_recommendations.rs:235-241`:

```
        let selection_fingerprint = crate::state::fingerprint(&(
            Self::NAME,
            &resource_arns,
            &account_id,
            &filters,
            &region,
        ));
```

Test coverage: `tests/all/recommendation_export_jobs.rs:638-681` walks pages and rejects a forged token, all within one Region, so nothing exercises a token across endpoints.

## Steps to Reproduce

1. Opt in, create the destination bucket and create several export jobs at the `us-east-1` endpoint so that `DescribeRecommendationExportJobs {maxResults: 1}` returns a `nextToken`.
2. Create a different number of export jobs at the `eu-west-1` endpoint (sign the requests for that Region).
3. Call `DescribeRecommendationExportJobs {maxResults: 1, nextToken: <token from us-east-1>}` against the `eu-west-1` endpoint.
4. Observe the token is accepted (the fingerprint matches, since only name/jobIds/filters are bound) and the answer is an offset into the eu-west-1 job list — jobs are skipped, or an empty page is returned, instead of the modelled `InvalidParameterValueException` the comment promises.
