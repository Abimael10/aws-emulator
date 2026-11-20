Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewaymanagementapi.forbidden-without-manage-connections-permission-rejected apigatewaymanagementapi.manage-connections-denial-precedes-connection-lookup
Issue-Shape-Id: com.amazonaws.apigatewaymanagementapi#ApiGatewayManagementApi
Issue-Key: apigatewaymanagementapi.forbidden-without-manage-connections-permission-rejected|run#12|AssertionError|assert message == want, f"expected the refusal message {want!r}, got {message!r}"
Issue-Key: apigatewaymanagementapi.manage-connections-denial-precedes-connection-lookup|run#16|AssertionError|assert message == want, (
Issue-Commit: af00353512b8f96b4224cdf0a6e66e42785b9365

# apigatewaymanagementapi: the @connections AccessDeniedException message names the UNMASKED account id in the resource ARN instead of AWS's '********<last4>' form

> _Note: this issue was identified against the code base at commit `af00353512b8f96b4224cdf0a6e66e42785b9365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PostToConnection

## Description
One net-new root cause in the ApiGatewayManagementApi @connections authorization refusal. It is reached only now that the authorization gate itself denies: the nearest open issue, 20261113T113139Z-apigatewaymanagementapi-4-net-new-causes-connections-is-served.md (Cause 1), describes @connections being SERVED with no execute-api:ManageConnections evaluation at all (HTTP 200 / 410 GoneException for an unprivileged caller). That no longer happens — both checks below now receive HTTP 403 with code AccessDeniedException, and the authorization decision is correctly taken BEFORE the connection lookup (the GET case is refused 403, not 410). What remains, and what is filed here, is the refusal MESSAGE: the account id embedded in the resource ARN is emitted in full instead of masked.

Cause 1 of 1: the account id in the `on resource: arn:aws:execute-api:...` clause of the AccessDeniedException message is not masked.
AWS: ApiGatewayManagementApi is sigv4-only and its @connections operations are authorized against execute-api:ManageConnections (Smithy: PostToConnection http POST /@connections/{ConnectionId}, GetConnection http GET /@connections/{ConnectionId}; both model ForbiddenException @httpError 403, GoneException @httpError 410, LimitExceededException @httpError 429 — the IAM implicit-deny refusal itself is not a modeled shape, so this is recorded behavior for the message). A principal holding no execute-api:ManageConnections grant is refused HTTP 403 with error code AccessDeniedException and this message, whose FIXED text is `User: <caller> is not authorized to perform: execute-api:ManageConnections on resource: <resource-arn> because no identity-based policy allows the execute-api:ManageConnections action`. Its VARIABLE parts come from the request and the caller's identity: `<caller>` is the calling session ARN, and `<resource-arn>` is `arn:aws:execute-api:<region>:<masked account>:<apiId>/<stage>/<HTTP method of the operation>/@connections/{connectionId}`. `<masked account>` is the rule at issue: AWS replaces all but the LAST FOUR digits of the 12-digit account id with '*' characters, i.e. eight asterisks followed by the final four digits. The trailing `/@connections/{connectionId}` is fixed text — AWS never interpolates the caller's connection id into it — and the method segment is POST for PostToConnection and GET for GetConnection.
Emulator: the message is otherwise byte-identical to AWS's, including the caller ARN, the api id, the stage, the method segment and the literal route template, but the ARN's account field carries the full unmasked 12-digit account id of the calling account. Observed on PostToConnection (POST segment) and on GetConnection (GET segment), so it is the shared ARN-rendering step, not one handler.
Why: the masking is AWS's deliberate suppression of a full account id in an error returned to an unauthorized principal — an emulator that prints it in full leaks the account number to exactly the caller AWS withholds it from, and any client, test or log-redaction assertion that matches AWS's message (or parses the ARN expecting the masked field) does not match.
Fix: the execute-api authorization-denial message builder / AccessDeniedException serializer shared by the @connections data-plane handlers for this service — mask all but the last four digits of the account id when rendering the resource ARN for this message. The rest of the template is already correct.
Traces: apigatewaymanagementapi.forbidden-without-manage-connections-permission-rejected (POST), apigatewaymanagementapi.manage-connections-denial-precedes-connection-lookup (GET).

## Evidence
Cause 1 — PostToConnection (check apigatewaymanagementapi.forbidden-without-manage-connections-permission-rejected): an assumed-role session with no inline policy calls PostToConnection on a LIVE connection id.
  expected (AWS): HTTP 403, code AccessDeniedException, message `User: <session-arn> is not authorized to perform: execute-api:ManageConnections on resource: arn:aws:execute-api:<region>:********<last4 of account>:<apiId>/<stage>/POST/@connections/{connectionId} because no identity-based policy allows the execute-api:ManageConnections action`
  actual: same status (403) and same code (AccessDeniedException), but the account field of the resource ARN is the full account id — `arn:aws:execute-api:<region>:<full 12-digit account>:<apiId>/<stage>/POST/@connections/{connectionId}` — where AWS emits eight '*' then the last four digits. Replay assertion: `expected the refusal message '...:********0000:mp67t4dbd4/test/POST/@connections/{connectionId}...', got '...:000000000000:mp67t4dbd4/test/POST/@connections/{connectionId}...'` (the digits and api id shown are that run's values, not part of the requirement).

Cause 1 — GetConnection (check apigatewaymanagementapi.manage-connections-denial-precedes-connection-lookup): the same unprivileged session calls GetConnection, and the refusal correctly precedes the connection lookup.
  expected (AWS): HTTP 403 AccessDeniedException with `.../<stage>/GET/@connections/{connectionId}` and the account masked to eight '*' plus the last four digits.
  actual: 403 AccessDeniedException with the GET segment and route template correct, account unmasked. Replay assertion: `expected the refusal message '...:********0000:mp67t4dbd4/dev/GET/@connections/{connectionId}...' for GET @connections, got '...:000000000000:mp67t4dbd4/dev/GET/@connections/{connectionId}...'`.

Everything else in both messages — the fixed text, the caller session ARN, the action name, the api id/stage/method segments and the literal `@connections/{connectionId}` template — already matches AWS; the sole divergence is the unmasked account id.

## Steps to Reproduce
1. Create a WEBSOCKET apigatewayv2 API with a Lambda-backed $connect/$default/$disconnect, deploy a stage, open a client socket and learn the connection id from the $default echo.
2. Assume a role with NO inline policy (no execute-api:ManageConnections grant) and build an apigatewaymanagementapi client on that stage's management endpoint with those credentials.
3. Call PostToConnection(ConnectionId=<live id>, Data=b"denied"). Observe 403 AccessDeniedException (correct) whose message renders `on resource: arn:aws:execute-api:<region>:<FULL account id>:<apiId>/<stage>/POST/@connections/{connectionId}` instead of masking all but the last four account digits with '*'.
4. With the same unprivileged credentials call GetConnection(ConnectionId=<any well-formed id>). Observe the same unmasked account id in the ARN, with the GET method segment.
