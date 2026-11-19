Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 2bfe2a8a9c1ad5a493cf856532ec70b14ee46345

# UpdateEnvironmentConnector runs the credential-secret check before deciding whether anything changes, so a no-op repeat naming the connector's own secret is refused where the sibling create answers 200

_Filed: 2026-11-19T11:29:59Z_
_Operation: UpdateEnvironmentConnector | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `2bfe2a8a9c1ad5a493cf856532ec70b14ee46345`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateEnvironmentConnector` resolves `secretIdentifier` to an ARN, then validates the secret against the Secrets Manager store, and only AFTER that compares the resolved ARN with the one the connector already carries to decide whether anything changed. So a request that names the secret the stored connector already points at — a request the handler itself defines as reconfiguring nothing — is still refused with `ValidationException` if that secret has meanwhile been deleted (or scheduled for deletion) in Secrets Manager.

The sibling create states the opposite rule explicitly and implements it: `CreateEnvironmentConnector` returns the stored record for an idempotent repeat BEFORE it consults Secrets Manager, on the stated ground that a call which creates no connection needs no credentials to create one with. The update handler's own comment claims it checks credentials "exactly as at a create", but the ordering differs: at a create the check is reached only when a connector is really about to be created, while at an update it is reached even when the comparison that follows will find nothing to reconfigure.

Result: for one and the same connector and one and the same secret, the idempotent repeat of the create is served 200 with the stored record, while the idempotent repeat of the update is refused 400 naming a secret the caller is not asking to change.

## Evidence

The create's rule — `services/service-com-amazonaws-evs-amazonelasticvmwareservice/src/ops/create_environment_connector.rs:306-313` returns the stored record first:
```
            if existing.is_live()
                && crate::state::same_dns_name(&existing.appliance_fqdn, &input.appliance_fqdn)
                && existing.secret_arn == secret_arn
            {
                return Ok(CreateEnvironmentConnectorOutput {
                    connector: Some(existing.to_wire(&environment_id)),
                });
            }
```
and `create_environment_connector.rs:331-335` states why the credential check sits after it:
```
        // Which is only possible with credentials to establish it WITH: the secret the
        // request names is one the account holds, storing the fields this connector type
        // requires. Evaluated here, where a connector is really about to be created: the
        // repeat above stores nothing and the conflict creates nothing, so neither needs
        // the credentials of a connection no new connector makes.
```

The update does it the other way round — `src/ops/update_environment_connector.rs:324-352`:
```
                // A connector is only ever pointed at credentials it can really connect
                // with, exactly as at a create: ...
                let credential_failures =
                    crate::state::secret_resources::credential_secret_failures(... secret_identifier, &connector.connector_type);
                if !credential_failures.is_empty() {
                    return Err(UpdateEnvironmentConnectorErrorModeled::ValidationException(
                        crate::state::aggregate_validation_error(credential_failures),
                    )
                    .into());
                }
                ...
                let changed = connector.secret_arn != secret_arn;
```
The `changed` comparison — the handler's own definition of "this call reconfigures nothing" — is evaluated at line 351, AFTER the refusal at lines 340-345, contradicting the rule stated above it at `update_environment_connector.rs:304-311`:
```
        // The reconfiguration itself. A value the connector already carries is NOT a
        // change: the record keeps its state and its modifiedAt, so repeating the call
        // neither restarts the connector nor moves a stamp. ... so a
        // request naming the stored appliance or the stored secret in its other form
        // reconfigures nothing and the stored record is left exactly as it was.
```

The refusal reached is `src/state/secret_resources.rs:221-228`, which treats a secret marked for deletion as absent:
```
    let Some(secret) = store
        .get(secret_region(region, secret_identifier), secret_identifier)
        .filter(|secret| !secret.scheduled_for_deletion())
    else {
        return vec![invalid_secret_clause(member, secret_identifier)];
    };
```

Test coverage: `tests/all/connector_credential_secret.rs:380-413` only exercises updates that name a DIFFERENT, unusable secret ("a bare name the account holds no secret for", "a full ARN the account holds no secret for", "a secret that does not store the fields this connector's type requires"); the no-change repeat is not covered, so no test pins the current answer.

## Steps to Reproduce

1. Register a Secrets Manager secret `evs/vcf-appliance-credentials` storing `username` and `password`.
2. CreateEnvironment, settle it, then CreateEnvironmentConnector with `type` VCENTER, an `applianceFqdn` and `secretIdentifier` `evs/vcf-appliance-credentials`; settle so the connector is ACTIVE.
3. Delete (or schedule deletion of) that secret through Secrets Manager.
4. Call CreateEnvironmentConnector again with the same `type`, `applianceFqdn` and `secretIdentifier`: HTTP 200, the stored connector is reported, Secrets Manager is never consulted (create_environment_connector.rs:306-313).
5. Call UpdateEnvironmentConnector with `secretIdentifier` = `evs/vcf-appliance-credentials`, i.e. the ARN the connector already carries: HTTP 400 ValidationException "The secret ... is not valid or does not exist", even though the handler's own `changed` comparison would have found nothing to reconfigure.
