Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.lakehouse-configuration-idc-application-not-exists-rejected
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift.lakehouse-configuration-idc-application-not-exists-rejected|_error_retrying_invalid_state#21|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: 481131fe26391e4c6b7b7c8a6446ebe963f4d903

# redshift: ModifyLakehouseConfiguration ignores DryRun=true and answers 200 with the lakehouse configuration instead of the DryRunOperation refusal

> _Note: this issue was identified against the code base at commit `481131fe26391e4c6b7b7c8a6446ebe963f4d903`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyLakehouseConfiguration

## Description
Replay of simparity-behavioral-aws-v1 against the emulated Redshift control plane left exactly ONE failing check that the suite did not mark ALREADY FILED or OWNED: redshift.lakehouse-configuration-idc-application-not-exists-rejected. This bundle carries ONE cause, in the redshift service; that single cause must be closed before submitting.

Related but NOT covering this cause as the suite replays it: the open issue 20261113T150542Z-redshift-6-net-new-behavioral-causes-modifyaquaconfiguration-refuses.md describes the same DryRun behaviour as its Cause 5, but its recorded replay key for this check is the earlier RedshiftIdcApplicationNotExists message assertion (run#17); that assertion now PASSES and the check fails later, at the DryRun positive control (run#93), so the suite no longer attributes this check to that issue and it is re-grounded here so it is re-replayed. Nothing in this issue is a refusal of an operation a fixed issue claimed to fix (the two fixed lakehouse issues stood the operation up; the call succeeds, it just succeeds where AWS refuses).

=== Cause 1 of 1: ModifyLakehouseConfiguration with DryRun=true returns HTTP 200 and a LakehouseConfiguration body instead of refusing with DryRunOperation ===
AWS: Smithy (services/service-com-amazonaws-redshift-redshiftserviceversion20121201/smithy-model/com-amazonaws-redshift-redshiftserviceversion20121201.json): ModifyLakehouseConfigurationMessage$DryRun is a BooleanOptional documented "A boolean value that, if true, validates the request without actually modifying the lakehouse configuration. Use this to check for errors before making changes."; the operation's modeled errors are ClusterNotFoundFault (404/ClusterNotFound), DependentServiceAccessDeniedFault, DependentServiceUnavailableFault, InvalidClusterStateFault (400/InvalidClusterState), RedshiftIdcApplicationNotExistsFault (404/RedshiftIdcApplicationNotExists), UnauthorizedOperation and UnsupportedOperationFault — the model declares NO DryRunOperation error, so the dry-run signal is recorded behavior only: a dry run whose request WOULD have succeeded is reported EC2-style by REFUSING the call with error code DryRunOperation and the message "Request would have succeeded, but DryRun flag is set." That message is entirely fixed text with no variable part, and no response body is returned.
Emulator: after validating the request the handler proceeds to the normal success path and answers HTTP 200 with the cluster's current LakehouseConfiguration (ClusterIdentifier, LakehouseRegistrationStatus, CatalogArn). No DryRunOperation error is ever raised, for any value of DryRun.
Why: DryRun is the only way a client can check a lakehouse registration/association change before applying it, and because AWS signals the would-succeed outcome with an ERROR rather than a 2xx, a client that probes with DryRun=true reads the emulator's 200 either as "DryRun unsupported / not honoured" or as a change actually having been applied. A caller cannot distinguish a validated dry run from a real modification, and any code branching on the DryRunOperation code takes the wrong branch.
Fix: the ModifyLakehouseConfiguration handler's DryRun branch (services/service-com-amazonaws-redshift-redshiftserviceversion20121201/src/ops/modify_lakehouse_configuration.rs — the DryRun check sits just ahead of the success serialization around lines 284-286): once every precondition has been validated and nothing has been mutated, refuse the request with error code DryRunOperation and AWS's fixed message instead of serializing the configuration.
Traces: redshift.lakehouse-configuration-idc-application-not-exists-rejected (its DryRun positive control, which calls ModifyLakehouseConfiguration(ClusterIdentifier=<cluster>, DryRun=True, LakehouseRegistration="Register") on an already-registered cluster and requires the DryRunOperation refusal).

## Evidence
Suite: simparity-behavioral-aws-v1 — Total 151/244 passed (61.9%); redshift 113/187 (60%), 74 failed, of which all but the one grounded here were marked ALREADY FILED or OWNED and skipped.

Cause 1 — redshift.lakehouse-configuration-idc-application-not-exists-rejected
  expected: redshift.modify_lakehouse_configuration(ClusterIdentifier=<cluster>, DryRun=True, LakehouseRegistration="Register") raises botocore ClientError with Error.Code == "DryRunOperation" and Error.Message == "Request would have succeeded, but DryRun flag is set." (the whole sentence is fixed text; there is no request- or state-derived part), and no response body
  actual:   HTTP 200 with a LakehouseConfiguration body; the check's helper reports
            AssertionError: expected the call to be rejected, but it succeeded and returned {'ClusterIdentifier': 'sim2real-lakehouse-c60fe3597d63', 'LakehouseRegistrationStatus': 'Registered', 'CatalogArn': 'arn:aws:glue:us-west-2:000000000000:catalog/sim2real-lakehouse-c60fe3597d63', 'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}
  (the cluster identifier, catalog ARN, account id, region, request id and date above are per-run values and are NOT part of the requirement; the requirement is that DryRun=true is answered with the DryRunOperation error code and that fixed message rather than a 200 body. The earlier assertions in the same check — the 404 RedshiftIdcApplicationNotExists code and message for a bogus LakehouseIdcApplicationArn, and the unchanged LakehouseRegistrationStatus afterwards — now pass; the failure is at the DryRun positive control.)

Model corroboration: ModifyLakehouseConfigurationMessage$DryRun documented "validates the request without actually modifying the lakehouse configuration. Use this to check for errors before making changes."; output shape LakehouseConfiguration{ClusterIdentifier, LakehouseIdcApplicationArn, LakehouseRegistrationStatus, CatalogArn}; no DryRunOperation error is modeled, so the error code and message are taken from the recording.

## Steps to Reproduce
0. git clone the repo, build and boot the emulator, and point a botocore `redshift` client at it.
1. Create a cluster subnet group over the run's network, then CreateCluster a single-node RA3 cluster (NodeType="ra3.large", ClusterType="single-node") and wait until ClusterStatus is "available" and ClusterAvailabilityStatus is "Available".
2. Register the lakehouse so the cluster has a catalog: redshift.modify_lakehouse_configuration(ClusterIdentifier=<cluster>, LakehouseRegistration="Register", CatalogName=<cluster>), retrying through InvalidClusterState, then wait for the cluster to be available again.
3. Cause 1: redshift.modify_lakehouse_configuration(ClusterIdentifier=<cluster>, DryRun=True, LakehouseRegistration="Register") -> expect a ClientError with Code "DryRunOperation" and Message "Request would have succeeded, but DryRun flag is set."; observe HTTP 200 and a LakehouseConfiguration body instead.
4. Re-replay the check id in trace_ids to confirm the fix.
