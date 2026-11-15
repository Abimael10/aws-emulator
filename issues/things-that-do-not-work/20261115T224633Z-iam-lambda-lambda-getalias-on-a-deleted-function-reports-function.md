Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases
Issue-Key: iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases|run#12|AssertionError|assert got == expected, f"the deleted function's alias answered {got!r}, expected {expected!r}"
Issue-Commit: bcfb02b63b9a0fdb9bee01e1437bdea83d16d1e1

# Lambda GetAlias on a deleted function reports "Function not found" instead of AWS's "Cannot find alias arn: <alias-arn>"

> _Note: this issue was identified against the code base at commit `bcfb02b63b9a0fdb9bee01e1437bdea83d16d1e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAlias

## Description
One cause.

Cause 1 of 1: GetAlias keys its 404 on the FUNCTION rather than on the ALIAS when the named function does not exist.

AWS: Lambda GetAlias (Smithy: GET /2015-03-31/functions/{FunctionName}/aliases/{Name}, errors ResourceNotFoundException httpError 404, InvalidParameterValueException 400, TooManyRequestsException 429, ServiceException 500). The model carries no message text, so recorded behavior only: GetAlias reports a miss as a MISSING ALIAS, keyed on the alias ARN, whatever the reason the alias cannot be found — the function is gone, or the function exists and carries no such alias. The message's fixed text is `Cannot find alias arn: `; its variable part is the alias ARN assembled from the request, `arn:aws:lambda:<region>:<account-id>:function:<FunctionName>:<Name>`. The recording shows the same wording for both legs of the check: after an unqualified DeleteFunction, and again against a function freshly recreated under the same name with no aliases. Note that the function-keyed wording IS correct for GetFunction on the same dead function: the recording accepts `Function not found: <function-arn>:1` from GetFunction(Qualifier="1"), and that assertion passes in this run — so the divergence is specific to GetAlias, not to the function-resolution message in general.

Emulator: the call is refused 404 ResourceNotFoundException (correct class) but with the function-resolution message `Function not found: arn:aws:lambda:<region>:<account-id>:function:<FunctionName>` — no alias ARN, no alias name, and the `Cannot find alias arn: ` text absent. Replay: expected `('ResourceNotFoundException', 'Cannot find alias arn: <function-arn>:LATEST')`, got `('ResourceNotFoundException', 'Function not found: <function-arn>')`. The handler resolves the function before it looks at `Name` and propagates the resolver's NotFound message straight out, so the alias-not-found message it already owns is only ever reached when the function exists.

Why: a client that reads this refusal to decide what to do next is misdirected — the message says the function is missing when the request's subject is the alias, so tooling that branches on the message (create the alias vs. create the function, or surface "alias not configured" to a user) takes the wrong branch, and a differential harness comparing refusal text against AWS sees a mismatch on every GetAlias of a name that holds no alias because its function is gone. The two legs of the check also become indistinguishable from each other even though AWS words them identically.

Fix: the GetAlias handler and its not-found mapping — services/service-com-amazonaws-lambda-awsgirapiservice/src/ops/get_alias.rs. The function-resolution failure must be mapped onto the alias-not-found message (keyed on the alias ARN for the request's FunctionName and Name), the same message the handler already produces when the function exists but holds no such alias, rather than surfacing the resolver's function-keyed text.

Traces: iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases.

## Evidence
Check iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases, verdict mismatch:

  AssertionError: the deleted function's alias answered ('ResourceNotFoundException', 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfnuf-56e05e24c8df-fn'), expected ('ResourceNotFoundException', 'Cannot find alias arn: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfnuf-56e05e24c8df-fn:LATEST')

Expected (AWS): HTTP 404, code ResourceNotFoundException, message fixed text `Cannot find alias arn: ` followed by the alias ARN built from the request (`...:function:<FunctionName>:<Name>`). Region, account id, function token and alias name above are this run's values filling that template.

Actual (emulator): HTTP 404, code ResourceNotFoundException, message fixed text `Function not found: ` followed by the UNQUALIFIED function ARN — the alias name never appears.

The immediately preceding assertion in the same check passes, so GetFunction(Qualifier="1") on the same deleted function correctly answers `Function not found: <function-arn>:1`; only the GetAlias leg diverges.

## Steps to Reproduce
1. CreateRole with a lambda.amazonaws.com trust policy; CreateFunction (python3.11) under a unique name; wait for State=Active.
2. PublishVersion (returns "1"); CreateAlias Name=LATEST FunctionVersion=1.
3. Record the function ARN from GetFunctionConfiguration.
4. DeleteFunction with no Qualifier.
5. Call GetFunction(FunctionName=<name>, Qualifier="1") — observe ResourceNotFoundException `Function not found: <fn-arn>:1` (matches AWS).
6. Call GetAlias(FunctionName=<name>, Name="LATEST") — AWS answers 404 ResourceNotFoundException `Cannot find alias arn: <fn-arn>:LATEST`; the emulator answers 404 ResourceNotFoundException `Function not found: <fn-arn>`.
7. Recreate the function under the same name and call GetAlias(FunctionName=<name>, Name="LATEST") again — AWS answers the same `Cannot find alias arn: <fn-arn>:LATEST`.
