Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases
Issue-Key: iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases|run#14|AssertionError|assert mutate_got == version_gone, (
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# lambda: UpdateAlias on a deleted function reports the UNQUALIFIED function ARN in its ResourceNotFoundException message, not the ARN qualified with the requested FunctionVersion

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAlias

## Description
Cause 1 of 1: UpdateAlias's not-found message drops the FunctionVersion qualifier.

AWS:      Smithy (com-amazonaws-lambda-awsgirapiservice): UpdateAlias = PUT /2015-03-31/functions/{FunctionName}/aliases/{Name}, modelled errors include ResourceNotFoundException (httpError 404); its input carries FunctionVersion (pattern ^(\$LATEST(\.PUBLISHED)?|[0-9]+)$). UpdateAlias resolves the requested FunctionVersion before it touches the alias, so when the function (and therefore that version) does not exist it refuses 404 ResourceNotFoundException naming the QUALIFIED target. The message's fixed text is "Function not found: "; its variable part is the function ARN with the requested FunctionVersion appended as ":<FunctionVersion>" — i.e. arn:aws:lambda:<region>:<account-id>:function:<function-name>:<FunctionVersion>, every component taken from the request and the caller's own account/region. GetFunction with Qualifier=<version> on the same deleted function uses exactly that qualified form, and the emulator already produces it correctly there (that leg of the check passes), which is what pins the expected wording.

Emulator: UpdateAlias(FunctionName=<deleted fn>, Name=<alias>, FunctionVersion="1") answers 404 ResourceNotFoundException with the message built from the UNQUALIFIED function ARN — the ":<FunctionVersion>" suffix is missing — so the error names the function instead of the version that could not be resolved. The code is right; only the message's variable part is wrong.

Why:      A client cannot tell "the function is gone" from "that version is gone" out of the refusal. IaC and deployment tooling that compares the not-found target against the qualified ARN it asked for (to decide whether to re-publish a version versus recreate the function) takes the wrong branch, and log/alarm matchers keyed on the qualified ARN never fire.

Fix:      The version-resolution error path of the UpdateAlias handler in the lambda service crate: it must format the ResourceNotFoundException message from the function ARN qualified with the request's FunctionVersion, reusing the same qualified-ARN formatter GetFunction's Qualifier path already uses, rather than the bare function ARN.

Traces:   iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases — the preceding GetFunction(Qualifier="1") and GetAlias legs of the same check pass, so only UpdateAlias's message diverges.

## Evidence
Failing check iam__lambda.cfnuf-unqualified-delete-function-ignores-the-alias-and-removes-versions-and-aliases, [mismatch]:

AssertionError: updating the deleted function's alias answered ('ResourceNotFoundException', 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfnuf-cfcb9955fe34-fn'), expected ('ResourceNotFoundException', 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfnuf-cfcb9955fe34-fn:1')

Expected (AWS): HTTP 404, code ResourceNotFoundException, message "Function not found: <function-arn>:<FunctionVersion>" — fixed text "Function not found: ", variable part the function ARN from the request qualified with the request's FunctionVersion.
Actual (emulator): HTTP 404, code ResourceNotFoundException, message "Function not found: <function-arn>" — the ":<FunctionVersion>" qualifier is absent.

(The region, account id and function name above are this run's values filling the template; they are not part of the requirement.)

## Steps to Reproduce
1. CreateRole with the lambda.amazonaws.com trust policy; CreateFunction (python3.11, Code.ZipFile) under that role.
2. PublishVersion -> "1"; CreateAlias(Name="LATEST", FunctionVersion="1").
3. DeleteFunction(FunctionName=<fn>) with no Qualifier.
4. GetFunction(FunctionName=<fn>, Qualifier="1") -> 404 ResourceNotFoundException "Function not found: <fn-arn>:1" (emulator already matches).
5. UpdateAlias(FunctionName=<fn>, Name="LATEST", FunctionVersion="1").
   Expected: 404 ResourceNotFoundException "Function not found: <fn-arn>:1".
   Observed: 404 ResourceNotFoundException "Function not found: <fn-arn>".
