Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3control.access-grants-get-data-access-returns-scoped-credentials s3control.access-grants-get-data-access-ungranted-prefix-rejected s3control.access-grants-location-and-resource-policy-lifecycle s3control.access-grants-operations-require-instance-rejected s3control.access-point-alias-and-endpoints-survive-policy-churn s3control.access-point-delete-severs-data-path-and-keeps-objects s3control.access-point-list-scopes-to-requested-bucket s3control.access-point-name-collision-rejected s3control.access-point-on-nonexistent-bucket-rejected s3control.access-point-policy-status-derives-ispublic s3control.access-point-prefix-scoped-policy-denies-object-outside-prefix-rejected s3control.access-point-public-policy-blocked-by-own-block-public-policy-rejected s3control.access-point-scope-does-not-answer-for-general-purpose-access-point-rejected s3control.access-point-vpc-origin-denies-internet-read-rejected s3control.account-public-access-block-blocks-public-access-point-policy-rejected s3control.account-public-access-block-blocks-public-bucket-policy-rejected s3control.batch-job-completion-report-accounts-for-mixed-task-outcomes s3control.batch-job-describe-and-tagging-bogus-id-rejected s3control.batch-job-lifecycle-priority-and-cancel s3control.batch-job-runs-to-completion-and-tags-objects s3control.batch-job-tagging-lifecycle-independent-of-job s3control.bucket-policy-delegating-to-access-point-denies-direct-path-rejected s3control.multi-region-access-point-create-and-delete-are-async-operations s3control.multi-region-access-point-duplicate-name-create-races-rejected s3control.public-access-block-absence-is-distinct-from-all-false s3control.s3c-access-point-policy-explicit-deny-overrides-bucket-allow-rejected s3control.s3c-batch-copy-job-copies-manifest-objects-and-leaves-the-rest s3control.s3c-batch-generated-manifest-filter-limits-job-to-matching-objects s3control.storage-lens-configuration-put-replaces-whole-document s3control.storage-lens-configuration-roundtrip-and-tagging-independent s3control.storage-lens-group-update-and-arn-tagging-roundtrip s3control.tag-resource-upserts-existing-bucket-tags s3control.untag-resource-removes-specific-bucket-tag-keys
Issue-Shape-Id: com.amazonaws.s3control#AWSS3ControlServiceV20180820
Issue-Key: s3control.access-grants-get-data-access-returns-scoped-credentials|_ensure_instance#13|EgressRefused|s3c.get_access_grants_instance(AccountId=account)
Issue-Key: s3control.access-grants-get-data-access-ungranted-prefix-rejected|_ensure_instance#13|EgressRefused|s3c.get_access_grants_instance(AccountId=account)
Issue-Key: s3control.access-grants-location-and-resource-policy-lifecycle|_ensure_instance#13|EgressRefused|s3c.get_access_grants_instance(AccountId=account)
Issue-Key: s3control.access-grants-operations-require-instance-rejected|run#3|EgressRefused|s3c.get_access_grants_instance(AccountId=account)
Issue-Key: s3control.access-point-alias-and-endpoints-survive-policy-churn|run#8|EgressRefused|created = s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)
Issue-Key: s3control.access-point-delete-severs-data-path-and-keeps-objects|run#9|EgressRefused|ap_arn = s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)[
Issue-Key: s3control.access-point-list-scopes-to-requested-bucket|run#13|EgressRefused|s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)
Issue-Key: s3control.access-point-name-collision-rejected|run#12|EgressRefused|created = s3c.create_access_point(AccountId=account, Name=name, Bucket=first_bucket)
Issue-Key: s3control.access-point-on-nonexistent-bucket-rejected|<lambda>#9|EgressRefused|lambda: s3c.create_access_point(AccountId=account, Name=name, Bucket=ghost)
Issue-Key: s3control.access-point-policy-status-derives-ispublic|run#10|EgressRefused|created = s3c.create_access_point(
Issue-Key: s3control.access-point-prefix-scoped-policy-denies-object-outside-prefix-rejected|run#11|EgressRefused|ap_arn = s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)[
Issue-Key: s3control.access-point-public-policy-blocked-by-own-block-public-policy-rejected|run#9|EgressRefused|blocked_arn = s3c.create_access_point(AccountId=account, Name=blocked, Bucket=bucket)[
Issue-Key: s3control.access-point-scope-does-not-answer-for-general-purpose-access-point-rejected|run#11|EgressRefused|ap_arn = s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)[
Issue-Key: s3control.access-point-vpc-origin-denies-internet-read-rejected|run#15|EgressRefused|vpc_arn = s3c.create_access_point(
Issue-Key: s3control.account-public-access-block-blocks-public-access-point-policy-rejected|run#9|EgressRefused|ap_arn = s3c.create_access_point(
Issue-Key: s3control.account-public-access-block-blocks-public-bucket-policy-rejected|_read_account_pab#14|EgressRefused|return s3c.get_public_access_block(AccountId=account)["PublicAccessBlockConfiguration"]
Issue-Key: s3control.batch-job-completion-report-accounts-for-mixed-task-outcomes|_create_job#18|EgressRefused|return s3c.create_job(
Issue-Key: s3control.batch-job-describe-and-tagging-bogus-id-rejected|<lambda>#9|EgressRefused|"DescribeJob": lambda jid: s3c.describe_job(AccountId=account, JobId=jid),
Issue-Key: s3control.batch-job-lifecycle-priority-and-cancel|run#14|EgressRefused|job_id = s3c.create_job(
Issue-Key: s3control.batch-job-runs-to-completion-and-tags-objects|run#16|EgressRefused|job_id = s3c.create_job(
Issue-Key: s3control.batch-job-tagging-lifecycle-independent-of-job|run#14|EgressRefused|job_id = s3c.create_job(
Issue-Key: s3control.bucket-policy-delegating-to-access-point-denies-direct-path-rejected|run#11|EgressRefused|ap_arn = s3c.create_access_point(AccountId=account, Name=name, Bucket=bucket)[
Issue-Key: s3control.multi-region-access-point-create-and-delete-are-async-operations|run#12|EgressRefused|created = s3c.create_multi_region_access_point(
Issue-Key: s3control.multi-region-access-point-duplicate-name-create-races-rejected|_create#17|EgressRefused|return s3c.create_multi_region_access_point(
Issue-Key: s3control.public-access-block-absence-is-distinct-from-all-false|run#4|EgressRefused|original = s3c.get_public_access_block(AccountId=account)[
Issue-Key: s3control.s3c-access-point-policy-explicit-deny-overrides-bucket-allow-rejected|run#13|EgressRefused|ap_arn = s3c.create_access_point(AccountId=account, Name=ap, Bucket=bucket)["AccessPointArn"]
Issue-Key: s3control.s3c-batch-copy-job-copies-manifest-objects-and-leaves-the-rest|_create_job_surviving_role_propagation#19|EgressRefused|job_id = s3c.create_job(**params)["JobId"]
Issue-Key: s3control.s3c-batch-generated-manifest-filter-limits-job-to-matching-objects|_create_job_surviving_role_propagation#15|EgressRefused|job_id = s3c.create_job(**params)["JobId"]
Issue-Key: s3control.storage-lens-configuration-put-replaces-whole-document|run#7|EgressRefused|s3c.put_storage_lens_configuration(
Issue-Key: s3control.storage-lens-configuration-roundtrip-and-tagging-independent|run#5|EgressRefused|s3c.put_storage_lens_configuration(
Issue-Key: s3control.storage-lens-group-update-and-arn-tagging-roundtrip|run#4|EgressRefused|s3c.create_storage_lens_group(
Issue-Key: s3control.tag-resource-upserts-existing-bucket-tags|_tags_dict#9|EgressRefused|resp = s3c.list_tags_for_resource(AccountId=account, ResourceArn=arn)
Issue-Key: s3control.untag-resource-removes-specific-bucket-tag-keys|run#8|EgressRefused|s3c.tag_resource(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# s3control: the account-id host prefix every operation requires ({AccountId}.<endpoint>) is not served, so no s3control call is ever delivered (1 cause)

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPoint

