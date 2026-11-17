Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain route53.route53-cname-answer-carries-the-chased-in-zone-target route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name route53.route53-alias-evaluate-target-health-withholds-the-alias-answer route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state route53.route53-second-primary-failover-record-in-the-same-group-rejected route53.calculated-health-check-threshold-decides-the-failover-answer route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver route53.route53-traffic-policy-instance-records-are-served-and-withdrawn route53.calculated-health-check-duplicate-child-ids-rejected route53.change-cidr-collection-block-already-in-use-rejected route53.create-reusable-delegation-set-duplicate-caller-reference-rejected route53.create-traffic-policy-version-unknown-policy-rejected route53.list-resource-record-sets-start-record-type-without-name-rejected route53.test-dns-answer-private-zone-rejected
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve|_nameserver_ips#5|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-cname-answer-carries-the-chased-in-zone-target|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-alias-evaluate-target-health-withholds-the-alias-answer|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-second-primary-failover-record-in-the-same-group-rejected|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.calculated-health-check-threshold-decides-the-failover-answer|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.route53-traffic-policy-instance-records-are-served-and-withdrawn|_nameserver_ips#4|AssertionError|assert entry["addresses"], (
Issue-Key: route53.calculated-health-check-duplicate-child-ids-rejected|_expect_error#4|AssertionError|raise AssertionError(f"expected the create to be refused, but it succeeded: {result!r}")
Issue-Key: route53.change-cidr-collection-block-already-in-use-rejected|run#12|AssertionError|assert err.get("Message") == "", err
Issue-Key: route53.create-reusable-delegation-set-duplicate-caller-reference-rejected|run#12|AssertionError|assert err.get("Message") == (
Issue-Key: route53.create-traffic-policy-version-unknown-policy-rejected|run#14|AssertionError|assert err.get("Message") == "", err
Issue-Key: route53.list-resource-record-sets-start-record-type-without-name-rejected|run#9|AssertionError|assert got == _ERR, f"expected {_ERR}, got {got}"
Issue-Key: route53.test-dns-answer-private-zone-rejected|run#19|AssertionError|assert msg == expected_message, f"expected message {expected_message!r}, got {msg!r}"
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# route53: 7 net-new causes — the authoritative DNS plane is unreachable because DelegationSet.NameServers advertises unresolvable AWS hostnames, plus six refusal defects (duplicate CALCULATED children accepted; four emulator-authored refusal Messages)

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateHostedZone

## Description
Seven net-new root causes in the route53 emulator, all grounded in failing behavioral checks of this suite. Cause 1 blocks every DNS data-plane check; causes 2-7 are control-plane refusal defects.

