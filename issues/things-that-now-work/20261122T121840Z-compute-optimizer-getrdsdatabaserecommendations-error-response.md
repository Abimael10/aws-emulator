Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: b2fe2d689da9f6d193550aea820e1c1e26b17eb1

# Six paginated readers re-author the invalid-nextToken refusal instead of the crate's single wording, which state.rs says exists so "no two readers refuse the same token differently"

_Filed: 2026-11-22T12:18:40Z_
_Operation: GetRDSDatabaseRecommendations | Class: error-response_

> _Note: this issue was identified against the code base at commit `b2fe2d689da9f6d193550aea820e1c1e26b17eb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state` declares ONE wording for the refusal of a `nextToken` a walk cannot resume, and its doc comment states the invariant that wording exists for: that no two readers of this service refuse the same token differently. `GetRecommendationPreferences` restates the same invariant in its own helper.

Six of this crate's paginated readers break it. Each writes its own, longer sentence inline instead of using the constant:

- `GetRDSDatabaseRecommendations` — "...: it must be a token returned by a previous GetRDSDatabaseRecommendations call for the same resourceArns, accountIds, filters and recommendationPreferences."
- `GetIdleRecommendations` — same shape, naming `resourceArns, accountIds, filters and orderBy`
- `GetLicenseRecommendations` — same shape, naming `resourceArns, accountIds and filters`
- `GetEnrollmentStatusesForOrganization` — same shape, naming `filters`
- `DescribeRecommendationExportJobs` — same shape, naming `jobIds and filters`
- `GetRecommendationSummaries` — does not use the constant either; it re-types the constant's exact literal, so the two copies can drift silently.

Meanwhile the five ARN-boundary readers (`GetEC2InstanceRecommendations`, `GetEBSVolumeRecommendations`, `GetLambdaFunctionRecommendations`, `GetAutoScalingGroupRecommendations`, `GetECSServiceRecommendations`) and `GetRecommendationPreferences` DO use the constant. So one client calling two readers of this one service with a stale/forged token gets two materially different sentences for the identical condition — which is exactly what the quoted comments say cannot happen. The six inline sentences additionally leak the request members that make up the selection fingerprint, which the short constant deliberately does not name.

The defect is the divergence, not the choice of sentence: every reader should refuse through `crate::state::INVALID_NEXT_TOKEN_MESSAGE`.

## Evidence

The rule, stated once with the invariant it exists for — services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/state.rs:1321-1324:

    /// What a reader answers when a `nextToken` is not one the walk it was handed to can resume.
    /// Worded once for the whole crate, so no two readers refuse the same token differently.
    pub const INVALID_NEXT_TOKEN_MESSAGE: &str =
        "The value supplied for parameter nextToken is not valid.";

Restated by a reader that honours it — src/ops/get_recommendation_preferences.rs:289-296:

    /// A 400 `InvalidParameterValueException` for a `nextToken` that is not one the walk it was
    /// handed to can resume, worded as the whole crate words it
    /// ([`crate::state::INVALID_NEXT_TOKEN_MESSAGE`]) so no two readers refuse the same token
    /// differently.
    fn invalid_next_token() -> GetRecommendationPreferencesError {
        ...
                message: Some(crate::state::INVALID_NEXT_TOKEN_MESSAGE.to_string()),

The readers that break it:

src/ops/get_rds_database_recommendations.rs:280-285:
                    return Err(invalid_parameter(
                        "The value supplied for parameter nextToken is not valid: it must be a \
                         token returned by a previous GetRDSDatabaseRecommendations call for the \
                         same resourceArns, accountIds, filters and recommendationPreferences."
                            .to_string(),
                    ));

src/ops/get_idle_recommendations.rs:264-269:
                    return Err(invalid_parameter(
                        "The value supplied for parameter nextToken is not valid: it must be a \
                         token returned by a previous GetIdleRecommendations call for the same \
                         resourceArns, accountIds, filters and orderBy."
                            .to_string(),
                    ));

src/ops/get_license_recommendations.rs:260-265:
                    return Err(invalid_parameter(
                        "The value supplied for parameter nextToken is not valid: it must be a \
                         token returned by a previous GetLicenseRecommendations call for the same \
                         resourceArns, accountIds and filters."
                            .to_string(),
                    ));

src/ops/get_enrollment_statuses_for_organization.rs:206-211:
                    return Err(invalid_parameter(
                        "The value supplied for parameter nextToken is not valid: it must be a \
                         token returned by a previous GetEnrollmentStatusesForOrganization call \
                         for the same filters."
                            .to_string(),
                    ));

src/ops/describe_recommendation_export_jobs.rs:224-229:
                    return Err(invalid_parameter(
                        "The value supplied for parameter nextToken is not valid: it must be a \
                         token returned by a previous DescribeRecommendationExportJobs call for \
                         the same jobIds and filters."
                            .to_string(),
                    ));

src/ops/get_recommendation_summaries.rs:250-252 — the constant's literal re-typed instead of referenced:
                        return Err(invalid_parameter(
                            "The value supplied for parameter nextToken is not valid.".to_string(),
                        ))

Test coverage: tests/all/ec2_instance_recommendations.rs:1010 pins the constant's wording for `GetEC2InstanceRecommendations` (`"The value supplied for parameter nextToken is not valid.",`). A grep of `tests/` for `token returned by a previous` returns no match, so no test pins any of the six inline sentences — routing them through `crate::state::INVALID_NEXT_TOKEN_MESSAGE` leaves every existing test passing.

## Steps to Reproduce

1. Boot the emulator and opt the account in with UpdateEnrollmentStatus {status: Active, includeMemberAccounts: false}.
2. Call GetEC2InstanceRecommendations with {"nextToken": "not-a-token"} — observe 400 InvalidParameterValueException with message "The value supplied for parameter nextToken is not valid." (the crate's single wording).
3. Call GetRDSDatabaseRecommendations with {"nextToken": "not-a-token"} — observe 400 InvalidParameterValueException whose message is instead "The value supplied for parameter nextToken is not valid: it must be a token returned by a previous GetRDSDatabaseRecommendations call for the same resourceArns, accountIds, filters and recommendationPreferences."
4. Repeat step 3 against GetIdleRecommendations, GetLicenseRecommendations, GetEnrollmentStatusesForOrganization and DescribeRecommendationExportJobs — each answers its own distinct sentence for the identical condition, contradicting state.rs:1322 ("Worded once for the whole crate, so no two readers refuse the same token differently").
