Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appstream.appblockbuilder-running-update-rejected
Issue-Shape-Id: com.amazonaws.appstream#PhotonAdminProxyService
Issue-Key: appstream.appblockbuilder-running-update-rejected|run#17|AssertionError|assert code == "ResourceInUseException", (
Issue-Commit: f97a2791479cf3e78acbdef1e7d516a9feec11d8

# appstream UpdateAppBlockBuilder is an unimplemented 501 scaffold stub: the RUNNING-state refusal and the STOPPED-state update are both unreachable (1 cause)

> _Note: this issue was identified against the code base at commit `f97a2791479cf3e78acbdef1e7d516a9feec11d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAppBlockBuilder

## Description
Cause 1 of 1: `UpdateAppBlockBuilder` never runs any behaviour — it answers the scaffold's HTTP 501 `NotImplemented` for every request, so neither its state-dependent refusal nor its successful mutation exists. Failing check ids: appstream.appblockbuilder-running-update-rejected.

AWS: Smithy model `com.amazonaws.appstream#UpdateAppBlockBuilder` — input `UpdateAppBlockBuilderRequest` (`Name` required, pattern `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`; optional `InstanceType` (len 1..), `Description` (0..256), `DisplayName` (0..100), `Platform`, `VpcConfig`, `EnableDefaultInternetAccess`, `IamRoleArn`, `AccessEndpoints` (1..4), `AttributesToDelete`, `DisableIMDSV1`), output `UpdateAppBlockBuilderResult` with member `AppBlockBuilder`, HTTP 200. The operation's own modeled documentation states the state rule: "If the app block builder is in the STARTING or STOPPING state, you can't update it. If the app block builder is in the RUNNING state, you can only update the DisplayName and Description. If the app block builder is in the STOPPED state, you can update any attribute except the Name." The modeled error for that refusal is `ResourceInUseException` (errorKind client, HTTP 400, "The specified resource is in use."). The recorded behaviour for an attempt to change `InstanceType` while the builder is RUNNING is HTTP 400, error code `ResourceInUseException`, message `Cannot update the attributes for AppBlockBuilder <Name> when it is in RUNNING state. ` — the fixed text is `Cannot update the attributes for AppBlockBuilder `, ` when it is in `, ` state. ` plus the trailing space; the variable parts are the builder's `Name` taken from the request and the builder's current state taken from emulator state (`RUNNING` here). The refusal must leave the stored builder untouched, so a following DescribeAppBlockBuilders still reports the pre-update `InstanceType`. After StopAppBlockBuilder drains the builder to STOPPED the identical update must succeed with HTTP 200, return the updated `AppBlockBuilder`, and the new `InstanceType` must read back from DescribeAppBlockBuilders.

Emulator: every UpdateAppBlockBuilder call is refused with HTTP 501 and error code `NotImplemented` (the handler's only statement is `Err(crate::scaffolded::Scaffolded::not_implemented("UpdateAppBlockBuilder"))`). The replay saw error code 'NotImplemented' where 'ResourceInUseException' was required; because the stub refuses unconditionally, the positive control (the same update after the builder reaches STOPPED) can never succeed either, and no attribute of an app block builder can ever be mutated.

Why: a client that updates an app block builder cannot distinguish "this operation does not exist" from "this builder is busy". Code that catches `ResourceInUseException` to stop-then-retry sees an unmodeled 501 instead and aborts; code that legitimately updates a STOPPED builder's InstanceType, DisplayName, Description, VpcConfig or IamRoleArn fails outright, so no provisioning flow that resizes or re-tags a builder can run against the emulator.

Fix: implement the handler in services/service-com-amazonaws-appstream-photonadminproxyservice/src/ops/update_app_block_builder.rs — look the builder up in the service state by `Name` (404 `ResourceNotFoundException` when absent), gate on its stored state (refuse `ResourceInUseException` HTTP 400 with the message above when the requested attributes are not updatable in the current state, i.e. anything beyond DisplayName/Description while RUNNING and any update while STARTING/STOPPING), otherwise apply the supplied members plus `AttributesToDelete` to the stored builder and serialize the updated `AppBlockBuilder` with HTTP 200. Delete the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker line once written.

Traces: appstream.appblockbuilder-running-update-rejected (asserts the RUNNING refusal code/message, that the refused update left InstanceType at its prior value, and that the same update after StopAppBlockBuilder takes effect).

## Evidence
Check appstream.appblockbuilder-running-update-rejected:
  AssertionError: updating a RUNNING app block builder returned error code 'NotImplemented', expected 'ResourceInUseException'

Expected (AWS / Smithy): HTTP 400, Error.Code = ResourceInUseException, Error.Message = "Cannot update the attributes for AppBlockBuilder <builder-name> when it is in RUNNING state. " (fixed text plus the request's Name and the builder's current state); the builder's InstanceType unchanged afterwards; and the identical UpdateAppBlockBuilder after the builder reaches STOPPED returning HTTP 200 with the new InstanceType readable via DescribeAppBlockBuilders.

Actual (emulator): HTTP 501, Error.Code = NotImplemented, for every UpdateAppBlockBuilder request regardless of builder state. Handler body is the scaffold stub `Err(crate::scaffolded::Scaffolded::not_implemented("UpdateAppBlockBuilder"))` (services/service-com-amazonaws-appstream-photonadminproxyservice/src/ops/update_app_block_builder.rs:209), and the file still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker at line 4. No modeled error and no successful update path exist.

Note: this check is NOT marked ALREADY FILED by the suite (it is reported as a [mismatch], because the check catches the ClientError and asserts on its code rather than letting the 501 propagate), and no open issue names UpdateAppBlockBuilder.

## Steps to Reproduce
1. Create a VPC with two subnets in distinct AZs (ec2) and an S3 asset bucket with an object.
2. appstream CreateAppBlockBuilder Name=<n> Platform=WINDOWS_SERVER_2019 InstanceType=stream.standard.small VpcConfig={SubnetIds:[2 subnets]}.
3. appstream CreateAppBlock + AssociateAppBlockBuilderAppBlock, then StartAppBlockBuilder and wait until DescribeAppBlockBuilders reports State=RUNNING.
4. appstream UpdateAppBlockBuilder Name=<n> InstanceType=stream.standard.medium — observe HTTP 501 NotImplemented; AWS answers HTTP 400 ResourceInUseException with message "Cannot update the attributes for AppBlockBuilder <n> when it is in RUNNING state. ".
5. StopAppBlockBuilder Name=<n>, wait for State=STOPPED, then repeat the same UpdateAppBlockBuilder — observe HTTP 501 NotImplemented; AWS answers HTTP 200 and DescribeAppBlockBuilders then reports InstanceType=stream.standard.medium.
