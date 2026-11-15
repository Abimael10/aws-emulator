Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 44bbf79358888366d10de6b934e7daf7fcf849e8

# GetRDSDatabaseRecommendationProjectedMetrics answers 200 for a resourceArn of another account instead of the crate's cross-account refusal

_Filed: 2026-11-15T21:39:08Z_
_Operation: GetRDSDatabaseRecommendationProjectedMetrics | Class: validation_

> _Note: this issue was identified against the code base at commit `44bbf79358888366d10de6b934e7daf7fcf849e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states ONE rule for an ARN whose account field names an account other than the one the emulator answers for: the reader refuses it with CROSS_ACCOUNT_MESSAGE. Both sibling projected-metrics readers implement it on their single ARN member - GetEC2RecommendationProjectedMetrics$instanceArn and GetECSServiceRecommendationProjectedMetrics$serviceArn both call crate::state::is_foreign_account(..) right after parsing the ARN and return the 400 InvalidParameterValueException. GetRDSDatabaseRecommendationProjectedMetrics parses resourceArn with crate::state::parse_arn and never calls is_foreign_account, so a well-formed ARN naming another account's database falls through to the lookup, matches nothing (the lookup is built against ::engine::emulator_account_id() at line 293 and compares resource.arn_in(&region)), and the request is answered HTTP 200 with recommendedOptionProjectedMetrics: [] - i.e. 'that account's database has no projection', which is the answer the shared rule says must never be produced. The identical request shape against the EC2 and ECS projected-metrics readers is a 400. One of three readers of the same kind skips a check the other two apply, so a client cannot tell from this surface whether a cross-account ARN was rejected or simply has no data.

## Evidence

The crate's rule, services/service-com-amazonaws-computeoptimizer-computeoptimizerservice/src/state.rs:955-965:
```
/// Whether `account_id` - an id a request named, or the account field of an ARN it carried
/// - names an account OTHER than the one this service answers for.
///
/// This emulator presents exactly one standalone account ([`engine::emulator_account_id`])
/// and records no organization, so a resource owned by any other account is one no request
/// can be answered about: a reader refuses it with [`CROSS_ACCOUNT_MESSAGE`] rather than
/// resolving an answer under that other account. ONE test for the whole crate, so an
/// account id and the account field of an ARN are judged by the same rule.
pub fn is_foreign_account(account_id: &str) -> bool {
    account_id != ::engine::emulator_account_id()
}
```

Sibling that applies it - src/ops/get_ec2_recommendation_projected_metrics.rs:220-237:
```
        // ... An ARN whose ACCOUNT field names
        // another account asks about an instance this service cannot report on, so it is
        // refused with the crate's cross-account message rather than selecting nothing.
        ...
        if crate::state::is_foreign_account(parsed.account_id) {
            return Err(invalid_parameter(
                crate::state::CROSS_ACCOUNT_MESSAGE.to_string(),
            ));
        }
```

Sibling that applies it - src/ops/get_ecs_service_recommendation_projected_metrics.rs:246-250:
```
        if crate::state::is_foreign_account(&reference.account_id) {
            return Err(invalid_parameter(
                crate::state::CROSS_ACCOUNT_MESSAGE.to_string(),
            ));
        }
```

The handler that skips it - src/ops/get_rds_database_recommendation_projected_metrics.rs:214-224 (there is no account check anywhere in `handle`; the only ARN test is the form test):
```
        // `resourceArn`: "The ARN that identifies the Amazon Aurora or RDS database" - so the
        // value is an ARN. WHICH resource it names is not checked here: a well-formed ARN
        // this service holds no database analysis for is reported as a missing resource
        // below, not refused.
        if crate::state::parse_arn(&input.resource_arn).is_none() {
            return Err(invalid_parameter(format!(
                "The value supplied for parameter resourceArn is not valid: {} is not an \
                 Amazon Resource Name (ARN).",
                input.resource_arn
            )));
        }
```
and the lookup it then falls into, same file lines 289-308, which can only ever match the emulator's own account:
```
        let analysed = crate::state::analysed_databases(
            &*self.engine,
            &guard,
            &region,
            ::engine::emulator_account_id(),
            crate::state::now_secs(),
        );
        let Some(resource) = analysed
            .iter()
            .find(|resource| resource.arn_in(&region) == input.resource_arn)
        else {
            ...
            return Ok(GetRDSDatabaseRecommendationProjectedMetricsOutput {
                recommended_option_projected_metrics: Some(Vec::new()),
            });
        };
```

Test coverage: tests/all/ec2_recommendation_projected_metrics.rs:851-856 pins the sibling's behaviour -
```
        (
            "another account's instance",
            with_arn(instance_arn(REGION, "210987654321", SIZED_DOWN)),
            400,
            "InvalidParameterValueException",
        ),
```
and tests/all/ecs_service_recommendation_projected_metrics.rs:513-515 carries the matching "another account's service" row. tests/all/rds_database_recommendation_projected_metrics.rs has the window/granularity refusal test (a_request_that_is_not_a_series_is_refused, line 673) but NO cross-account row at all, so nothing pins the lenient answer.

## Steps to Reproduce

1. Start the emulator and opt the account in with UpdateEnrollmentStatus {"status":"Active","includeMemberAccounts":false}; wait for the analysis delay so recommendations are visible.
2. Call GetEC2RecommendationProjectedMetrics with instanceArn="arn:aws:ec2:us-east-1:210987654321:instance/i-0123456789abcdef0", stat="Maximum", period=3600 and a forward window. Observe HTTP 400 InvalidParameterValueException with message "Invalid permission to access another account. If this is an organization account, request the management account to enable service access."
3. Call GetECSServiceRecommendationProjectedMetrics with serviceArn="arn:aws:ecs:us-east-1:210987654321:service/web/checkout" and the same stat/period/window. Observe the same HTTP 400 and message.
4. Call GetRDSDatabaseRecommendationProjectedMetrics with resourceArn="arn:aws:rds:us-east-1:210987654321:db:members-primary" and the same stat/period/window.
5. Expected (by the rule quoted on crate::state::is_foreign_account and applied by both siblings): HTTP 400 InvalidParameterValueException carrying CROSS_ACCOUNT_MESSAGE. Actual: HTTP 200 with {"recommendedOptionProjectedMetrics": []}.
