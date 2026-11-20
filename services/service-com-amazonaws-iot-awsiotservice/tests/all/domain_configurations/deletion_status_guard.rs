// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of WHEN a domain configuration may be deleted, driven against
//! the `CreateDomainConfiguration` / `DescribeDomainConfiguration` /
//! `UpdateDomainConfiguration` / `DeleteDomainConfiguration` helpers of the parent suite.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `DELETE /domainConfigurations/{domainConfigurationName}` signed for the `iot` signing
//! name — so what is asserted is what an AWS SDK client would see, not an internal
//! function's return value.
//!
//! The rule under test: a configuration in `ENABLED` status is the live endpoint settings
//! devices connect under, so it cannot be torn out from under them in one call. The delete
//! is admissible only once the configuration serves nobody — `DISABLED`, the other value
//! `DomainConfigurationStatus` admits — and the way there is the operation that carries the
//! status, `UpdateDomainConfiguration$domainConfigurationStatus` ("The status to which the
//! domain configuration should be updated"). Asking for that status is not being in it: the
//! configuration stops serving the connections it is serving first, so a delete that comes
//! before the disable is in effect is refused too. A delete that comes too early is refused
//! with the modelled `InvalidRequestException` (HTTP 400) and leaves the configuration
//! exactly as it was, so the caller can read it, wait for the disable and delete it; the
//! status is read from the RECORD, so a configuration put back into service is undeletable
//! once more.

use super::*;

/// RULE: a configuration the account still serves connections under is not deleted, and
/// the refusal leaves it exactly as it was.
#[tokio::test]
async fn a_configuration_that_still_serves_cannot_be_deleted() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("deletion guard", &certificate_pem, &private_key_pem)
        .await;
    let trusted = trusted_validation_certificate(&emulator, "deletion guard").await;

    // The rows span the kinds of configuration an account holds and the service types
    // one may deliver (`smithy_query(query="shape",shape="ServiceType")`: DATA,
    // CREDENTIAL_PROVIDER, JOBS), since the guard is about the STATUS and not about what
    // the configuration configures.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "the AWS-managed data endpoint",
            "guard-data",
            json!({ "serviceType": "DATA" }),
        ),
        (
            "the credentials endpoint",
            "guard-credentials",
            json!({ "serviceType": "CREDENTIAL_PROVIDER" }),
        ),
        (
            "the jobs endpoint",
            "guard-jobs",
            json!({ "serviceType": "JOBS" }),
        ),
        (
            "a customer-managed domain",
            "guard.custom-1",
            json!({
                "domainName": "iot.example.com",
                "serverCertificateArns": [held.clone()],
                "validationCertificateArn": trusted.clone(),
            }),
        ),
    ];

    for (case, name, body) in &rows {
        let created = emulator.create_configuration(name, body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: the configuration must be creatable, got {} body {}",
            created.status,
            created.text()
        );
        let before = emulator.describe_configuration(name).await.json();
        assert_eq!(
            before["domainConfigurationStatus"],
            json!("ENABLED"),
            "case {case}: a created configuration serves from the moment it exists, got {before}"
        );

        let refused = emulator.delete_configuration(name).await;
        assert_eq!(
            refused.status,
            400,
            "case {case}: expected the delete of a serving configuration to answer 400, got {} \
             body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            refused.text()
        );
        assert!(
            refused.error_message().contains("DISABLED"),
            "case {case}: the refusal must name the status to move to, got {}",
            refused.error_message()
        );
        assert_eq!(
            emulator.describe_configuration(name).await.json(),
            before,
            "case {case}: a refused delete must leave the configuration exactly as it was"
        );

        // Disabled, the configuration serves nobody, so the delete removes it and a read
        // of the name refuses afterwards.
        emulator.disable_configuration(case, name).await;
        let disabled = emulator.describe_configuration(name).await.json();
        assert_eq!(
            disabled["domainConfigurationStatus"],
            json!("DISABLED"),
            "case {case}: the update must move the status, got {disabled}"
        );
        let deleted = emulator.delete_configuration(name).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case}: expected the delete of a disabled configuration to answer 200, got {} \
             body {}",
            deleted.status,
            deleted.text()
        );
        assert_eq!(
            deleted.json(),
            json!({}),
            "case {case}: the delete's output is empty, got {}",
            deleted.text()
        );
        let read = emulator.describe_configuration(name).await;
        assert_eq!(
            read.status,
            404,
            "case {case}: expected the read of the deleted configuration to answer 404, got {} \
             body {}",
            read.status,
            read.text()
        );
    }
}

