Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: d12a09599b9528aaa3eec06d793185a816ce0c56

# DescribeAppInstanceBot never answers the modelled NotFoundException for a bot that does not exist

_Filed: 2026-11-21T13:14:45Z_
_Operation: DescribeAppInstanceBot | Class: error-response_

> _Note: this issue was identified against the code base at commit `d12a09599b9528aaa3eec06d793185a816ce0c56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DescribeAppInstanceBot` is the only operation in this service whose model declares `NotFoundException` (HTTP 404), documented as the error for a resource that is not in the system. The handler never constructs it: every unresolvable `AppInstanceBotArn` — a bot never minted, one deleted, one cascaded away with its parent — is answered with `ForbiddenException` (403, "The client is permanently forbidden from making the request"), and the handler's own comment states that it is declining the 404 the model declares. A client that branches on the modelled 404 for an absent bot therefore never sees it, and the modelled error shape is unreachable in the whole crate.

## Evidence

Model (smithy_query query="operation" operation="DescribeAppInstanceBot"): errors include `{"shape": "NotFoundException", "errorKind": "client", "httpError": 404}`; smithy_query query="doc" shape="NotFoundException" -> "One or more of the resources in the request does not exist in the system."
Handler: `src/ops/describe_app_instance_bot.rs:168` `let Some(record) = state.bot(&input.app_instance_bot_arn) else {` -> `:169` `return Err(DescribeAppInstanceBotErrorModeled::ForbiddenException(ForbiddenException { code: Some("Forbidden".to_string()), message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()) }).into());`
The file admits the divergence at `:163`: "The refusal is therefore the SAME whatever left the ARN unresolvable (a bot never minted, one deleted, one cascaded away with its parent `AppInstance`, or an ARN naming another resource kind, account or region) ... which is what the 403 class means here and what the `NotFoundException` the model also declares for this operation would not."
Grep confirms the variant is never built: no `NotFoundException(` constructor anywhere under `src/ops/`.
Test coverage: `tests/all/app_instance_bot_description.rs:9` pins the 403 ("is refused with the modelled 403 `ForbiddenException` rather than answered or told that the resource is absent") — the reason it states (non-disclosure) is a crate choice, not the model's, and the model's own 404 contradicts it.

## Steps to Reproduce

1. CreateAppInstance -> AppInstanceArn A
2. GET /app-instance-bots/{A}%2Fbot%2Fnever-minted (DescribeAppInstanceBot on a bot ARN the service holds no record of)
3. Observe 403 ForbiddenException; the model declares NotFoundException (404) for exactly this operation, documented as "One or more of the resources in the request does not exist in the system."
4. Repeat after CreateAppInstanceBot + DeleteAppInstanceBot on a real bot ARN: same 403, never the modelled 404
