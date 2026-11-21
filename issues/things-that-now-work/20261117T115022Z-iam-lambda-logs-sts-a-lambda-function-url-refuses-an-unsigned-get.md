Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for
Issue-Key: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for|run#7|AssertionError|assert plain["status"] == 200, f"the plain GET must be served 200, got {plain!r}"
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# A Lambda function URL refuses an unsigned GET with 403 InvalidSignatureException instead of serving it

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeFunctionUrl

## Description
Cause 1 of 1: the function-URL data plane requires a SigV4 signature on a request that must be served without one.
AWS: recorded behavior only for the URL data plane — a Lambda function URL serves the request and returns the handler's response (HTTP 200 for this handler) for the plain GET the scenario sends; the URL's own source-IP observable (the event's requestContext.http.sourceIp) must come from the TCP connection, not from a client-supplied X-Forwarded-For, which is what the check goes on to verify. 'InvalidSignatureException' with the message 'The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details.' is a SigV4 verification failure of the AWS control plane, not an answer a function URL gives this request.
Emulator: the plain GET to the function URL is answered HTTP 403 with x-amzn-errortype: InvalidSignatureException and that signature message, so the handler never runs and the sourceIp contract is never observable.
Why: every unsigned client of a function URL (a browser, a webhook sender) is refused 403; and because the refusal is a signature error rather than an authorization one, a caller cannot even tell that signing was expected of it.
Fix: the function-URL request path's authentication stage — it must apply the URL's own auth type to the data-plane request rather than requiring control-plane SigV4 verification, and only then invoke the handler.
Failing check: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for

## Evidence
[mismatch] iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for — AssertionError: 'the plain GET must be served 200, got {'status': 403, 'headers': {... 'x-amzn-errortype': 'InvalidSignatureException' ...}, 'body': b'{"message":"The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}'}'.

## Steps to Reproduce
1. Create a function with a function URL and invoke the URL with a plain GET (no SigV4 signature).
2. Observe 403 InvalidSignatureException instead of the handler's 200; then repeat with an X-Forwarded-For header and inspect the event's requestContext.http.sourceIp (it must be the connection's address).
