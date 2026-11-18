Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.account-settings-limits-drive-enforcement
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.account-settings-limits-drive-enforcement|run#13|AssertionError|assert code == "RequestEntityTooLargeException", (
Issue-Commit: bc4720188099afd2bb4f421d1e83533814225f44

# An oversize CreateFunction request is refused by the core with no error code, so the modelled 413 never reaches the caller

> _Split out of `issues/things-that-now-work/20261109T210043Z-lambda-8-net-new-causes-lambda-control-plane-writes.md`
> (its Cause 1 of 8), whose other seven causes are closed. This one is kept open on its own
> because it cannot be fixed from `services/`: the refusal is answered by the vendored core
> before any service sees the request. The cause, its trace and its key are reproduced above
> unchanged._

## Operation
CreateFunction

## Description
A deployment package over the account's `CodeSizeZipped` limit is answered with a bare HTTP 413 instead of a modelled, code-carrying error.

AWS: `CreateFunction` refuses a request whose body is larger than the quota published for that operation with `RequestEntityTooLargeException` and HTTP 413, carrying the error code and the message `Request must be smaller than <quota> bytes for the CreateFunction operation` in the body, so an SDK maps it to an exception type.

Emulator: answers a bare `413` with no body and no error code, so the SDK surfaces the transport status and a client cannot catch the case by error type.

## Where it is, and why it is not a service fix
The service half is already implemented and tested: `services/service-com-amazonaws-lambda-awsgirapiservice/src/service/request_quota.rs` refuses a body past the operation's published quota with `413`, the `RequestEntityTooLargeException` error code, the `x-amzn-errortype` header and that sentence, applied to every operation of this service by the per-operation wrapper in `src/service.rs` (`request_quota::refusing_body`).

The service never receives the request. The core's production serve path buffers at most `MAX_PRODUCTION_BODY_BYTES` — 16 MiB, `vendor/server/src/lib.rs:913` — and `read_production_body` (`vendor/server/src/lib.rs:993-1017`) answers a status-only `413` before `EmulatorService::handle_request` is called. A `CreateFunction` carrying a ~50 MB archive travels as a ~70 MB base64 body, so every request that could exercise this quota is refused there, with no error code, however the service's own table is written. Nothing under `services/` runs before that point, so the error body cannot be supplied from a service crate.

Required core change (one of):

* `read_production_body`'s refusal carries the addressed service's own error envelope — i.e. the over-size request is still handed to the service, or the core asks the service for the refusal body, so that `request_quota::refusing_body` answers it; or
* `MAX_PRODUCTION_BODY_BYTES` is raised above the largest per-operation request quota this emulator publishes (`UpdateFunctionCode`/`PublishLayerVersion`: 70,167,211 bytes), so an over-quota request reaches the service and is refused there with its code and message.

## Evidence
AssertionError: expected CreateFunction to reject a 58720514-byte deployment package, which exceeds the account's reported CodeSizeZipped limit of 52428800 bytes, with RequestEntityTooLargeException, got '413'.
(The byte counts are per-run values; the status/code pair and the message template are the requirements.)

## Steps to Reproduce
`GetAccountSettings` to read `CodeSizeZipped`, then `CreateFunction` with a `ZipFile` larger than it, against the emulator's production endpoint (the in-process router is not affected: there the service answers its own modelled 413).
