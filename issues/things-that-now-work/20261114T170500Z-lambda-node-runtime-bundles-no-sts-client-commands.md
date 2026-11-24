Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.nodejs-function-sdk-v3-writes-binary-bytes-the-caller-reads-back
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.nodejs-function-sdk-v3-writes-binary-bytes-the-caller-reads-back|_invoke#20|AssertionError|raise AssertionError(
Issue-Commit: bc4720188099afd2bb4f421d1e83533814225f44

# The Node runtime's bundled @aws-sdk/client-sts exports no commands

> _Split out of `issues/things-that-now-work/20261109T210043Z-lambda-10-net-new-causes-lambda-publishes-unreachable-data.md`
> (cause 10 of that bundle), which closed its other causes and carries this one's replay key. It was
> identified against commit `bc4720188099afd2bb4f421d1e83533814225f44`, so the code base you are
> working in can have drifted._

## Operation
Invoke

## Description
The Node runtime's bundled `@aws-sdk/client-sts` exports no `GetCallerIdentityCommand`.
AWS: the managed Node runtimes bundle the AWS SDK v3; `import { GetCallerIdentityCommand } from
'@aws-sdk/client-sts'` resolves and the command runs against STS.
Emulator: the handler fails at load with `Runtime.UserCodeSyntaxError` — `SyntaxError: Could not find
export 'GetCallerIdentityCommand' in module
'file:///var/runtime/node_modules/@aws-sdk/client-sts'`.
Why: any Node handler that uses the bundled SDK v3 against STS cannot even be imported, so it fails as
a user syntax error rather than running.
Fix: the Node runtime's bundled SDK v3 client surface. The names an `@aws-sdk/client-<token>` package
exports are read out of the service model this emulator holds for that token
(`state::execution::node_sdk_exports::client_exports`, over `state::execution::aws_api::APIS`), and no
entry of that table answers to `sts` — so the package exports its client class and no command. STS's
own model declares `awsQuery`, for which this crate holds one bespoke client
(`state::execution::aws_sns`) and no general one, so the entry needs that protocol half beside it: a
call the guest makes has to be BUILT and PARSED as that protocol, not only named.
Traces: lambda.nodejs-function-sdk-v3-writes-binary-bytes-the-caller-reads-back.

## Evidence
AssertionError: the function raised (Unhandled):
{"errorMessage":"SyntaxError: Could not find export 'GetCallerIdentityCommand' in module
'file:///var/runtime/node_modules/@aws-sdk/client-sts'","errorType":"Runtime.UserCodeSyntaxError"}.

## Steps to Reproduce
Invoke a nodejs function whose handler imports `GetCallerIdentityCommand` from `@aws-sdk/client-sts`
and sends it, then reads the identity it answered.
