Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__simpledbv2.export-domain-to-s3-contains-written-items s3__simpledbv2.repeated-client-token-with-different-params-rejected
Issue-Key: s3__simpledbv2.export-domain-to-s3-contains-written-items|run#6|ResponseParserError|sdb.create_domain(DomainName=domain)
Issue-Key: s3__simpledbv2.repeated-client-token-with-different-params-rejected|run#6|ResponseParserError|sdb.create_domain(DomainName=domain)
Issue-Commit: ffc87a9d44db1f32843bf66b14ed7266d71ed3fd

# REOPENS: Amazon SimpleDB 2009-04-15 on the sdb endpoint is still refused — a SigV2-signed CreateDomain answers AccessDenied "Sign the request with the emulator credentials", and the refusal is serialized as a JSON __type body the query-protocol client cannot parse (2 causes)

> _Note: this issue was identified against the code base at commit `ffc87a9d44db1f32843bf66b14ed7266d71ed3fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
REOPENS 20261114T064955Z-simpledbv2-reopens-simpledb-2009-04-15-createdomain-on-the.md: its fix left the call refused. Both cross-service domain-export checks still error on their very first SimpleDB call. The refusal has changed shape — it is no longer HTTP 400 InvalidAction "unknown action: CreateDomain" but an AccessDenied refusal with a JSON body — so the request is no longer answered by the routing default, but it is still never served, and nothing of the domain-export lifecycle is exercised.

Cause 1 of 2: a request to the sdb endpoint signed with AWS Signature Version 2 is not authenticated, so every Amazon SimpleDB 2009-04-15 call is refused AccessDenied. Failing check ids for this cause: s3__simpledbv2.export-domain-to-s3-contains-written-items, s3__simpledbv2.repeated-client-token-with-different-params-rejected.

  AWS: recorded behavior only — this repo carries no Smithy model for the 2009-04-15 query surface; the endpoint's wire identity comes from com.amazonaws.simpledbv2#SimpleDBv2 (endpointPrefix/sigv4 name "sdb"), which is cited only for that shared identity. Amazon SimpleDB is a Signature Version 2 service: an SDK's sdb client (botocore's sdb 2009-04-15 model declares signatureVersion v2) sends a form-encoded POST to "/" whose BODY carries Action=CreateDomain, Version=2009-04-15, DomainName=<name>, AWSAccessKeyId, SignatureVersion=2, SignatureMethod=HmacSHA256, Timestamp and Signature — and NO Authorization header at all. AWS accepts that signature on this endpoint and answers HTTP 200 with <CreateDomainResponse xmlns="http://sdb.amazonaws.com/doc/2009-04-15/"> carrying only <ResponseMetadata><RequestId>..</RequestId><BoxUsage>..</BoxUsage></ResponseMetadata>; the domain it creates is then a valid domainName for the v2 StartDomainExport/ListExports/GetExport surface. (RequestId and BoxUsage are per-run values, not requirements.)
  Emulator: the same call is answered with a non-2xx carrying error code AccessDenied and message "Sign the request with the emulator credentials" — the emulator's own signature gate, the identical refusal it gives any request that does not present a SigV4 Authorization header it recognises. Its fixed text is the whole sentence "Sign the request with the emulator credentials" and it has no variable part. Hypothesis: the auth seam requires a SigV4 Authorization header and has no SigV2 path, so a SimpleDB request whose credential and signature live in the form-encoded body is seen as unsigned; the credentials themselves are good, since every other check in this run signs with SigV4 and is served.
  Why: a client doing exactly what AWS requires — create a domain, PutAttributes items into it, then export the domain to S3 — is refused at its first call, so no client can ever obtain a domain and the whole v2 export surface (exportStatus progression to SUCCEEDED, the exported data file under the key prefix, clientToken idempotency, ListExports) is unreachable through the only API that can create its input. The code is also wrong for the condition: AccessDenied tells a client its credentials are not permitted, when in fact the request was correctly signed for this service.
  Fix: the sdb endpoint's request-authentication seam — it must accept AWS Signature Version 2 (credential and signature taken from the form-encoded request parameters, not from an Authorization header) for the Amazon SimpleDB 2009-04-15 query surface, and only then hand the request to the already-implemented 2009-04-15 action handlers.

Cause 2 of 2: a refusal on the SimpleDB 2009-04-15 query surface is serialized as a restJson1 error document, so a query-protocol client cannot read the error at all. Failing check ids for this cause: s3__simpledbv2.export-domain-to-s3-contains-written-items, s3__simpledbv2.repeated-client-token-with-different-params-rejected.

  AWS: recorded behavior only (no Smithy model for the 2009-04-15 surface in this repo). The 2009-04-15 surface is an AWS Query/XML API: both its success and its error documents are XML in the http://sdb.amazonaws.com/doc/2009-04-15/ namespace. An error is the query-protocol error envelope — a <Response> carrying <Errors><Error><Code>..</Code><Message>..</Message></Error></Errors> and a <RequestID> — whose Code is the fixed error identifier and whose Message is that error class's text; it is never a JSON object and never carries a "__type" member.
  Emulator: the refusal body is b'{"__type":"AccessDenied","message":"Sign the request with the emulator credentials"}' — a JSON (restJson1-shaped) error document returned for a form-encoded query request. The SDK, parsing this endpoint with the query/XML error parser, cannot read it and raises botocore.parsers.ResponseParserError: Unable to parse response (not well-formed (invalid token): line 1, column 0), invalid XML received. So the error code never reaches the client's error branch at all.
  Why: even once the status and code are right, a client gets a parser crash rather than a ClientError it can inspect: no Error.Code, no Error.Message, and no chance to branch on the condition or retry. Any refusal on this surface is therefore undiagnosable from the client side, and it also mislabels the endpoint's protocol to anything inspecting the body.
  Fix: the sdb service's error serializer / protocol selection for the 2009-04-15 query surface — a request that arrived as a form-encoded Action=... POST on "/" must have its refusal rendered as the SimpleDB query XML error envelope in the http://sdb.amazonaws.com/doc/2009-04-15/ namespace, not as the service's restJson1 __type document (the v2 /v2/* operations keep the JSON form).

Related, not covering this: the open issue 20261109T154301Z-s3-simpledbv2-domain-export-to-s3-checks-blocked-simpledb-2009.md names these same two checks but describes the earlier HTTP 400 InvalidAction "unknown action: CreateDomain" refusal; the refusal observed in this run is a different one (AccessDenied with a JSON body), and the suite did not report these checks as owned by it.

Traces: s3__simpledbv2.export-domain-to-s3-contains-written-items, s3__simpledbv2.repeated-client-token-with-different-params-rejected (both verdict error, both failing on the first sdb.create_domain call).

## Evidence
Both s3__simpledbv2 checks fail identically on their first SimpleDB call, sdb.create_domain(DomainName=<unique name>) with sdb = ctx.client("sdb") (Amazon SimpleDB, API version 2009-04-15) — a form-encoded, SigV2-signed POST to "/" on the sdb endpoint:

  File "parsers.py", line 600, in _do_error_parse
      root = self._parse_xml_string_to_dom(xml_contents)
  File "parsers.py", line 550, in _parse_xml_string_to_dom
      raise ResponseParserError(
  botocore.parsers.ResponseParserError: Unable to parse response (not well-formed (invalid token): line 1, column 0), invalid XML received. Further retries may succeed:
  b'{"__type":"AccessDenied","message":"Sign the request with the emulator credentials"}'

Expected (AWS): HTTP 200 with body
  <CreateDomainResponse xmlns="http://sdb.amazonaws.com/doc/2009-04-15/"><ResponseMetadata><RequestId>..</RequestId><BoxUsage>..</BoxUsage></ResponseMetadata></CreateDomainResponse>
and the domain afterwards resolvable as domainName by StartDomainExport. Any refusal on this surface is the query XML error envelope (<Response><Errors><Error><Code>..</Code><Message>..</Message></Error></Errors><RequestID>..</RequestID></Response>) in that same namespace.

Actual (emulator): a non-2xx (botocore entered its error-parsing path) whose error code is AccessDenied, whose message is the fixed sentence "Sign the request with the emulator credentials", and whose body is JSON — b'{"__type":"AccessDenied","message":"Sign the request with the emulator credentials"}' — rather than XML, so the SDK raises ResponseParserError and the code/message never surface as a ClientError.

That the credentials are sound is shown by the rest of this run: every other service in the suite is signed with SigV4 by the same harness and is served. SimpleDB is the one surface the SDK signs with Signature Version 2 (credential and signature carried in the form-encoded body, no Authorization header), and it is the one surface refused for being unsigned. The same refusal message is what the emulator returns for other requests it does not consider SigV4-signed.

Because the very first call is refused, no assertion past it was reached in either check: no exportStatus progression to SUCCEEDED, no exported data file under exports/, no byte-faithful item comparison, no clientToken idempotent-retry equality, and no ConflictException "Idempotent request failed due to parameter mismatch." for a reused token with changed parameters.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Point an SDK client at it with the legacy SimpleDB API: sdb = boto3.client("sdb")  # Amazon SimpleDB, 2009-04-15, signatureVersion v2.
3. Call sdb.create_domain(DomainName="behav-domain-1"). On the wire this is POST "/" with a form-encoded body Action=CreateDomain&Version=2009-04-15&DomainName=behav-domain-1&AWSAccessKeyId=...&SignatureVersion=2&SignatureMethod=HmacSHA256&Timestamp=...&Signature=... and NO Authorization header.
   Observe: the body comes back as {"__type":"AccessDenied","message":"Sign the request with the emulator credentials"} and botocore raises ResponseParserError (invalid XML), instead of HTTP 200 <CreateDomainResponse xmlns="http://sdb.amazonaws.com/doc/2009-04-15/">...</CreateDomainResponse>.
4. Same for sdb.put_attributes(DomainName="behav-domain-1", ItemName="item-widget", Attributes=[{"Name":"color","Value":"blue","Replace":True}]) and sdb.delete_domain(DomainName="behav-domain-1").
5. To see cause 2 in isolation once cause 1 is fixed: send any refusable form-encoded Action=... POST on "/" (e.g. an unknown Action) and confirm the refusal body is the SimpleDB query XML error envelope in the http://sdb.amazonaws.com/doc/2009-04-15/ namespace, not a JSON __type document.
6. Then exercise the v2 surface the checks need: StartDomainExport(domainName="behav-domain-1", s3Bucket=<bucket>, s3KeyPrefix="exports/", clientToken=<uuid>), poll GetExport to SUCCEEDED, read the exported data file under exports/, repeat StartDomainExport with the same token (same exportArn) and then with the same token but s3KeyPrefix="exports/changed/" (ConflictException "Idempotent request failed due to parameter mismatch.").
7. Re-replay checks: s3__simpledbv2.export-domain-to-s3-contains-written-items, s3__simpledbv2.repeated-client-token-with-different-params-rejected
