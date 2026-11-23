Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ce9da1949a31b48f6e624e7aad44b9692a7b8b4d

# An empty functionArns list selects no function instead of the account's whole inventory

_Filed: 2026-11-23T20:03:38Z_
_Operation: GetLambdaFunctionRecommendations | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ce9da1949a31b48f6e624e7aad44b9692a7b8b4d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`requested_functions` returns `Ok(None)` ("ask about everything") only when `functionArns` is ABSENT. A request carrying `"functionArns": []` enters the `Some(..)` branch, pushes nothing and returns `Ok(Some(vec![]))`, so the selection `wanted.contains(&function.arn_resource())` excludes every analysed function and the handler answers an empty `lambdaFunctionRecommendations` list. The helper's own doc states the opposite rule ("`None` when it names none"), the EC2 instance reader normalises the empty list explicitly, and the ASG/RDS/license/idle readers all test `is_empty() ||` first — so one reader of this crate answers nothing where its siblings answer the whole inventory.

## Evidence

This handler: `services/.../src/ops/get_lambda_function_recommendations.rs:429-436` — doc "The function VERSIONS this request asks about, by the resource part of their ARN, or `None` when it names none and so asks about the account's whole inventory in this Region." but the code is only `let Some(function_arns) = function_arns else { return Ok(None); };`, so an empty list reaches line 468 `Ok(Some(selected))` with `selected` empty; line 306-310 then filters `.filter(|function| requested_functions.as_ref().is_none_or(|wanted| wanted.contains(&function.arn_resource())))`.

Sibling that normalises it — `services/.../src/ops/get_ec2_instance_recommendations.rs:450-452`: `if instance_arns.is_empty() { return Ok(None); }`.

Crate rule pinned for a sibling: `tests/all/license_recommendations.rs:581-585` — "Rows: the member absent, empty, one ARN, ...", `("empty", json!({"resourceArns": []}), all.clone())`.

## Steps to Reproduce

1. Opt in with UpdateEnrollmentStatus(status=Active) and register analysed Lambda functions so GetLambdaFunctionRecommendations reports them
2. Call GetLambdaFunctionRecommendations with `{}` and note the functions reported
3. Call GetLambdaFunctionRecommendations with `{"functionArns": []}`
4. Observe `lambdaFunctionRecommendations` is empty, unlike the absent-member call and unlike the sibling readers' handling of an empty ARN list
