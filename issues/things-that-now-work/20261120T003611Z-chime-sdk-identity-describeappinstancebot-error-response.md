Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 0a5d9cda53656d0e20014bdf39cff0e297709330

# DescribeAppInstanceBot never serves the NotFoundException the model declares for it — an absent bot is answered 403 Forbidden instead

_Filed: 2026-11-20T00:36:11Z_
_Operation: DescribeAppInstanceBot | Class: error-response_

> _Note: this issue was identified against the code base at commit `0a5d9cda53656d0e20014bdf39cff0e297709330`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

DescribeAppInstanceBot is the ONLY operation in this service whose model declares `NotFoundException` (HTTP 404), and its documented trigger is "One or more of the resources in the request does not exist in the system." The operation names exactly one resource — `AppInstanceBotArn`, `@required`, `@httpLabel` — so the condition that trigger describes is precisely "this ARN names no AppInstanceBot".

The handler answers that exact condition with `ForbiddenException` instead, on the reasoning (written in its own comment) that resolving the bot ARN is "an ACCESS decision rather than a lookup that reports absence" and that "the modelled 404 — for resources a request names BESIDE the one it addresses — never applies here". That second clause is the handler's own invention: nothing in the model restricts the 404 to co-named resources, and the model gives this operation no second resource member for such a restriction to be about. The consequence is that the declared error shape is DEAD: the `NotFoundException(NotFoundException)` variant generated for this operation's error enum is never constructed anywhere in the crate, so no input whatsoever can make this emulator serve the one 404 its model declares.

The effect on a caller is the ordinary one for a misclassified refusal: a client that branches on NotFoundException (the shape the SDK generates for this operation and for no other call in the service) never takes that branch, and reads a permanent authorization refusal for what the model classifies as a missing resource.

Note the related precedent in the crate: the 403-for-an-absent-grant rule was introduced for DescribeAppInstanceAdmin by a fixed issue, and DescribeAppInstanceAdmin's model declares NO 404 shape at all — so copying that rule onto DescribeAppInstanceBot, which does declare one, is the step that contradicts the model.

## Evidence

The model declares the 404 for this operation and documents its trigger — `smithy_query(query="operation",operation="DescribeAppInstanceBot")`:
```
"errors": [... {"shape": "NotFoundException", "errorKind": "client", "httpError": 404} ...],
"input": {"AppInstanceBotArn": {"target": "ChimeArn", "httpLabel": true, "required": true, ...}}
```
and `smithy_query(query="doc",shape="NotFoundException")`:
```
"documentation": "<p>One or more of the resources in the request does not exist in the system.</p>"
```
It is declared for no other operation of this service — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/OPERATIONS.md:108 is the only line in the file that names it:
```
- **Errors:** `BadRequestException (HTTP 400)`, `ForbiddenException (HTTP 403)`, `NotFoundException (HTTP 404)`, `ServiceFailureException (HTTP 500)`, ...
```

The handler's OWN doc header restates that trigger — src/ops/describe_app_instance_bot.rs:61-62:
```
//! * `NotFoundException` (HTTP 404) — One or more of the resources in the request does not
//!   exist in the system.
```

and the code under it does the opposite for exactly that case — src/ops/describe_app_instance_bot.rs:154-170:
```
        // Resolving the `AppInstanceBotArn` is an ACCESS decision rather than a lookup
        // that reports absence: an ARN this service holds no bot for names a bot this
        // caller may not read, so the answer is the permanent 403 refusal, and it tells
        // the caller nothing about whether such a bot exists anywhere. The only resource
        // the request names is the one being resolved, so the modelled 404 — for
        // resources a request names BESIDE the one it addresses — never applies here.
        let Some(record) = state.bot(&input.app_instance_bot_arn) else {
            return Err(DescribeAppInstanceBotErrorModeled::ForbiddenException(
                ForbiddenException {
                    code: Some("Forbidden".to_string()),
                    message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()),
                },
            )
            .into());
        };
```

The declared variant exists and is never constructed: `grep -n NotFoundException src/` returns only the generated declarations — src/ops/describe_app_instance_bot.rs:88 (`NotFoundException(NotFoundException),`) and src/ops/types/describe_app_instance_bot_types.rs:37-39 (the struct) — and no construction site anywhere in the crate.

Test coverage: tests/all/app_instance_bot_description.rs:386-449 pins the 403 for all seven ways an ARN can fail to resolve to a bot, with the stated reason at :9-10 "an `AppInstanceBotArn` this service holds no bot for is an ARN the caller may not read, so it is refused with the modelled 403 `ForbiddenException` rather than answered or told that the resource is absent" — which is the reason the model's declared 404 and its quoted trigger contradict. No trace-derived issue covers the bot case: the fixed 403 rule in 20261103T105532Z-chime-sdk-identity-describeappinstanceadmin-on-an-absent-demoted is about DescribeAppInstanceAdmin, whose model declares no 404 (OPERATIONS.md:101).

## Steps to Reproduce

1. POST /app-instances with {"Name":"bots","ClientRequestToken":"tok-1"} -> AppInstanceArn A
2. POST /app-instance-bots with {"AppInstanceArn":A,"ClientRequestToken":"tok-2","Name":"helper","Configuration":{"Lex":{"LexBotAliasArn":"arn:aws:lex:us-west-2:123456789012:bot-alias/BOTIDKEPTA/ALIASIDAAA","LocaleId":"en_US","RespondsTo":"STANDARD_MESSAGES"}}} -> AppInstanceBotArn B (confirms the route works)
3. GET /app-instance-bots/{A}%2Fbot%2Fnever-minted (a well-formed ChimeArn under the live instance that names no bot)
4. Observe HTTP 403 with {"Code":"Forbidden","Message":"You are not allowed to make this request."}, where the model's only declared 404 for this operation — NotFoundException, documented as "One or more of the resources in the request does not exist in the system." — applies
5. DELETE /app-instance-bots/{B}, then GET /app-instance-bots/{B}: same 403, so no input to this operation can ever produce the declared NotFoundException
