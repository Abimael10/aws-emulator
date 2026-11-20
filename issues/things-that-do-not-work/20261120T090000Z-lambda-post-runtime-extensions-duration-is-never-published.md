Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__lambda.metrics-lambda-post-runtime-extensions-duration-exists-only-with-an-extension
Issue-Key: cloudwatch__lambda.metrics-lambda-post-runtime-extensions-duration-exists-only-with-an-extension|_poll_until#15|AssertionError|raise AssertionError(f"{description} did not happen within {timeout_s}s; last read {last!r}")
Issue-Commit: c33038d6637516191c9271b81207503148b3a194

# AWS/Lambda PostRuntimeExtensionsDuration is never published, because nothing runs an extension

> _Split out of `issues/things-that-now-work/20261109T183935Z-cloudwatch-lambda-7-net-new-causes-aws-lambda.md`
> (cause 5 of that bundle), whose other six causes are fixed and which carries their replay keys. This
> report carries the one replay key that does NOT pass, re-filed here so it is not recorded as working.
> It was identified against commit `c33038d6637516191c9271b81207503148b3a194`, so the code base you are
> working in can have drifted._

## Operation
GetMetricStatistics

## Description
PostRuntimeExtensionsDuration is never published.
AWS: a function with a registered external extension records `PostRuntimeExtensionsDuration` under
`{FunctionName}` — the time extensions run after the handler returns — and a function with no
extension publishes no such series at all, so the metric's PRESENCE is the differential.
Emulator: the series reads `None` for the function WITH an extension, so the differential cannot
exist.

Why it is still open, and what it needs first: the series measures the time the runtime spends
running EXTENSION code after the handler returned, and in this emulator no extension code runs at
all. `crate::state::execution::runtime_api` serves the runtime API's own address but answers the
`/2020-01-01/extension/*` paths as unrouted, and nothing under `/opt/extensions/` is ever started.
That capability — executing an executable shipped in a LAYER inside the sandbox, plus the Extensions
API (Register / INVOKE / SHUTDOWN) lifecycle — is the subject of two other open reports:

* `issues/things-that-do-not-work/20261114T170500Z-lambda-extensions-and-telemetry-api-are-not-served.md`
  (the Extensions and Telemetry API on the runtime-API endpoint), and
* `issues/things-that-do-not-work/20261118T120000Z-lambda-cross-region-store-isolation-response-streaming-extensions-and-the-exec-wrapper.md`
  (cause 4 of that bundle: extensions never run and the Extensions API lifecycle is not enforced).

Until an extension really runs, there is no span to measure: publishing a duration for work nothing
performed would report an outcome no engine produced, which is worse than the absent series. So the
fix is that capability FIRST, and then one measurement — the span from the handler returning to the
last registered extension finishing its post-invoke work — published as one
`PostRuntimeExtensionsDuration` observation in `Milliseconds` per invocation of a function that has a
registered extension, in the dimension sets the invocation is metered on
(`crate::state::metrics::InvokedTarget`), and none at all for a function with no extension.

Fix: the Lambda extensions/runtime lifecycle component (`crate::state::execution::runtime_api` plus
the init phase of `crate::state::execution`), then the measurement in `crate::state::metrics` beside
`Duration`.
Trace: cloudwatch__lambda.metrics-lambda-post-runtime-extensions-duration-exists-only-with-an-extension.

## Evidence
'AssertionError: PostRuntimeExtensionsDuration for [FunctionName=...] did not happen within 240.0s;
last read None' — the same reading after the parent bundle's fix, which published `Errors`,
`Throttles`, `Duration`, the asynchronous-event counters, the provisioned-concurrency family and the
`ExecutedVersion`-qualified dimension sets, and left this one untouched.
(function names, hosts and request ids in these lines are per-run values, never requirements.)

## Steps to Reproduce
Deploy a function with an external extension (a layer carrying `extensions/<name>`), invoke it, and
poll `AWS/Lambda` `PostRuntimeExtensionsDuration` for `{FunctionName}`; then do the same for a twin
function with no extension and confirm that one has no such series.
