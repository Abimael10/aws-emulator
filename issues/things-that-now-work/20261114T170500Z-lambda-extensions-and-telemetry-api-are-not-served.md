Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies|run#5|AssertionError|assert resp["StatusCode"] == 200 and "FunctionError" not in resp, (
Issue-Commit: bc4720188099afd2bb4f421d1e83533814225f44

# The Extensions and Telemetry API are not served on the runtime-API endpoint

> _Split out of `issues/things-that-now-work/20261109T210043Z-lambda-10-net-new-causes-lambda-publishes-unreachable-data.md`
> (cause 5 of that bundle), which closed its other causes and carries this one's replay key. It was
> identified against commit `bc4720188099afd2bb4f421d1e83533814225f44`, so the code base you are
> working in can have drifted: since then `state::execution::runtime_api` has begun serving the runtime
> API's own address, so a request to `/2020-01-01/extension/register` now reaches that interface and is
> REFUSED there rather than failing on an address nothing resolved — which is not the same as serving
> the Extensions API, and is why this cause stays open._

## Operation
Invoke

## Description
The Extensions/Telemetry API register endpoint is not served.
AWS: an extension or a handler reaching `http://$AWS_LAMBDA_RUNTIME_API/2020-01-01/extension/register`
gets the Extensions API — a 200 whose `Lambda-Extension-Identifier` header names the registration — and
Telemetry API subscribe then validates its body and answers with documented error bodies.
Emulator: nothing registers an extension. At the commit this was filed against, the in-sandbox HTTP
client reported that `http://127.0.0.1:9001/2020-01-01/extension/register` "addresses no service this
emulator serves" and listed only AWS service names, so the handler failed to import; the interface now
answers that path, but it answers a refusal — no registration is made, no identifier is minted, and
nothing of the Telemetry API's validation contract is observable.
Why: no extension can register, and nothing of the Telemetry API's validation contract is observable.
Fix: the in-sandbox runtime-API router (`state::execution::runtime_api`) — serve the
`/2020-01-01/extension/*` and telemetry paths on the runtime API endpoint: register an extension
against the invocation's own state, mint its identifier, and validate a subscribe body with the error
bodies AWS words for each class.
Traces: lambda.telemetry-api-subscribe-validation-rejected-with-exact-error-bodies.

## Evidence
AssertionError: the probing invoke must itself succeed: FunctionError='Unhandled', log
"[ERROR] Runtime.ImportModuleError: ... http://127.0.0.1:9001/2020-01-01/extension/register addresses
no service this emulator serves: sqs, events, ... rds-data". Since the runtime-API address began being
served the same unit reaches its next step, `assert obs["status"] == status`, because the register call
is refused rather than served.
(hosts, ports and request ids in these lines are per-run values, never requirements.)

## Steps to Reproduce
From a handler (or extension) POST to `$AWS_LAMBDA_RUNTIME_API/2020-01-01/extension/register`, then
subscribe to the Telemetry API with a body that fails each documented validation and compare the status
and body of each answer.
