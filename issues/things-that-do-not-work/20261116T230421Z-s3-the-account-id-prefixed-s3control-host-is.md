Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.s3-metrics-cfg-access-point-arn-filter-round-trips-alone-and-under-and
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.s3-metrics-cfg-access-point-arn-filter-round-trips-alone-and-under-and|run#5|EgressRefused|ap_arn = s3c.create_access_point(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# s3: the account-ID-prefixed s3control host is not served, so an access-point ARN filter check never reaches the emulator

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessPoint

## Description
Cause 1 of 1: every AccountId-bearing S3 Control operation is addressed at an account-ID-prefixed host, and the emulator serves no address for that host, so the request is refused before a byte is sent.

AWS: the S3 Control plane is reached at an account-prefixed authority. Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's3-control', sigv4Name 's3'): CreateAccessPoint, PUT /v20180820/accesspoint/{Name}, input member AccountId {httpHeader 'x-amz-account-id', hostLabel: true, pattern ^\d{12}$, required}, and the operation carries endpoint{hostPrefix: '{AccountId}.'}; CreateJob (POST /v20180820/jobs) carries the same hostPrefix. The SDK therefore prepends '<AccountId>.' to whatever endpoint authority it is configured with — '123456789012.s3-control.<region>.amazonaws.com' against AWS, and '<AccountId>.<host>' against any configured endpoint — and the service answers there (CreateAccessPoint 200 with AccessPointArn/Alias).

Emulator: the emulator is reached at 127.0.0.1 and offers no address for the prefixed host, so the call never leaves the runner: 'CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' / 'egress refused: 000000000000.127.0.0.1 is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on'. There is no status and no body — the AccountId-bearing s3control surface (CreateJob/DescribeJob/UpdateJobStatus, CreateAccessPoint and the access-point policy operations, TagResource/ListTagsForResource, PutPublicAccessBlock) is unreachable at the address the emulator announces.

Why: a client that uses S3 Control at all gets a name-resolution/connection failure instead of a response. Access points, S3 Batch Operations and s3control generic tagging are all addressed through the account-prefixed host, so an access-point ARN cannot even be minted, and every behaviour that hangs off one (a metrics configuration filtered by AccessPointArn, a batch job, an access-point policy) is unobservable.

Fix: the emulator's endpoint/host handling for the s3control service — the address it announces must also answer the '{AccountId}.' prefixed authority (serve the host alias / strip the leading 12-digit account label when routing), instead of only the bare endpoint host.

Related open issues, none of which covers this (they all describe responses of s3control calls that DID reach the emulator, so they presuppose reachability): 20261022T213144Z-s3control-access-points-directory-bucket-listing-job-object.md, 20261106T121347Z-s3control-access-point-reads-diverge-authorization-and-existence.md, 20261110T084205Z-s3control-getaccesspoint-success-bodies-diverge-endpoints-map-order.md.

Traces: s3.s3-metrics-cfg-access-point-arn-filter-round-trips-alone-and-under-and (the check's first s3control call, CreateAccessPoint(AccountId=<account>, Name=<ap>, Bucket=<bucket>), is refused at the prefixed host).

## Evidence
s3.s3-metrics-cfg-access-point-arn-filter-round-trips-alone-and-under-and ->
Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
  File 'egress.py', line 371, in resolve / refuse(...)
  simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on
Expected: CreateAccessPoint answers 200 with AccessPointArn == arn:aws:s3:<region>:<account>:accesspoint/<name>, after which PutBucketMetricsConfiguration round-trips Filter.AccessPointArn alone and inside And. Actual: no HTTP exchange at all — the account-prefixed authority the SDK builds from the operation's endpoint hostPrefix is not served.

## Steps to Reproduce
1. Replay the behavioral suite and run s3.s3-metrics-cfg-access-point-arn-filter-round-trips-alone-and-under-and.
2. Minimal reproduction: with an SDK pointed at the emulator, call s3control CreateAccessPoint(AccountId='<12-digit account>', Name='<ap-name>', Bucket='<bucket>'). The SDK sends it to '<AccountId>.<emulator-host>' (per the operation's endpoint hostPrefix '{AccountId}.'); the emulator serves no address for that host and the request is refused before transmission.
3. The same happens for CreateJob/DescribeJob and the access-point policy and tagging operations, all of which carry the same hostPrefix.
