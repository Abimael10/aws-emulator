Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: transfer.connector-config-exclusivity-and-described-shape
Issue-Shape-Id: com.amazonaws.transfer#TransferService
Issue-Key: transfer.connector-config-exclusivity-and-described-shape|_err#19|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: cd7717ef55dd858ee594fa09dd5c2a3d5ba64567

# Transfer CreateConnector does not validate the connector Url: an unresolvable host (RFC 2606 .invalid TLD) is accepted with HTTP 200 instead of 400 InvalidRequestException "The specified Url is not valid"

> _Note: this issue was identified against the code base at commit `cd7717ef55dd858ee594fa09dd5c2a3d5ba64567`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConnector

## Description
One net-new root cause in the transfer emulator, grounded in a failing behavioral check of this suite.

=== Cause 1 of 1: CreateConnector accepts a Url whose host cannot resolve ===

AWS: Smithy CreateConnector.Url -> target Url, @length 0..255, with NO @pattern — so the URL rule is service-side, not wire-side; CreateConnector models InvalidRequestException (errorKind client, httpError 400). Recorded behavior only: real Transfer validates the Url beyond its length and refuses a syntactically well-formed https URL whose host is unresolvable (a host under the RFC 2606 reserved `.invalid` TLD) with HTTP 400, error code `InvalidRequestException` and the message `The specified Url is not valid`. That message is entirely fixed text for this error class — it carries no request- or state-derived values at all. The recording also shows the exclusivity checks (both As2Config and SftpConfig present / neither present) are evaluated BEFORE the Url check, so this leg sends exactly one config, and that a refused CreateConnector creates nothing: the connector must not appear in ListConnectors afterwards.

Emulator: CreateConnector(Url='https://<unique-host>.invalid/as2', AccessRole=<role arn>, As2Config={LocalProfileId, PartnerProfileId, MdnResponse, Compression, EncryptionAlgorithm, SigningAlgorithm}) returns HTTP 200 with a ConnectorId (observed `{'ConnectorId': 'c-00000000000000003'}`, content-type application/x-amz-json-1.1) — the Url is never validated, so the call is accepted and a connector is persisted where AWS refuses with 400. The check fails with "expected the call to be rejected, but it succeeded". Note the two exclusivity legs that precede it (both configs -> 'Only one of As2Config or SftpConfig must be specified'; neither -> 'One of As2Config or SftpConfig must be specified') DID pass, so the exclusivity rules are correct and only the Url rule is missing. Because the call is accepted, the connector the request should never have produced also leaks into ListConnectors, which the check asserts immediately after the rejection legs.

Why: a client that relies on the service to reject an unroutable connector endpoint provisions a connector that can never complete a transfer, and gets no signal at provisioning time where AWS hands it a 400 it can act on. Negative/least-surprise tests that assert the refusal pass against AWS and fail against the emulator, and the emulator's resource inventory diverges from what AWS would have allowed (an extra connector exists).

Fix: Url validation in the CreateConnector handler of the transfer service crate (the same rule belongs on UpdateConnector), raising the modeled InvalidRequestException with the fixed message `The specified Url is not valid`, ordered AFTER the As2Config/SftpConfig exclusivity checks and BEFORE any state mutation so nothing is persisted on refusal.

Related but NOT covering this: open issue 20261022T202037Z-transfer-input-validation-authorization-and-reference-data-diverge.md Cause 1 covers a Url with NO SCHEME at all ('ftp.example.com') on an SFTP connector; this check's Url is scheme-complete and well-formed and is refused by AWS for its unresolvable host, and the suite does not report this check as owned by that issue.

Traces: transfer.connector-config-exclusivity-and-described-shape (the check aborts at the Url leg, so the later legs of that check — the individually-required As2Config.Compression/EncryptionAlgorithm/SigningAlgorithm diagnostics and the DescribedConnector shape assertions — were never exercised and are not claimed here; re-replaying this check after the fix will reveal whether they hold).

## Evidence
Cause 1 — expected: CreateConnector(Url='https://<host>.invalid/as2', AccessRole=<role arn>, As2Config={'LocalProfileId': <LOCAL profile>, 'PartnerProfileId': <PARTNER profile>, 'MdnResponse': 'NONE', 'Compression': 'DISABLED', 'EncryptionAlgorithm': 'NONE', 'SigningAlgorithm': 'NONE'}) -> HTTP 400, Error.Code 'InvalidRequestException', Error.Message 'The specified Url is not valid' (fixed text, no variable parts), and no connector created (the Url does not appear in ListConnectors).
actual: HTTP 200 with body {'ConnectorId': 'c-00000000000000003'} (content-type application/x-amz-json-1.1); the connector is created. Harness assertion: AssertionError: expected the call to be rejected, but it succeeded: {'ConnectorId': 'c-00000000000000003', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}.
The generated id, host name, profile ids and timestamp above are per-run values from this replay, not part of the requirement; the status, error code and the fixed message text are.
Passing context from the same check (not a divergence): CreateConnector with BOTH As2Config and SftpConfig -> InvalidRequestException 'Only one of As2Config or SftpConfig must be specified'; with NEITHER -> InvalidRequestException 'One of As2Config or SftpConfig must be specified'.

## Steps to Reproduce
1. Boot the emulator and point botocore at it (client 'transfer').
2. tf.create_profile(As2Id=<unique>, ProfileType='LOCAL') and tf.create_profile(As2Id=<unique>, ProfileType='PARTNER'); note both ProfileIds.
3. iam.create_role(RoleName=<unique>, AssumeRolePolicyDocument=<trust policy for transfer.amazonaws.com>) and note the role ARN.
4. Call tf.create_connector(Url='https://<unique-host>.invalid/as2', AccessRole=<role arn>, As2Config={'LocalProfileId': <local>, 'PartnerProfileId': <partner>, 'MdnResponse': 'NONE', 'Compression': 'DISABLED', 'EncryptionAlgorithm': 'NONE', 'SigningAlgorithm': 'NONE'}).
5. Observe HTTP 200 and a ConnectorId instead of HTTP 400 InvalidRequestException 'The specified Url is not valid'.
6. Call tf.list_connectors() (paginated) and observe the connector with that '.invalid' Url is present, i.e. the call that should have been refused leaked a resource.
7. Re-replay the behavioral check transfer.connector-config-exclusivity-and-described-shape to confirm the fix and to exercise the later legs the abort skipped.
