Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for
Issue-Key: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for|run#14|AssertionError|assert ip1 == ip2, f"sourceIp must be stable across calls from one client: {ip1!r} vs {ip2!r}"
Issue-Commit: 7ddb4188dd9af5c3d292129d1524d0998b9f045b

# A Lambda function URL takes requestContext.http.sourceIp from the client-supplied X-Forwarded-For header instead of the TCP connection

> _Note: this issue was identified against the code base at commit `7ddb4188dd9af5c3d292129d1524d0998b9f045b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeFunctionUrl

## Description
Cause 1 of 1: the function URL front door lets a client dictate its own sourceIp by sending an X-Forwarded-For header.

AWS: for a function URL invocation the payload-format-2.0 event's requestContext.http.sourceIp is the address of the TCP connection the request arrived on — it is derived from the connection, never from a request header. A client-supplied X-Forwarded-For is passed through to the function verbatim in headers['x-forwarded-for'] and does not change sourceIp; when the client sends no X-Forwarded-For, the front door synthesizes one equal to the real sourceIp alone. Consequently sourceIp is stable across two requests from the same client regardless of what headers they carry. This is the documented function-URL request payload (Lambda developer guide, "Invoking Lambda function URLs"); the event payload shape is not described in the Smithy model (recorded behavior plus documentation only).

Emulator: two signed GETs from one client to the same function URL report different sourceIp values — the plain GET reports the real peer address, and the GET that adds the header "X-Forwarded-For: 203.0.113.9" reports sourceIp '203.0.113.9', i.e. the client-supplied value. The emulator is populating sourceIp from the inbound X-Forwarded-For header rather than from the connection. (The real peer address recorded in this environment, '127.0.0.1', and the spoof value the check chooses are per-run values; the requirement is that sourceIp equals the connection's address and is unchanged by the header.)

Why: any authorization, rate limiting, geo/IP allow-listing or audit logging a function performs on requestContext.http.sourceIp is trivially forgeable by an unsigned request header — the caller controls the value the function believes it came from. Code that correlates sourceIp with the forwarded chain also sees two contradictory views of one client.

Fix: the function URL front door's event builder — populate requestContext.http.sourceIp from the TCP peer address of the inbound connection, keep the client's X-Forwarded-For in headers unmodified, and synthesize X-Forwarded-For from the peer address only when the client sent none.

Traces: iam__lambda__logs__sts.url-sourceip-from-tcp-connection-not-client-x-forwarded-for.

## Evidence
AssertionError: sourceIp must be stable across calls from one client: '127.0.0.1' vs '203.0.113.9'

The same client makes two SigV4-signed GETs to one AWS_IAM function URL; the second adds the unsigned header X-Forwarded-For: 203.0.113.9.
Expected (AWS): both events carry the same requestContext.http.sourceIp (the connection's address, never the header value); ev1.headers['x-forwarded-for'] == that sourceIp, ev2.headers['x-forwarded-for'] == '203.0.113.9' passed through verbatim.
Actual (emulator): sourceIp is '<peer address>' on the plain call and '203.0.113.9' — the client-supplied value — on the spoofed call.

## Steps to Reproduce
1. Create a python3.12 function whose handler returns the whole event as its JSON body, and a function URL config with AuthType=AWS_IAM, granting this caller lambda:InvokeFunctionUrl.
2. Make a SigV4-signed GET to the published URL with no X-Forwarded-For; read requestContext.http.sourceIp.
3. Make a second signed GET adding the header X-Forwarded-For: 203.0.113.9; read requestContext.http.sourceIp.
4. Observe the second call reports sourceIp 203.0.113.9 instead of the same connection address as the first.