=== Cause 1 of 7: the name servers a hosted zone delegates to are AWS hostnames that resolve to nothing in the environment, so no query can ever reach the emulator's authoritative server. ===
AWS: Smithy: CreateHostedZone/GetHostedZone output member `DelegationSet` -> `DelegationSet.NameServers` (list of `DNSName`). The wire/DNS behavior itself is not modeled, so: recorded behavior only — the four hostnames Route 53 reports (form `ns-<n>.awsdns-<n>.<tld>`) are the zone's authoritative servers, they RESOLVE to addresses, and those addresses answer queries for names in the zone authoritatively (AA set), and NXDOMAIN for a name not in the zone. Every check here first reads the delegation set, resolves each advertised host, and only then queries it — exactly what a client following the delegation does.
Emulator: the advertised hostnames do not resolve at all from the vantage point the suite queries from. Each check fails resolving the FIRST advertised name server, verbatim: `AssertionError: the delegated nameserver ns-1819.awsdns-36.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}` (the host label varies per zone: ns-301.awsdns-46.com, ns-783.awsdns-40.com, ns-1736.awsdns-10.com, ns-867.awsdns-52.com, ns-603.awsdns-62.com, ns-1631.awsdns-34.com, ns-52.awsdns-03.com, ns-948.awsdns-52.com, ns-1949.awsdns-19.com, ns-788.awsdns-37.com, ns-1432.awsdns-60.com, ns-791.awsdns-16.com). The preceding control-plane calls (CreateHostedZone, ChangeResourceRecordSets, GetChange) all succeeded, so the zone and its records exist; what is missing is that the emulator announces a delegation set it is not reachable on. The generated label is a per-run value; the requirement is that whatever `DelegationSet.NameServers` publishes must be resolvable and must answer on the host the emulator is actually reached on.
Why: a client that does what Route 53 documents — take the delegation set and query it — can never resolve a name the emulator claims authority for. Every routing-policy behavior the service exists to provide (failover, weighted, latency, multivalue, geo, alias/in-zone alias, CNAME chasing, traffic-policy instances, NXDOMAIN for absent names, INSYNC meaning "actually served") is unobservable, and so is anything layered on a hosted zone.
Fix: the delegation-set assignment in the route53 state/CreateHostedZone handler together with the DNS responder that serves hosted zones — the names published in `DelegationSet.NameServers` must be resolvable in the environment and must be answered on by that responder (UDP and TCP/53). Related but NOT covering this: open issue 20261117T134237Z-acm-cloudfront-iam-lambda-route53-s3-a-route-53-public-hosted-zone-s-delegated reports that the advertised servers never ANSWER over TCP; the 13 checks here never get that far — the hostname does not resolve, so nothing is ever contacted.
Traces: route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve, route53.route53-insync-change-is-actually-served-by-every-delegated-nameserver, route53.route53-query-for-a-name-absent-from-the-zone-rejected-with-nxdomain, route53.route53-cname-answer-carries-the-chased-in-zone-target, route53.route53-in-zone-alias-answers-the-targets-address-under-the-aliased-name, route53.route53-alias-evaluate-target-health-withholds-the-alias-answer, route53.route53-failover-answer-flips-when-an-inverted-health-check-changes-state, route53.route53-second-primary-failover-record-in-the-same-group-rejected, route53.calculated-health-check-threshold-decides-the-failover-answer, route53.route53-latency-routing-selects-the-sibling-for-the-queriers-region, route53.route53-multivalue-answer-returns-a-rotating-subset-of-the-healthy-records, route53.route53-zero-weight-sibling-answer-rejected-at-the-authoritative-nameserver, route53.route53-traffic-policy-instance-records-are-served-and-withdrawn

=== Cause 2 of 7: CreateHealthCheck accepts a CALCULATED health check that lists the same child health check id twice. ===
AWS: Smithy: CreateHealthCheck binds `InvalidInput` (client, httpError 400); `HealthCheckConfig.ChildHealthChecks` is a list of ChildHealthCheck (length 0..256) and the model states no uniqueness trait, so: recorded behavior only — a CALCULATED config whose ChildHealthChecks repeats an id is refused 400 `InvalidInput` with Message `Calculated health checks must not have duplicate child health check IDs.` That text is entirely fixed: it names no id and has no variable part.
Emulator: the call SUCCEEDS — HTTP 201 with `HealthCheck.HealthCheckConfig.ChildHealthChecks = ['6d0ddc1c-...', '6d0ddc1c-...']` (the same id twice) and `HealthCheckVersion: 1`, and the health check is created and listed.
Why: a caller that relies on Route 53 to reject a malformed aggregation gets a silently-accepted health check whose threshold arithmetic double-counts one child, so a calculated check reports healthy on evidence from a single endpoint. Negative-path tests that assert the refusal pass against AWS and fail here.
Fix: input validation in the CreateHealthCheck handler (the CALCULATED branch of HealthCheckConfig validation) — reject a ChildHealthChecks list containing a repeated id with InvalidInput before any state is written.
Traces: route53.calculated-health-check-duplicate-child-ids-rejected

