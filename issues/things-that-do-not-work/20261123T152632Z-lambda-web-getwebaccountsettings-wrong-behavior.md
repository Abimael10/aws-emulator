Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: c3e9e6b2ad75a47d5c834c28447cb874a69b2715

# accountUsage.functionCount counts every stored web function regardless of Region, although the member reports usage "in the current AWS Region"

_Filed: 2026-11-23T15:26:32Z_
_Operation: GetWebAccountSettings | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `c3e9e6b2ad75a47d5c834c28447cb874a69b2715`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetWebAccountSettings` is documented as reporting "account settings for the current AWS Region", and `AccountUsage$functionCount` is "The number of web functions in your account in the current AWS Region." The handler reads no Region at all: it discards `ctx` (`let _ = (&input, ctx);`) and answers `state.account_usage()`, which is `self.functions.len()` over the whole store. Functions are created in whatever Region signed the create — `CreateWebFunction` mints a Region-bearing ARN from `crate::state::request_region(ctx.parts())` — but they all land in one `functions: BTreeMap<String, WebFunction>` keyed by name only, so a settings read signed for us-east-1 counts functions whose own `functionArn` names eu-west-1 and ap-southeast-2. The reported usage is therefore an all-Regions total presented as a per-Region one.

The handler's own comment asserts the opposite of what it does: it says the Region reported on is "the one the request was made against", on the line that throws the request away. The crate already has what the count needs — `request_region` for the caller's Region and `::service_common::arn::region_of(..)` which `CreateWebFunctionEndpoint` uses to read a function's Region back out of its stored ARN — so the Region each stored function belongs to is recoverable; nothing reads it here.

## Evidence

Smithy (`smithy_query(query="doc", shape="GetWebAccountSettings")`): "Retrieves details about your AWS Lambda Web Functions account settings for the current AWS Region, including the quotas that apply to web functions and your current usage." and (`smithy_query(query="shape", shape="AccountUsage")`, member `functionCount`): "The number of web functions in your account in the current AWS Region. This count includes web functions in all states, not only functions in the Active state." — the same two sentences are copied into the handler at services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_account_settings.rs:49-57 ("* `accountUsage` (AccountUsage) **required** — The current web function usage for your account in the current AWS Region.").

The handler discards the request, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_account_settings.rs:143-145:
```
        // The operation takes no input: the account and Region it reports on are
        // the ones the request was made against.
        let _ = (&input, ctx);
```
and the count it serves, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/account.rs:75-79:
```
    pub fn account_usage(&self) -> AccountUsage {
        AccountUsage {
            function_count: as_count_member(self.functions.len()),
        }
    }
```

The Region each function belongs to IS fixed at create, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function.rs:266 and :396:
```
        let region = crate::state::request_region(ctx.parts());
...
        let function_arn = crate::state::function_arn(&region, &input.function_name);
```
and the crate already reads it back, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:346:
```
    ::service_common::arn::region_of(&function.function_arn, crate::state::ARN_NAMESPACE)
```

Test coverage: no test exercises a cross-Region count. Every call in tests/all/web_account_settings.rs uses the single `const REGION: &str = "us-east-1";` (line 31), and the one multi-Region test, `the_quotas_do_not_depend_on_which_region_signed_the_request` (line 435), compares only `accountQuotas` across the four Regions (line 464-475) — it asserts nothing about `accountUsage`.

## Steps to Reproduce

1. PUT /2025-03-07/web-functions with a request signed for us-east-1, creating function "east-one"
2. PUT /2025-03-07/web-functions with a request signed for eu-west-1, creating function "west-one" (its functionArn names eu-west-1)
3. GET /2025-03-07/web-account-settings signed for us-east-1
4. Observe accountUsage.functionCount = 2, although only one web function exists in the current AWS Region the response is documented to report on