/// RULE: the guard reads the STATUS the record holds, so a configuration put back into
/// service is undeletable again, and one whose disable is in effect is deletable.
#[tokio::test]
async fn the_guard_reads_the_status_the_record_holds() {
    let emulator = Emulator::new();

    // The rows are the status sequences a configuration can have been through when the
    // delete arrives: disabled then put back into service, disabled twice (the second
    // update changes nothing), and enabled explicitly.
    let rows: Vec<(&str, &str, Vec<&str>, u16)> = vec![
        (
            "disabled and enabled again",
            "re-enabled",
            vec!["DISABLED", "ENABLED"],
            400,
        ),
        (
            "disabled twice",
            "twice-disabled",
            vec!["DISABLED", "DISABLED"],
            200,
        ),
        (
            "enabled explicitly",
            "explicitly-enabled",
            vec!["ENABLED"],
            400,
        ),
    ];

    for (case, name, statuses, expected) in &rows {
        let created = emulator
            .create_configuration(name, &json!({ "serviceType": "DATA" }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: the configuration must be creatable, got {} body {}",
            created.status,
            created.text()
        );
        for status in statuses {
            let updated = emulator
                .update_configuration(name, &json!({ "domainConfigurationStatus": status }))
                .await;
            assert_eq!(
                updated.status,
                200,
                "case {case}: expected the move to {status} to answer 200, got {} body {}",
                updated.status,
                updated.text()
            );
            // Each move is waited for, so the next one acts on the configuration as it
            // is rather than on one still moving.
            emulator.await_status(case, name, status).await;
        }
        let last = statuses
            .last()
            .expect("every row moves the status at least once");
        let described = emulator.describe_configuration(name).await.json();
        assert_eq!(
            described["domainConfigurationStatus"],
            json!(last),
            "case {case}: expected the record to hold {last}, got {described}"
        );
        let deleted = emulator.delete_configuration(name).await;
        assert_eq!(
            deleted.status, *expected,
            "case {case}: expected the delete to answer {expected}, got {} body {}",
            deleted.status,
            deleted.text()
        );
    }
}

/// RULE: asking for `DISABLED` is not being in it — the configuration keeps serving until
/// the change is in effect — so a delete that follows the disable immediately is refused,
/// and the same delete succeeds once the status has moved.
#[tokio::test]
async fn a_delete_that_outruns_the_disable_is_refused() {
    let emulator = Emulator::new();

    // The rows span the kinds of configuration the sequence is driven on, since the
    // transition is about the STATUS and not about what the configuration configures.
    let rows = [
        ("the data endpoint", "outrun-data", "DATA"),
        ("the jobs endpoint", "outrun-jobs", "JOBS"),
    ];
    for (case, name, service_type) in &rows {
        let created = emulator
            .create_configuration(name, &json!({ "serviceType": service_type }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: the configuration must be creatable, got {} body {}",
            created.status,
            created.text()
        );

        // The disable is acknowledged ...
        let disabled = emulator
            .update_configuration(name, &json!({ "domainConfigurationStatus": "DISABLED" }))
            .await;
        assert_eq!(
            disabled.status,
            200,
            "case {case}: expected the disable to answer 200, got {} body {}",
            disabled.status,
            disabled.text()
        );
        // ... and the delete that follows it in the same breath still meets the
        // configuration the account had, so it is refused and removes nothing.
        let refused = emulator.delete_configuration(name).await;
        assert_eq!(
            refused.status,
            400,
            "case {case}: expected the delete that outruns the disable to answer 400, got {} \
             body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            refused.text()
        );

        // Once the disable is in effect the same delete removes the configuration.
        emulator.await_status(case, name, "DISABLED").await;
        let deleted = emulator.delete_configuration(name).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case}: expected the delete of a disabled configuration to answer 200, got {} \
             body {}",
            deleted.status,
            deleted.text()
        );
        assert_eq!(
            emulator.describe_configuration(name).await.status,
            404,
            "case {case}: the configuration must be gone"
        );
    }
}