=== Cause 3 of 7: ChangeCidrCollection's CidrBlockInUseException carries a Message AWS does not send. ===
AWS: Smithy: ChangeCidrCollection binds `CidrBlockInUseException` (client, httpError 400) with a single member `Message`; the shape doc "This CIDR block is already in use." is the RULE, not the wire sentence. Recorded behavior: the refusal arrives with Type=Sender, Code=CidrBlockInUseException and NO Message element (botocore normalizes the absent child to '').
Emulator: `{'Message': 'This CIDR block is already in use.', 'Code': 'CidrBlockInUseException', 'Type': 'Sender'}` — the shape's documentation string is emitted as the wire Message.
Why: a client that logs or string-matches Error.Message sees a sentence AWS never sent; any caller normalizing on the absent-Message shape diverges.
Fix: the CidrBlockInUseException constructor used by the ChangeCidrCollection handler (route53 state's refusal constructors) — do not populate `Message` from the shape doc for this error class.
Traces: route53.change-cidr-collection-block-already-in-use-rejected

=== Cause 4 of 7: CreateReusableDelegationSet's DelegationSetAlreadyCreated Message is the shape's documentation instead of AWS's sentence. ===
AWS: Smithy: CreateReusableDelegationSet binds `DelegationSetAlreadyCreated` (client, httpError 400 defaulted) with member `message`; its shape doc is "A delegation set with the same owner and caller reference combination has already been created." — again the rule, not the wire text. Recorded behavior: Code=DelegationSetAlreadyCreated, Type=Sender, Message = `A delegation set has already been created with the specified caller reference.` — entirely fixed text, with no variable part (it never echoes the CallerReference).
Emulator: Code and Type match, but Message is `A delegation set with the same owner and caller reference combination has already been created.` (the doc string).
Why: the Message is the human-readable half of the refusal; a client that asserts or matches AWS's sentence fails even though the code is right.
Fix: the DelegationSetAlreadyCreated constructor behind the CreateReusableDelegationSet handler — word the message as AWS does rather than from the model's documentation.
Traces: route53.create-reusable-delegation-set-duplicate-caller-reference-rejected

=== Cause 5 of 7: CreateTrafficPolicyVersion on an id that was never created answers NoSuchTrafficPolicy WITH a Message, where the recording has none. ===
AWS: Smithy: CreateTrafficPolicyVersion binds `NoSuchTrafficPolicy` (client, httpError 404) with member `message`. Recorded behavior for this condition — an `Id` no traffic policy ever held (a fresh UUID) — is Type=Sender, Code=NoSuchTrafficPolicy and NO Message element.
Emulator: `{'Message': 'The policy does not exist.', 'Code': 'NoSuchTrafficPolicy', 'Type': 'Sender'}`.
Why: the same error shape is worded differently per condition in AWS, and a client that branches on the presence of a reason text sees one where the recording has none. NOTE the tension a fixer must resolve rather than flip blindly: open issues 20261109T063005Z-route53-route-53-nosuchtrafficpolicy-is-answered-with-an-empty and 20261109T074659Z-route53-route-53-nosuchtrafficpolicy-refusal-is-serialized-without-an record the opposite direction for GetTrafficPolicy/UpdateTrafficPolicyComment on an existing policy with a non-existent VERSION (Message = "The policy does not exist."). The fix must make the Message conditional on the path/condition, not shared unconditionally by every NoSuchTrafficPolicy raise site.
Fix: the NoSuchTrafficPolicy raise site in the CreateTrafficPolicyVersion handler (not the shared messageless/with-message constructor used by the version paths).
Traces: route53.create-traffic-policy-version-unknown-policy-rejected

=== Cause 6 of 7: ListResourceRecordSets' InvalidInput for StartRecordType without StartRecordName carries an emulator-authored Message. ===
AWS: Smithy: ListResourceRecordSets binds `InvalidInput` (client, httpError 400); inputs `StartRecordName` (httpQuery name) and `StartRecordType` (httpQuery type). Recorded behavior: specifying `type` without `name` is refused with Code=InvalidInput and NO Message element.
Emulator: Code=InvalidInput is right, but Message is `StartRecordType was specified without StartRecordName; type only sorts between records with the same record name` — prose the service does not send.
Why: the refusal body does not match AWS, so a client (or test) that compares the error pair sees a mismatch on a path whose code is correct.
Fix: the InvalidInput raise site in the ListResourceRecordSets handler's parameter validation — refuse without authoring a Message.
Traces: route53.list-resource-record-sets-start-record-type-without-name-rejected

=== Cause 7 of 7: TestDNSAnswer's private-hosted-zone refusal message is emulator-authored. ===
AWS: Smithy: TestDNSAnswer binds `InvalidInput` (client, httpError 400) and `NoSuchHostedZone` (404); the private-zone restriction is not in the model, so: recorded behavior only — TestDNSAnswer against a PRIVATE hosted zone is refused with Message `Cannot send DNS query to a Private Hosted Zone.` That sentence is entirely fixed text: it does not name the zone id.
Emulator: the message is `The hosted zone <zone id> is a private hosted zone; TestDNSAnswer only supports querying public hosted zones` — different wording, and it interpolates the zone id AWS does not include (the id itself is a per-run value; the defect is the template).
Why: callers that surface or match the refusal reason see different text; the extra interpolated id makes the message unmatchable even by prefix.
Fix: the private-zone guard in the TestDNSAnswer handler — emit AWS's fixed sentence.
Traces: route53.test-dns-answer-private-zone-rejected

## Evidence
Behavioral suite simparity-behavioral-aws-v1, route53: 20 failing checks, 7 root causes.

Cause 1 (13 checks) — all fail identically while resolving the FIRST host listed in the zone's DelegationSet.NameServers, e.g. route53.route53-authoritative-nameserver-answers-the-record-it-was-told-to-serve:
  AssertionError: the delegated nameserver ns-1819.awsdns-36.com did not resolve from the vantage point: {'addresses': [], 'errno': -2, 'error': 'gaierror: [Errno -2] Name or service not known'}
Expected: every name published in DelegationSet.NameServers resolves and answers authoritatively for names in the zone (and NXDOMAIN outside it). Actual: name resolution of the advertised host fails, so no query is ever sent.

Cause 2 — route53.calculated-health-check-duplicate-child-ids-rejected:
  Expected: ('InvalidInput', 'Calculated health checks must not have duplicate child health check IDs.')
  Actual: HTTP 201 and a created health check: {'HealthCheckConfig': {'Type': 'CALCULATED', 'HealthThreshold': 0, 'ChildHealthChecks': ['6d0ddc1c-64b7-2891-4f74-656711a64280', '6d0ddc1c-64b7-2891-4f74-656711a64280']}, 'HealthCheckVersion': 1}

Cause 3 — route53.change-cidr-collection-block-already-in-use-rejected:
  Expected: Code=CidrBlockInUseException, Type=Sender, no Message element (botocore '')
  Actual: {'Message': 'This CIDR block is already in use.', 'Code': 'CidrBlockInUseException', 'Type': 'Sender'}

Cause 4 — route53.create-reusable-delegation-set-duplicate-caller-reference-rejected:
  Expected: Message == 'A delegation set has already been created with the specified caller reference.'
  Actual: {'Message': 'A delegation set with the same owner and caller reference combination has already been created.', 'Code': 'DelegationSetAlreadyCreated', 'Type': 'Sender'}

Cause 5 — route53.create-traffic-policy-version-unknown-policy-rejected (Id = a fresh uuid4):
  Expected: Code=NoSuchTrafficPolicy with no Message element
  Actual: {'Message': 'The policy does not exist.', 'Code': 'NoSuchTrafficPolicy', 'Type': 'Sender'}

Cause 6 — route53.list-resource-record-sets-start-record-type-without-name-rejected:
  Expected: ('InvalidInput', '')
  Actual: ('InvalidInput', 'StartRecordType was specified without StartRecordName; type only sorts between records with the same record name')

Cause 7 — route53.test-dns-answer-private-zone-rejected:
  Expected message: 'Cannot send DNS query to a Private Hosted Zone.'
  Actual message: 'The hosted zone ZEG23J27DRI08SN is a private hosted zone; TestDNSAnswer only supports querying public hosted zones'

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Cause 1: route53 CreateHostedZone a public zone; read DelegationSet.NameServers; from a client in the environment resolve each advertised host name and send it a DNS query for a record created via ChangeResourceRecordSets. Observe gaierror (name does not resolve) instead of an authoritative answer. Replay any of the 13 listed checks.
3. Cause 2: create two HTTP health checks; CreateHealthCheck(HealthCheckConfig={'Type':'CALCULATED','HealthThreshold':0,'ChildHealthChecks':[leaf_a, leaf_a]}). Observe 201 instead of 400 InvalidInput.
4. Cause 3: CreateCidrCollection, add a CIDR block, then ChangeCidrCollection putting the same block again; inspect response['Error'] — a Message is present.
5. Cause 4: CreateReusableDelegationSet(CallerReference=X) twice; inspect Error.Message.
6. Cause 5: CreateTrafficPolicyVersion(Id=str(uuid.uuid4()), Document=<valid doc>); inspect Error — Message is present.
7. Cause 6: ListResourceRecordSets(HostedZoneId=<zone>, StartRecordType='A') with no StartRecordName; inspect Error.Message.
8. Cause 7: CreateHostedZone with a VPC (private), then TestDNSAnswer(HostedZoneId=<private zone>, RecordName=..., RecordType='A'); inspect Error.Message.
