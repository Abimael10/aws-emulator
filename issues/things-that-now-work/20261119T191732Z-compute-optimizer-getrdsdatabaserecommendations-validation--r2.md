Issue-Severity: HIGH
Issue-Gap-Class: validation
Issue-Commit: f420f96d635d512f65dbfe0e5d9b800a5c85b936

# GetRDSDatabaseRecommendations answers a resourceArns entry naming another account instead of refusing it

_Filed: 2026-11-19T19:17:32Z_
_Operation: GetRDSDatabaseRecommendations | Class: validation_

> _Note: this issue was identified against the code base at commit `f420f96d635d512f65dbfe0e5d9b800a5c85b936`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other ARN-taking reader in this crate refuses an ARN whose account field names an account the service does not answer for, with the crate's single cross-account refusal (`InvalidParameterValueException`, HTTP 400, `crate::state::CROSS_ACCOUNT_MESSAGE`). `GetRDSDatabaseRecommendations` validates each `resourceArns` entry only for ARN SHAPE and deliberately skips the account check, so `resourceArns: ["arn:aws:rds:us-east-1:210987654321:db:prod"]` is answered HTTP 200 with an empty `rdsDBRecommendations` list.

That is self-contradictory twice over:

1. The same handler refuses the SAME foreign account when it arrives on `accountIds` (via `crate::state::requested_account_id`), so one request member is refused and the other is silently answered with an empty selection a client reads as "that account has no databases".
2. `crate::state::is_foreign_account` — the helper the crate declares as the single test for this condition — states explicitly that an account id and the account field of an ARN are judged by the same rule. This handler never calls it.

## Evidence

The crate-wide rule, `src/state.rs:957-966`:

    /// Whether `account_id` — an id a request named, or the account field of an ARN it carried
    /// — names an account OTHER than the one this service answers for.
    ...
    /// and records no organization, so a resource owned by any other account is one no request
    /// can be answered about: a reader refuses it with [`CROSS_ACCOUNT_MESSAGE`] rather than
    /// resolving an answer under that other account. ONE test for the whole crate, so an
    /// account id and the account field of an ARN are judged by the same rule.
    pub fn is_foreign_account(account_id: &str) -> bool {

The handler that breaks it, `src/ops/get_rds_database_recommendations.rs:216-230`:

        // `resourceArns`: "The ARN that identifies the Amazon Aurora or RDS database" — so
        // each value is an ARN. WHICH resource an ARN names is not checked here: a
        // well-formed ARN this service holds no database recommendation for — including one
        // naming a resource of an account it does not answer for — selects nothing, and is
        // not refused.
        let resource_arns = input.resource_arns.clone().unwrap_or_default();
        if let Some(value) = resource_arns
            .iter()
            .find(|value| crate::state::parse_arn(value).is_none())
        { ... }

Sibling handlers in THIS crate that do apply it to their own ARN list:

- `src/ops/get_license_recommendations.rs:217-219`:
        if parsed.account_id != account_id {
            return Err(foreign_account());
        }
- `src/ops/get_idle_recommendations.rs:220-222`:
        if !parsed.account_id.is_empty() && parsed.account_id != account_id {
            return Err(foreign_account());
        }
- `src/ops/get_ecs_service_recommendations.rs:268-282` states the rule the RDS reader breaks: "An ARN whose ACCOUNT field names another account asks about a service this service cannot report on, exactly as `accountIds` naming that account does, so it is refused with the crate's cross-account message rather than reported in `errors`." followed by `if crate::state::is_foreign_account(&reference.account_id) { return Err(foreign_account()); }`
- `src/ops/get_auto_scaling_group_recommendations.rs:226-241`: same sentence, same check.

Test coverage, `tests/all/cross_account_resource_arns.rs:3-17` and `:88-90`:

    //! ... so no reader can answer what another refuses.
    /// Every reader of this service that takes a resource ARN, in each ARN form its member
    /// documents.
    const ARN_MEMBERS: [ArnMember; 7] = [ ... ];

That table asserts 400 + the cross-account message for 7 readers, and `GetRDSDatabaseRecommendations$resourceArns` is absent from it — so the divergence is untested rather than pinned.

## Steps to Reproduce

1. Start the emulator and call `UpdateEnrollmentStatus` with `{"status":"Active","includeMemberAccounts":false}` so the account is opted in.
2. Call `GetRDSDatabaseRecommendations` with `{"accountIds":["210987654321"]}` — observe HTTP 400 `InvalidParameterValueException` with message "Invalid permission to access another account. ..." (`crate::state::CROSS_ACCOUNT_MESSAGE`).
3. Call `GetRDSDatabaseRecommendations` with `{"resourceArns":["arn:aws:rds:us-east-1:210987654321:db:prod"]}` — observe HTTP 200 with `rdsDBRecommendations: []` instead of the same 400.
4. For contrast, call `GetEBSVolumeRecommendations` with `{"volumeArns":["arn:aws:ec2:us-east-1:210987654321:volume/vol-0123456789abcdef0"]}` — observe the HTTP 400 cross-account refusal that `tests/all/cross_account_resource_arns.rs` pins.
