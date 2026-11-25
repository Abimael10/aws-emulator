Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pca-connector-scep.create-challenge-on-inactive-connector-rejected
Issue-Shape-Id: com.amazonaws.pcaconnectorscep#PcaConnectorScep
Issue-Key: pca-connector-scep.create-challenge-on-inactive-connector-rejected|_assert_error#12|AssertionError|assert error.get("Message") == expected[1], (
Issue-Commit: 849876c9e4fa69c10b30bb76c9334887d39383a5

# CreateChallenge's ResourceNotFoundException message names the resource KIND ("Connector was not found") where AWS names the input member ("ConnectorArn was not found")

> _Note: this issue was identified against the code base at commit `849876c9e4fa69c10b30bb76c9334887d39383a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChallenge

## Description
Cause 1 of 1 — CreateChallenge's not-found sentence is built from the resource kind instead of the input member that carried the unresolvable reference. Failing check: pca-connector-scep.create-challenge-on-inactive-connector-rejected.

AWS: CreateChallenge takes a required `ConnectorArn` (Smithy: CreateChallenge.ConnectorArn, @required, @length 5..200, pattern ^arn:aws(-[a-z]+)*:pca-connector-scep:...:connector/<uuid>$) and models ResourceNotFoundException (httpError 404, members Message/ResourceId/ResourceType all @required; ResourceType doc: "either `Connector` or `Challenge`"). The model does not state the Message wording, so this is recorded behavior only: for a well-formed but nonexistent connector ARN, the recording shows CreateChallenge answering 404 `ResourceNotFoundException` with Message `"ConnectorArn was not found: <connector arn>."` — fixed text `"ConnectorArn was not found: "` plus the ARN taken verbatim from the request's ConnectorArn member and a terminating `.`. The SAME check's positive control shows the sibling read, ListChallengeMetadata, wording the same class differently: `"Connector was not found: <connector arn>."` (fixed text `"Connector was not found: "` + the request's ConnectorArn + `.`), and the emulator matches that one. So AWS words CreateChallenge's refusal after the INPUT MEMBER that carried the reference, and ListChallengeMetadata's after the resource KIND; they are not one sentence.

Emulator: CreateChallenge answers 404 `ResourceNotFoundException` — the right status and code — but with Message `"Connector was not found: <connector arn>."`. The sentence is produced by the single shared builder `crate::state::resource_not_found(resource_type, resource_id)`, whose Message is `format!("{resource_type} was not found: {resource_id}.")`, and CreateChallenge's `not_found` helper passes `crate::state::resource_type_of(connector_arn)` — the ARN's resource KIND, i.e. `"Connector"`. Because that one builder derives Message from the ResourceType member, the handler cannot state the member-named sentence AWS uses without also changing ResourceType, and the message word is therefore pinned to `"Connector"` for CreateChallenge too. Note the previously FIXED issue 20261116T052430Z-pca-connector-scep-createchallenge-error-response.md correctly moved the ResourceType MEMBER to `"Connector"`; it is the MESSAGE that must keep naming `ConnectorArn`. ResourceType is not what this check asserts — it asserts Code and Message only — so that fix is not being reopened; the defect is that the message got coupled to it.

Why: a client that branches on, logs, or asserts the refusal sentence (and the many callers that surface the service's message straight to an operator) sees a different sentence than AWS emits for a nonexistent connector on CreateChallenge, and it sees CreateChallenge and ListChallengeMetadata as indistinguishable where AWS distinguishes them. Error-message assertions and log scrapes written against AWS fail against the emulator even though the status and code are right.

Fix: CreateChallenge's error construction — src/ops/create_challenge.rs `not_found` — must word its Message after the input member that carried the unresolved reference (`ConnectorArn`) while keeping the ResourceType member within the model's documented domain (`Connector`). That means the shared builder in src/state.rs can no longer derive the message word from the ResourceType member: give it a separate "what the sentence names" input (defaulting to the resource kind, so ListChallengeMetadata, GetConnector, DeleteChallenge and the rest keep the kind-named sentence they already match), or let CreateChallenge supply its own sentence. Do not change ListChallengeMetadata's wording.

Traces: pca-connector-scep.create-challenge-on-inactive-connector-rejected (assertion in _assert_rejection_is_state_specific: expected message 'ConnectorArn was not found: <arn>.', got 'Connector was not found: <arn>.'). Code citations: src/ops/create_challenge.rs:267-269 and :358-373; src/state.rs:860-887 (`resource_not_found`, message template at :883).

## Evidence
Failing check pca-connector-scep.create-challenge-on-inactive-connector-rejected, in _assert_rejection_is_state_specific (check file lines 63/89):

  AssertionError: expected message 'ConnectorArn was not found: arn:aws:pca-connector-scep:us-west-2:000000000000:connector/1affea54-7b8d-477e-8f41-b1864b3a7f7d.', got 'Connector was not found: arn:aws:pca-connector-scep:us-west-2:000000000000:connector/1affea54-7b8d-477e-8f41-b1864b3a7f7d.'

(The region, account and connector UUID above are this run's values; the requirement is the fixed text plus the ConnectorArn the request supplied.)

Expected (AWS), CreateChallenge on a well-formed nonexistent connector ARN:
  HTTP 404, Code "ResourceNotFoundException",
  Message "ConnectorArn was not found: <request's ConnectorArn>."
    fixed text: "ConnectorArn was not found: " and the trailing "."
    variable: the ConnectorArn value from the request

Actual (emulator):
  HTTP 404, Code "ResourceNotFoundException",
  Message "Connector was not found: <request's ConnectorArn>."

Positive control in the same check — ListChallengeMetadata on the same bogus ARN expects, and the emulator produces, "Connector was not found: <arn>." So the two operations must word this class differently, and the emulator words both with the resource kind.

Source of the single shared sentence — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/state.rs:881-887:
    pub fn resource_not_found(resource_type: &str, resource_id: &str) -> ResourceNotFoundException {
        ResourceNotFoundException {
            message: format!("{resource_type} was not found: {resource_id}."),
            resource_id: resource_id.to_string(),
            resource_type: resource_type.to_string(),
        }
    }

CreateChallenge's call into it — src/ops/create_challenge.rs:367-373:
    fn not_found(connector_arn: &str) -> CreateChallengeError {
        CreateChallengeErrorModeled::ResourceNotFoundException(crate::state::resource_not_found(
            crate::state::resource_type_of(connector_arn),
            connector_arn,
        ))
        .into()
    }

Model (smithy_query, shape ResourceNotFoundException): httpError 404; Message/ResourceId/ResourceType @required; ResourceType doc "The resource type, which can be either Connector or Challenge." — it constrains the MEMBER, not the message wording.

## Steps to Reproduce
1. Create and activate an ACM Private CA, then CreateConnector against it (any state; the connector involved here is irrelevant).
2. Build a well-formed connector ARN that was never created: arn:aws:pca-connector-scep:<region>:<account>:connector/<fresh uuid4>.
3. POST /challenges with {"ConnectorArn": "<that bogus arn>", "ClientToken": "<uuid4>"}.
4. Observe HTTP 404 ResourceNotFoundException with Message "Connector was not found: <bogus arn>."; AWS answers "ConnectorArn was not found: <bogus arn>."
5. For contrast, GET /challengeMetadata?ConnectorArn=<the same bogus arn>: both AWS and the emulator answer 404 ResourceNotFoundException with Message "Connector was not found: <bogus arn>." — that one is correct and must not change.
6. Replay check pca-connector-scep.create-challenge-on-inactive-connector-rejected to confirm.
