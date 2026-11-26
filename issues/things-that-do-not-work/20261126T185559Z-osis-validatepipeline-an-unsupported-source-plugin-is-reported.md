Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: osis.validate-pipeline-reports-errors-for-invalid-config osis.validate-pipeline-missing-permission-rejected
Issue-Shape-Id: com.amazonaws.osis#AmazonOpenSearchIngestionService
Issue-Key: osis.validate-pipeline-reports-errors-for-invalid-config|run#7|AssertionError|assert resp["isValid"] is False, "expected isValid=False for an unsupported source plugin"
Issue-Key: osis.validate-pipeline-missing-permission-rejected|_client_error#12|AssertionError|raise AssertionError(
Issue-Commit: ff0a2433ea076228f93b8b79ef2b1e947e770a3c

# osis ValidatePipeline: an unsupported source plugin is reported isValid true, and a no-permission assumed role is served 200 instead of 403 AccessDeniedException

> _Note: this issue was identified against the code base at commit `ff0a2433ea076228f93b8b79ef2b1e947e770a3c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ValidatePipeline

## Description
ValidatePipeline is the pre-flight gate a client asks before CreatePipeline/UpdatePipeline. The emulator's handler (services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/ops/validate_pipeline.rs) derives the whole verdict from one judgement, crate::state::configuration_validation_messages, which asks only whether every sts_role_arn the document names resolves to a locally stored, assumable IAM role; and the operation is served without ever evaluating the caller's identity. Two independent root causes follow. 2 causes, both on ValidatePipeline.

Cause 1 of 2: a configuration whose source names a plugin OpenSearch Ingestion does not support is reported isValid: true with no Errors (failing check: osis.validate-pipeline-reports-errors-for-invalid-config).
AWS: ValidatePipeline (POST /2022-01-01/osis/validatePipeline, success 200) answers isValid — "A boolean indicating whether or not the pipeline configuration is valid" — and Errors — "A list of errors if the configuration is invalid" (Smithy: ValidatePipelineRequest.PipelineConfigurationBody @length 1..100000 required; ValidatePipelineResponse {isValid: Boolean, Errors: ValidationMessageList}; ValidationMessage.Message = "The validation message"; modeled errors AccessDeniedException 403, DisabledOperationException 409, InternalException 500, ValidationException 400). The model does not enumerate the supported plugin names, so for this class the behavior is recorded behavior only: a body that parses as a Data Prepper document but whose source stanza names an unknown plugin is answered HTTP 200 (not ValidationException) with isValid: false and exactly ONE ValidationMessage, whose text names the unsupported plugin and the JSON path of the offending node — the fixed text includes the phrase "not in the list of sources supported" and the quoted path component 'source'; the variable parts are the rejected plugin name taken from the request body and the JSON path to it. Only the plugin name and path vary per request; the phrasing is fixed.
Emulator: HTTP 200 with {"isValid": true} and Errors absent (the replayed response body was 16 bytes, i.e. isValid only) for a body whose source stanza is `not-a-real-source-plugin`. The handler's own module doc states the gap: "WHAT IS JUDGED, and what is not: the judgement is the roles the document names ... The SYNTAX of a configuration body is not judged here". A document that names no resolvable role question raises no message, so isValid is computed as reported.is_empty() == true.
Why: a client uses this call precisely to catch a bad plugin before paying for a pipeline create. A false positive sends a configuration AWS rejects straight into CreatePipeline, the caller has no ValidationMessage to surface to a user, and a test or deployment pipeline that gates on isValid passes a configuration that can never run.
Fix: the operation's validation — the shared configuration judgement used by this handler and by the CreatePipeline/UpdatePipeline submit paths — must read the submitted body as a Data Prepper document and judge its plugin names (at minimum, raise a ValidationMessage for a source plugin outside the supported set) in addition to the role questions, so isValid reflects the document's content.
Note: open issue 20261126T080449Z-osis-validatepipeline-reports-the-wrong-isvalid-verdict-it.md covers the role-assumability false negative and a body that does not DESERIALIZE at all; this cause is a body that deserializes fine and names an unsupported plugin, which that issue's causes do not cover, and this check was not marked OWNED by it. Open issue 20261021T234042Z-osis-validatepipeline-stub.md describes the operation answering 501; it now answers 200, so it does not cover this either.
Traces: osis.validate-pipeline-reports-errors-for-invalid-config.

Cause 2 of 2: ValidatePipeline is served successfully to a caller whose credentials carry no osis permissions at all (failing check: osis.validate-pipeline-missing-permission-rejected).
AWS: ValidatePipeline models AccessDeniedException, HTTP 403, documented "You don't have permissions to access the resource" (Smithy: com.amazonaws.osis#AccessDeniedException, errorKind client, httpError 403, member message). A sigv4 call made with credentials from an assumed role that has no identity-based policy granting osis:ValidatePipeline is refused with that error and the standard implicit-deny message: fixed text "User: " / " is not authorized to perform: " / " on resource: " / " because no identity-based policy allows the " / " action", with the variable parts taken from the request and IAM state — the caller's assumed-role principal ARN (arn:aws:sts::<account>:assumed-role/<roleName>/<sessionName>), the action osis:ValidatePipeline, and the resource arn:aws:osis:<region>:<account>:*. Beyond the modeled error and its doc, the exact message layout is recorded behavior only.
Emulator: HTTP 200 with {"isValid": true}. The call made with the freshly assumed, permission-less role's temporary credentials (STS AssumeRole succeeded, so the credentials are the emulator's own) was answered identically to the privileged control. No AccessDeniedException is ever produced: the handler takes nothing from ctx but the region and never consults the calling principal's policies.
Why: a client cannot verify its least-privilege posture against the emulator — an IAM policy test suite, a permission-boundary regression test, or any negative-authorization assertion passes here and fails against AWS. Worse, code that treats a 200 from ValidatePipeline as proof the caller may also create the pipeline will proceed on a principal AWS would have refused at the first call.
Fix: the operation's handler/authorization layer — ValidatePipeline must resolve the calling principal from the signed request and refuse with the modeled AccessDeniedException (403) when no identity-based policy allows osis:ValidatePipeline, rather than answering the verdict unconditionally.
Traces: osis.validate-pipeline-missing-permission-rejected.

## Evidence
Cause 1 — check osis.validate-pipeline-reports-errors-for-invalid-config (mismatch):
  AssertionError: expected isValid=False for an unsupported source plugin
  Positive control passed: the structurally valid config returned isValid=true.
  Request: POST /2022-01-01/osis/validatePipeline with a Data Prepper v2 document whose `source` stanza is `not-a-real-source-plugin: {path: "/ingest"}` and a valid s3 sink.
  Expected: 200, isValid=false, Errors of length 1 whose Message contains the fixed phrase "not in the list of sources supported" and the quoted path component 'source' (variable parts: the rejected plugin name and its JSON path).
  Actual: 200, isValid=true, Errors absent.
  Code: services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/ops/validate_pipeline.rs:15-21 (documented gap: syntax/content of the body is not judged) and :168-187 (isValid = reported.is_empty(), reported coming only from configuration_validation_messages).

Cause 2 — check osis.validate-pipeline-missing-permission-rejected (mismatch):
  AssertionError: expected AccessDeniedException, but the restricted-role call SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, ...'content-length': '16'...}, 'isValid': True}
  Setup: iam.CreateRole with a trust policy allowing arn:aws:iam::<account>:root, then sts.AssumeRole with session name sim2real-osis-noperm; the role has NO attached or inline policy.
  Expected: HTTP 403, Error.Code "AccessDeniedException", Error.Message "User: arn:aws:sts::<account>:assumed-role/<roleName>/<sessionName> is not authorized to perform: osis:ValidatePipeline on resource: arn:aws:osis:<region>:<account>:* because no identity-based policy allows the osis:ValidatePipeline action". Fixed text: "User: ", " is not authorized to perform: ", " on resource: ", " because no identity-based policy allows the ", " action". Variable parts from the request/IAM state: the assumed-role principal ARN (account, role name, session name), the action name, the region and account in the resource ARN.
  Actual: HTTP 200, body {"isValid": true} — the same answer as the privileged control call.
  Code: services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/ops/validate_pipeline.rs:155-188 (handle reads only the region from ctx; AccessDeniedException is declared in ValidatePipelineErrorModeled at :100-107 but never constructed).

## Steps to Reproduce
Cause 1:
1. Boot the emulator.
2. Call osis.ValidatePipeline with a valid Data Prepper v2 configuration (http source, s3 sink with codec/aws/threshold). Observe 200 isValid=true — the control.
3. Call osis.ValidatePipeline with the same document but the source plugin renamed to a plugin that does not exist (e.g. `not-a-real-source-plugin`). Observe 200 with isValid=true and no Errors, where AWS answers 200 with isValid=false and exactly one ValidationMessage naming the unsupported source and the 'source' JSON path.

Cause 2:
1. Boot the emulator.
2. iam.CreateRole with a trust policy allowing arn:aws:iam::<account>:root and attach no permissions policy.
3. sts.AssumeRole on that role (session name sim2real-osis-noperm) and build an osis client from the returned temporary credentials.
4. Call ValidatePipeline with any valid configuration body through that client. Observe HTTP 200 {"isValid": true}, where AWS refuses with HTTP 403 AccessDeniedException and the implicit-deny message naming the assumed-role principal, osis:ValidatePipeline and arn:aws:osis:<region>:<account>:*.
