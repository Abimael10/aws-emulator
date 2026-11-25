Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 57fead3d76889f671b891b7ff2471bd21da9f263

# CreateChallenge's ResourceNotFoundException reports ResourceType "ConnectorArn", a value the model's documented domain excludes

_Filed: 2026-11-16T05:24:30Z_
_Operation: CreateChallenge | Class: error-response_

> _Note: this issue was identified against the code base at commit `57fead3d76889f671b891b7ff2471bd21da9f263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ResourceNotFoundException$ResourceType` is a @required member whose model documentation states its value domain exactly: it "can be either `Connector` or `Challenge`". When `CreateChallenge` cannot find the connector its `ConnectorArn` names, the handler fills that member with the INPUT MEMBER NAME `"ConnectorArn"` instead, and the `Message` is built from the same string, so the wire answer is `ResourceType: "ConnectorArn"` / `Message: "ConnectorArn was not found: <arn>."`.

That value is outside the documented domain, so a client that branches on `ResourceType` (the only machine-readable discriminator the shape carries besides `ResourceId`) matches neither documented case.

It is also inconsistent with the rest of this same crate: `src/state.rs` declares the vocabulary as exactly those two values and quotes the model sentence for it, `state::resource_type_of()` maps any ARN onto one of them, and every OTHER operation of this service that answers this error uses them (`GetConnector`, `GetChallengeMetadata`, `GetChallengePassword`, `ListChallengeMetadata`). `CreateChallenge` is one of only two call sites that step outside the vocabulary the crate itself states.

The handler and the test carry a stated reason for the divergence ("the thing that was not found is not the resource it acts on but the parameter that named it"), but that reason is the crate's own invention; the model sentence contradicts it.

## Evidence

Model (smithy_query query="doc" shape="ResourceNotFoundException" member="ResourceType"):
  "<p>The resource type, which can be either <code>Connector</code> or <code>Challenge</code>.</p>"
and the member is @required (smithy_query query="shape" shape="ResourceNotFoundException": `"ResourceType": {"target": "String", "required": true, ...}`).

Handler, services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/create_challenge.rs:353:
  `const MISSING_MEMBER_CONNECTOR_ARN: &str = "ConnectorArn";`
and :358-364:
```
fn not_found(connector_arn: &str) -> CreateChallengeError {
    CreateChallengeErrorModeled::ResourceNotFoundException(crate::state::resource_not_found(
        MISSING_MEMBER_CONNECTOR_ARN,
        connector_arn,
    ))
    .into()
}
```
reached from :255-257 `let connector = store.connector(&region, &connector_arn).ok_or_else(|| not_found(&connector_arn))?;`.

The builder puts that string straight into both the member and the sentence — src/state.rs:760-765:
```
pub fn resource_not_found(resource_type: &str, resource_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: format!("{resource_type} was not found: {resource_id}."),
        resource_id: resource_id.to_string(),
        resource_type: resource_type.to_string(),
```

The same crate states the opposite vocabulary — src/state.rs:66-70:
```
/// `ResourceId`/`ResourceType` vocabulary of this service's errors: the `ResourceType`
/// member's doc says it "can be either `Connector` or `Challenge`".
pub const RESOURCE_TYPE_CONNECTOR: &str = "Connector";
/// The other value `ResourceType` carries.
pub const RESOURCE_TYPE_CHALLENGE: &str = "Challenge";
```
and src/state.rs:738-744 `resource_type_of()` returns only those two.

Test coverage that currently pins the out-of-domain value (with the crate's own reason, not the model's): tests/all/resource_not_found_message.rs:40-43
```
/// What `CreateChallenge`'s 404 names instead: the input member that referenced the connector
/// (smithy_query: operation CreateChallenge), because the resource that call acts on is the
/// challenge it creates.
const MISSING_MEMBER_CONNECTOR_ARN: &str = "ConnectorArn";
```
Contrast tests/all/challenge_metadata.rs:505-511, where `GetChallengeMetadata`'s 404 is asserted to carry `state::RESOURCE_TYPE_CHALLENGE`.

## Steps to Reproduce

1. POST /challenges with body {"ConnectorArn": "arn:aws:pca-connector-scep:eu-west-2:000000000000:connector/11111111-2222-3333-4444-555555555555"} signed for eu-west-2, where no such connector exists.
2. Observe HTTP 404 ResourceNotFoundException with body member ResourceType = "ConnectorArn" and Message = "ConnectorArn was not found: <arn>.".
3. Compare with the model sentence for ResourceNotFoundException$ResourceType ("can be either Connector or Challenge") and with GET /connectors/<same arn>, whose 404 carries ResourceType = "Connector".