## Description
Every s3control behavioral check in this replay fails before a single s3control operation reaches the emulator, all for one reason. This bundle has 1 cause; it must be closed.

Cause 1 of 1: the emulator serves/announces no address for the account-id-prefixed s3control host that the API model mandates, so the SDK's compliant request cannot be delivered at all.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml, endpointPrefix 's s3-control', sigv4Name 's3'): EVERY account-scoped operation carries @endpoint(hostPrefix: "{AccountId}.") and binds AccountId as both @httpHeader("x-amz-account-id") and @hostLabel (target AccountId, pattern ^\d{12}$) — e.g. GetAccessPoint (GET /v20180820/accesspoint/{Name}, endpoint.hostPrefix "{AccountId}.") and PutPublicAccessBlock (PUT /v20180820/configuration/publicAccessBlock, endpoint.hostPrefix "{AccountId}."). An AWS SDK therefore prepends the caller's 12-digit account id and a dot to the resolved endpoint authority for every such call: against real AWS the request goes to <account-id>.s3-control.<region>.amazonaws.com, and against a custom endpoint authority it goes to <account-id>.<that authority>. The account id is fixed text only in its position; its value comes from the request (the AccountId member, here the account the harness's STS GetCallerIdentity reports).

Emulator: the emulator offers only the bare endpoint authority it is reached on. When the SDK applies the modeled host prefix, the resulting host is refused before any HTTP request is made: 'CapabilityNotOffered: the target serves no address for <account-id>.<endpoint-host>' / simparity.egress.EgressRefused: "egress refused: '<account-id>.<endpoint-host>' is not the endpoint under test (<endpoint-host>); an emulator run may reach only the emulator". No status, no error code and no body is produced, because the emulator announces no address for the host its own API contract requires clients to use. (In this run the host was 000000000000.127.0.0.1 — the account id and the loopback authority are per-environment values; the requirement is that <AccountId>.<served authority> is served, not those literals.)

Why: every SDK/CLI client of s3control is unusable against the emulator. boto3, the AWS CLI and all other SDKs apply the modeled hostPrefix unconditionally, so a client gets a name-resolution/connection failure instead of a response for AccessPoint, Access Grants, Batch Operations, Storage Lens, PublicAccessBlock and tagging calls alike. Note the emulator's s3control handlers themselves answer when a request is addressed directly to the bare authority (the wire corpus reaches them), so this is purely the front door: every behavioral check of the service is unreachable and the whole service reads as broken to any real client.

Fix: the emulator's HTTP front door — the set of hostnames/authorities it announces it serves and its host-based request dispatch — must accept and route the modeled '{AccountId}.' host prefix for the s3control endpoint (and strip/validate it against the x-amz-account-id header and the AccountId @hostLabel pattern ^\d{12}$) so a host-prefixed request lands on the same s3control handlers as a bare-authority request. No existing open issue covers this: the open s3control issues (20261106T121347Z-s3control-access-point-reads-diverge-authorization-and-existence.md, 20261106T121347Z-s3control-put-operations-diverge-access-point-policy-validation.md, 20261110T084205Z-s3control-getaccesspoint-success-bodies-diverge-endpoints-map-order.md, 20261029T192817Z-s3control-batch-operations-jobs-never-settle-and-refuse.md, 20261113T223008Z-s3control-success-response-members-diverge-from-recorded-aws.md) all describe response-body/enforcement divergences reached over the bare authority and none of them addresses host-prefix reachability.

Traces: all 33 unowned s3control checks in this replay fail with the identical egress refusal on <account-id>.<endpoint-host>; they are listed in trace_ids. (The two further s3control failures, account-public-access-block-restrict-denies-anonymous-object-read-rejected and directory-bucket-access-point-listing-excludes-general-purpose, are already owned by open issues and are not re-filed here.)

## Evidence
Behavioral suite: 0/44 passed (0.0%); s3control 0/35. Every one of the 33 unowned s3control checks reports the same pre-request failure:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    File "egress.py", line 371, in resolve -> refuse(...)
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: the SDK call reaches the emulator. Per the Smithy model, botocore resolves the host for every s3control operation as '<AccountId>.' + the endpoint authority (GetAccessPoint and PutPublicAccessBlock both carry endpoint.hostPrefix "{AccountId}." and AccountId is @hostLabel with pattern ^\d{12}$), and AWS answers that host (e.g. <account-id>.s3-control.<region>.amazonaws.com) with the operation's modeled 200 body or its modeled refusal.

Actual: nothing is served on that host, so the call fails at address resolution — no HTTP status, no error code, no body. The account id (000000000000) and authority (127.0.0.1) in the message are per-run values; the fixed part of the requirement is that the authority prefixed with '<12-digit account id>.' must be served.

That the handlers exist is shown by the wire corpus, which reaches s3control operations over the bare authority (see 20261113T223008Z-s3control-success-response-members-diverge-from-recorded-aws.md); only the host-prefixed address is unserved.

## Steps to Reproduce
1. Clone the repo and boot the emulator; confirm the s3control endpoint answers on the bare authority (e.g. GET /v20180820/configuration/publicAccessBlock with header x-amz-account-id: <12-digit account id>).
2. With boto3, create a client for s3control against the emulator's endpoint and call any account-scoped operation, e.g. s3control.get_access_point(AccountId=<account id>, Name='any') or s3control.put_public_access_block(AccountId=<account id>, PublicAccessBlockConfiguration={...}).
3. Observe that botocore applies the modeled @endpoint(hostPrefix "{AccountId}.") and addresses '<account id>.<emulator authority>'; the request never reaches the emulator — under the harness it is refused with 'CapabilityNotOffered: the target serves no address for <account id>.<authority>'.
4. After the fix, the same call must be routed to the s3control handlers (the host prefix accepted, validated against x-amz-account-id and the ^\d{12}$ label pattern) and answer the operation's modeled response.
5. Re-replay every check id listed in trace_ids to confirm the service is reachable again.
