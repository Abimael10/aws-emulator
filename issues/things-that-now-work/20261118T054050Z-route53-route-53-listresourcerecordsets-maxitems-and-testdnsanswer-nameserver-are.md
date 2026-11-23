Issue-Source: trace-sweep-divergence
Issue-Trace: route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|ListResourceRecordSets|2xx->2xx|$.ListResourceRecordSetsResponse.MaxItems
Issue-Key: route53|TestDNSAnswer|2xx->2xx|$.TestDNSAnswerResponse.Nameserver
Issue-Commit: e731816d9d754023c43f20a6999b56eec2c1047e

# Route 53 ListResourceRecordSets MaxItems and TestDNSAnswer Nameserver are emulator-invented values

> _Note: this issue was identified against the code base at commit `e731816d9d754023c43f20a6999b56eec2c1047e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResourceRecordSets

## Description
TWO net-new causes, one shared fault: on the record-set read paths the emulator reports a constant of its own choosing where AWS reports a value derived from the request or from the zone's own state.

Cause 1 of 2: ListResourceRecordSets echoes MaxItems as 300 when the request did not specify maxitems; AWS echoes the effective page size 100 (fixture route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted, step task#7; the cluster also covers task#11 and task#19 of the same fixture).
AWS: ListResourceRecordSets answers 200 with a required MaxItems member documented as 'The maximum number of records you requested'. maxitems is an optional query parameter; when it is omitted Route 53 applies its default page size and echoes THAT value, and the recording shows the echoed value is 100. The requirement is the rule, not the literal: MaxItems must be the request's maxitems when one was supplied, and the service's default page size (100 per the recording) when it was not - and it must agree with the page actually returned and with IsTruncated/NextRecordName. Smithy: ListResourceRecordSetsRequest.MaxItems -> Integer, httpQuery 'maxitems', optional; ListResourceRecordSetsResponse.MaxItems -> Integer, required.
Emulator: answers 200 and echoes MaxItems = 300 for the same request with no maxitems query parameter.
Why: a client that sizes its own paging loop, allocates buffers, or asserts 'did I get a full page?' from the echoed MaxItems computes against 300 while the emulator actually pages at a different size, so a truncated listing looks complete (count < MaxItems) and records are silently dropped. It also means a request that DID supply maxitems cannot be distinguished from one that did not.
Fix: the ListResourceRecordSets handler - resolve the effective page size once (request maxitems, else the service default) and echo exactly that resolved value in the response serializer, instead of a hard-coded 300.

Cause 2 of 2: TestDNSAnswer reports a fixed, synthetic Nameserver unrelated to the queried zone (fixture route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type, step task#6; the cluster also covers route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#12).
AWS: TestDNSAnswer answers 200 with a required Nameserver member documented as 'The Amazon Route 53 name server used to respond to the request' - i.e. the name server that actually served this query for this hosted zone, which is one of the zone's own delegated name servers as published in its DelegationSet.NameServers. The concrete host name is per-zone and per-environment, which is why the fixture asserts its SHAPE rather than a literal; the requirement is that the value be a real member of the queried zone's delegation set, not that it be any particular string. Smithy: TestDNSAnswerResponse.Nameserver -> Nameserver (string, @length max 255), required; doc as quoted.
Emulator: answers 200 with Nameserver = 'ns-999.awsdns-33.com', the same constant for every zone and every query, which fails the fixture's shape assertion because it is not drawn from the zone's delegation set.
Why: a client that correlates the answering name server with the zone's delegation set (to verify delegation, to pin a probe at a specific name server, or to attribute an answer to a name server) can never make the correlation: the reported name server is not one the zone was delegated to, so delegation-verification logic concludes the zone is mis-delegated.
Fix: the TestDNSAnswer handler - select the answering name server from the queried hosted zone's stored delegation set and serialize that, instead of a constant in the response builder.
Note: the open issue 'route53: 7 net-new causes - the authoritative DNS plane is unreachable because DelegationSet.NameServers advertises unresolvable AWS hostnames...' (20261117T145616Z-route53-7-net-new-causes-the-authoritative-dns.md) concerns resolving the advertised name servers; it does not cover TestDNSAnswer reporting a name server outside the zone's delegation set.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator, two mismatch clusters (status matched in both; only the body diverged):

1) ListResourceRecordSets, fixture route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#7, expected 200 / actual 200 (cluster also covers #task#11 and #task#19):
   $.ListResourceRecordSetsResponse.MaxItems: value_mismatch (expected '100', got '300')

2) TestDNSAnswer, fixture route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type#task#6, expected 200 / actual 200 (cluster also covers route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#12):
   $.TestDNSAnswerResponse.Nameserver: value_mismatch (expected '<shaped-not-asserted>', got 'ns-999.awsdns-33.com') - the fixture asserts the member's shape (a name server belonging to the queried zone's delegation set) rather than a literal host name, and the emulator's constant fails that assertion.

Model evidence: smithy_query on services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json - ListResourceRecordSetsRequest.MaxItems is an optional httpQuery 'maxitems' documented as '(Optional) The maximum number of resource records sets to include in the response body for this request'; ListResourceRecordSetsResponse.MaxItems is required, documented 'The maximum number of records you requested'; TestDNSAnswerResponse.Nameserver is required, targets Nameserver (string @length 0..255), documented 'The Amazon Route 53 name server used to respond to the request.'

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted step task#7 (also task#11, task#19) - GET /2013-04-01/hostedzone/{HostedZoneId}/rrset with NO maxitems query parameter - and read ListResourceRecordSetsResponse.MaxItems: the emulator echoes 300, AWS echoes its effective default page size 100. Repeat with an explicit maxitems to confirm the echoed value tracks the request.
3. Cause 2: replay fixture route53-record-ttl-zero-upsert-delete-by-trailing-dot-name-listed-by-name-and-type step task#6 (also route53-record-weighted-zero-weight-and-multivalue-set-identifiers-listed-and-deleted#task#12) - GET /2013-04-01/testdnsanswer?hostedzoneid=...&recordname=...&recordtype=... - and compare TestDNSAnswerResponse.Nameserver with the DelegationSet.NameServers that CreateHostedZone/GetHostedZone published for that zone: the emulator returns the constant 'ns-999.awsdns-33.com', which is not a member of that set.
4. Both steps must match the recorded AWS bodies before this issue is closed.
