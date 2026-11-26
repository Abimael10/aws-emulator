Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__gamelift__iam__s3.deploy-gamelift-build-round-trip-and-delete
Issue-Key: cloudformation__gamelift__iam__s3|ListBuilds|NotImplemented
Issue-Commit: 3fa8c42c1df735de25d8beda9af18b43438c14a4

# gamelift ListBuilds is an unimplemented scaffold stub — HTTP 501 NotImplemented instead of the account's build listing

> _Note: this issue was identified against the code base at commit `3fa8c42c1df735de25d8beda9af18b43438c14a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBuilds

## Description
Cause 1 of 1: `gamelift:ListBuilds` is still a scaffold stub, so no caller can enumerate the builds of the account and any flow that resolves a build by name is dead.

AWS: Smithy `com.amazonaws.gamelift#ListBuilds` is a normal paginated read bound to this service — input `Status` (BuildStatus: FAILED | INITIALIZED | READY), `Limit` (PositiveInteger, @range min 1), `NextToken` (NonEmptyString, @length min 1); output `Builds` (BuildList) plus `NextToken` (NonEmptyString, @length min 1); @paginated inputToken=NextToken, outputToken=NextToken, items=Builds, pageSize=Limit. Its only modeled errors are `InternalServiceException` (HTTP 500), `InvalidRequestException` (HTTP 400) and `UnauthorizedException` (HTTP 400) — there is no modeled "not implemented" outcome, and `NotImplemented` is not in the operation's error set at all. On a well-formed call AWS answers HTTP 200 with `Builds` holding every build resource of the account in use (an account with no builds yields an empty/absent list, not an error), filtered by `Status` when supplied, and returns `NextToken` only while more results remain. The documented ordering guarantee is explicitly none ("Build resources are not listed in any particular order").

Emulator: every ListBuilds call is refused. The handler's `handle` body is unconditionally `Err(crate::scaffolded::Scaffolded::not_implemented("ListBuilds"))`, which serializes as HTTP 501 with error code `NotImplemented` and message `operation ListBuilds is scaffolded but not implemented in this emulator` (fixed text; the only variable part is the operation name `ListBuilds` taken from the request's target). The file still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker line, so nothing about ListBuilds' real behavior is asserted. Note this is NOT a template-registration gap: the stack in the failing check reaches CREATE_COMPLETE and `AWS::GameLift::Build` resolves, so the divergence is purely the missing GameLift read.

Why: a client that uploads or CloudFormation-deploys a build and then looks it up by name — the standard way to confirm a deployment, to detect a leftover build after a rollback, or to page a build inventory — gets a 501 with an error code that is not in the operation's modeled error set. botocore raises `ClientError(NotImplemented)` rather than returning a page, so the code never reaches its own assertions; a retry loop sees a non-retryable unmodeled code and no amount of correct input helps. Absence of a build can no longer be distinguished from the operation being unavailable.

Fix: implement the operation's handler, `services/service-com-amazonaws-gamelift-gamelift/src/ops/list_builds.rs` — read the build records out of this crate's state through `crate::state::service_state`, apply the optional `Status` filter, page with `Limit`/`NextToken` honoring the modeled @paginated binding (emit `NextToken` only when a further page exists, and refuse a token this listing never issued with the modeled `InvalidRequestException` rather than a 501), return HTTP 200 with `Builds`, and delete the `SCAFFOLDED, NOT IMPLEMENTED` marker line at the top of the file. Each listed build must carry the same `BuildId`, `BuildArn`, `Name`, `Status`, `OperatingSystem` and `ServerSdkVersion` that `DescribeBuild` reports for it, so a name lookup through the listing and a direct describe agree.

Traces: cloudformation__gamelift__iam__s3.deploy-gamelift-build-round-trip-and-delete — the check creates an `AWS::GameLift::Build` stack (CREATE_COMPLETE), then calls `list_builds()` to resolve the build by its declared Name and is refused at that call. Related but distinct: the auto-filed issue for the sibling `ListScripts` stub (services/service-com-amazonaws-gamelift-gamelift/src/ops/list_scripts.rs) covers scripts only and does not cover ListBuilds; no open issue names ListBuilds.

## Evidence
Failing check: cloudformation__gamelift__iam__s3.deploy-gamelift-build-round-trip-and-delete

Expected (AWS): HTTP 200, body `{"Builds": [ {"BuildId": "build-<uuid>", "BuildArn": "arn:aws:gamelift:<region>:<account>:build/build-<uuid>", "Name": "<the declared build name>", "Status": "READY", "OperatingSystem": "<declared OS>", "ServerSdkVersion": "<declared version>", ...} ], "NextToken": <present only while more pages remain>}`. The one build the stack created is listed under the Name the template declared, with the same BuildId that `Ref Build` returned. (Ids, names and the region/account above are values from the request and emulator state, not literal requirements.)

Actual (emulator): the call is refused —
`botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the ListBuilds operation: operation ListBuilds is scaffolded but not implemented in this emulator`
HTTP status 501, error code `NotImplemented` (not one of the operation's modeled errors InternalServiceException/InvalidRequestException/UnauthorizedException). Fixed text of the message: `operation ` + `<OperationName>` + ` is scaffolded but not implemented in this emulator`; the only variable part is the operation name.

Source: services/service-com-amazonaws-gamelift-gamelift/src/ops/list_builds.rs line 4 (`//! SCAFFOLDED, NOT IMPLEMENTED:` marker) and line 175 (`Err(crate::scaffolded::Scaffolded::not_implemented("ListBuilds"))`).

## Steps to Reproduce
1. Boot the emulator and point a botocore/boto3 gamelift client at it.
2. Call `gamelift.list_builds()` with no arguments (any combination of Status/Limit/NextToken behaves the same — the refusal precedes any use of the input).
3. Observe HTTP 501 / `NotImplemented` / "operation ListBuilds is scaffolded but not implemented in this emulator" instead of HTTP 200 with a `Builds` list.
4. Full path: replay cloudformation__gamelift__iam__s3.deploy-gamelift-build-round-trip-and-delete — it creates an S3 bucket + object, CloudFormation-deploys an `AWS::GameLift::Build` with an IAM role (stack reaches CREATE_COMPLETE), then calls `list_builds()` to find the build by the Name the template declared and is refused at that call.
