// SPDX-License-Identifier: CC0-1.0

//! Activating and deactivating LDAP secure calls, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body, so
//! nothing here reaches into the state struct and a response that is not computed from
//! persisted state cannot pass.
//!
//! The rule under test is that LDAP security is a switch a directory carries per
//! `LDAPSType`: it can be activated only for a directory that is in service, of a type
//! that serves an LDAPS endpoint, that holds a certificate in service securing LDAP
//! calls, and whose LDAP security is not already active — and both operations APPLY the
//! switch they ask for in the call that asks for it, so the first read after either one
//! reports the position it put the switch in and nothing moves it afterwards. Every suite
//! below reads the recorded status back through `DescribeLDAPSSettings`, the operation a
//! client discovers it by.
//!
//! Every wait below waits for the CONDITION — the build of a directory, the registration
//! of a certificate — by looking again until it holds, so no case's outcome depends on how
//! fast the host runs.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ad_connector::{connect_directory, minimal_connect};
use crate::certificates::{
    certificate_pem, describe, register, STATE_REGISTERED,
    TYPE_CLIENT_CERT_AUTH, TYPE_CLIENT_LDAPS,
};
use crate::directory_lifecycle::{
    await_in_service, call, create_directory, minimal_create, ok, refused, service, string,
};
use crate::microsoft_ad::{create_microsoft_ad, minimal_microsoft_ad};

/// How many times a case looks again for a registration to be made, and how long it leaves
/// between looks: a generous bound on a state that never settles rather than a wait for a
/// known duration.
const LOOK_ATTEMPTS: usize = 1200;
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Every value of the modelled `LDAPSType` enum
/// (`smithy_query(query="shape",shape="LDAPSType")` answers `["Client"]`), which is what
/// both operations' required `Type` member admits: "Currently only the value Client is
/// supported."
const LDAPS_TYPE_CATALOG: [&str; 1] = ["Client"];

/// The values of the modelled `LDAPSStatus` enum
/// (`smithy_query(query="shape",shape="LDAPSStatus")` answers `["Enabling", "Enabled",
/// "EnableFailed", "Disabled"]`) this emulator's own writes produce: the one an activation
/// records and the one a deactivation records.
const STATUS_ENABLED: &str = "Enabled";
const STATUS_DISABLED: &str = "Disabled";

/// Seconds since the epoch, the unit an `awsJson1_1` timestamp is rendered in.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// A Managed Microsoft AD directory the case has looked for until its build finished, so
/// it is in service for the activations the cases make.
async fn directory_in_service(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let id = create_microsoft_ad(router, minimal_microsoft_ad(name), case).await;
    await_in_service(router, &id, case).await;
    id
}

/// An AD CONNECTOR directory the case has looked for the same way.
///
/// Client authentication — what a `ClientCertAuth` certificate performs — is offered for
/// this directory type and no other, so every row whose inventory holds such a certificate
/// is made against one of these.
async fn connector_in_service(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let id = connect_directory(router, minimal_connect(name), case).await;
    await_in_service(router, &id, case).await;
    id
}

/// Look again until `DescribeCertificate` reports `certificate_id` registered.
///
/// A registration IS a clock-owed transition — `RegisterCertificate` answers while the
/// registration is still being made — so a case that needs the certificate in service
/// polls for THAT CONDITION instead of waiting a fixed time for it.
async fn await_registered(
    router: &MultiServiceRouter,
    directory_id: &str,
    certificate_id: &str,
    case: &str,
) {
    for _ in 0..LOOK_ATTEMPTS {
        let certificate = describe(router, directory_id, certificate_id, case).await;
        if string(&certificate, "State", case) == STATE_REGISTERED {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let certificate = describe(router, directory_id, certificate_id, case).await;
    panic!(
        "case {case:?}: expected {certificate_id} to reach State {STATE_REGISTERED}, it stayed \
         at {:?}",
        certificate["State"]
    );
}

/// A certificate of `certificate_type` registered with `directory_id` and looked for until
/// the registration was made, so it is the certificate in service an activation needs.
async fn certificate_in_service(
    router: &MultiServiceRouter,
    directory_id: &str,
    common_name: &str,
    certificate_type: &str,
    case: &str,
) -> String {
    let (pem, _) = certificate_pem(Some(common_name), (2030, 1, 1));
    let id = register(
        router,
        json!({
            "DirectoryId": directory_id,
            "CertificateData": pem,
            "Type": certificate_type
        }),
        case,
    )
    .await;
    await_registered(router, directory_id, &id, case).await;
    id
}

/// The `LDAPSStatus` the store holds for `directory_id`'s `ldaps_type`, read back through
/// `DescribeLDAPSSettings`; `None` when it holds no setting for it at all.
async fn ldaps_setting(
    router: &MultiServiceRouter,
    directory_id: &str,
    ldaps_type: &str,
    case: &str,
) -> Option<Value> {
    let answer = ok(
        router,
        "DescribeLDAPSSettings",
        json!({ "DirectoryId": directory_id, "Type": ldaps_type }),
        case,
    )
    .await;
    answer["LDAPSSettingsInfo"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected LDAPSSettingsInfo, got {answer}"))
        .first()
        .cloned()
}

/// The `LDAPSStatus` of that setting, or `None` when there is no setting.
async fn ldaps_status(
    router: &MultiServiceRouter,
    directory_id: &str,
    ldaps_type: &str,
    case: &str,
) -> Option<String> {
    ldaps_setting(router, directory_id, ldaps_type, case)
        .await
        .and_then(|setting| setting["LDAPSStatus"].as_str().map(str::to_string))
}

/// `EnableLDAPS`, asserting it answered 200 with the modelled empty output.
async fn enable(router: &MultiServiceRouter, directory_id: &str, ldaps_type: &str, case: &str) {
    let answer = ok(
        router,
        "EnableLDAPS",
        json!({ "DirectoryId": directory_id, "Type": ldaps_type }),
        case,
    )
    .await;
    assert_eq!(
        answer,
        json!({}),
        "case {case:?}: expected EnableLDAPS to answer the modelled empty output, got {answer}"
    );
}

/// `DisableLDAPS`, asserting it answered 200 with the modelled empty output.
async fn disable(router: &MultiServiceRouter, directory_id: &str, ldaps_type: &str, case: &str) {
    let answer = ok(
        router,
        "DisableLDAPS",
        json!({ "DirectoryId": directory_id, "Type": ldaps_type }),
        case,
    )
    .await;
    assert_eq!(
        answer,
        json!({}),
        "case {case:?}: expected DisableLDAPS to answer the modelled empty output, got {answer}"
    );
}

/// One row of the activation table: the certificate inventory the directory holds when
/// the activation is made.
struct ActivationCase {
    case: &'static str,
    /// The certificates registered with the directory before the activation, as
    /// `(common name, CertificateType)` — at least one of them securing LDAP calls.
    certificates: Vec<(&'static str, &'static str)>,
}

#[tokio::test]
async fn an_activation_is_in_force_from_the_call_that_makes_it() {
    let router = service();

    // The rows differ along the certificate inventory the directory holds — one
    // LDAP-securing certificate, several of them, and one alongside a certificate that
    // performs the other modelled function — and the activation is made for every value
    // of the modelled `LDAPSType`.
    let cases = [
        ActivationCase {
            case: "one certificate securing LDAP calls",
            certificates: vec![("one.example.com", TYPE_CLIENT_LDAPS)],
        },
        ActivationCase {
            case: "several certificates securing LDAP calls",
            certificates: vec![
                ("first.example.com", TYPE_CLIENT_LDAPS),
                ("second.example.com", TYPE_CLIENT_LDAPS),
                ("third.example.com", TYPE_CLIENT_LDAPS),
            ],
        },
        ActivationCase {
            case: "an LDAP-securing certificate beside a client-authentication one",
            certificates: vec![
                ("auth.example.com", TYPE_CLIENT_CERT_AUTH),
                ("ldaps.example.com", TYPE_CLIENT_LDAPS),
            ],
        },
    ];

    for (row, ldaps_type) in cases
        .iter()
        .flat_map(|row| LDAPS_TYPE_CATALOG.iter().map(move |kind| (row, *kind)))
    {
        let case = row.case;
        // Client authentication is offered for AD Connector alone, so a row whose
        // inventory includes a certificate performing it is made against one of those;
        // LDAP security is served by both that type and Managed Microsoft AD.
        let client_authentication = row
            .certificates
            .iter()
            .any(|(_, certificate_type)| *certificate_type == TYPE_CLIENT_CERT_AUTH);
        let directory_id = if client_authentication {
            connector_in_service(&router, "activation.example.com", case).await
        } else {
            directory_in_service(&router, "activation.example.com", case).await
        };
        for (common_name, certificate_type) in &row.certificates {
            certificate_in_service(
                &router,
                &directory_id,
                common_name,
                certificate_type,
                case,
            )
            .await;
        }

        // Nothing has activated LDAP security yet, so the store holds no setting at all.
        assert_eq!(
            ldaps_status(&router, &directory_id, ldaps_type, case).await,
            None,
            "case {case:?}: expected no LDAPS setting before the activation"
        );

        let enabled_from = epoch_seconds(SystemTime::now());
        enable(&router, &directory_id, ldaps_type, case).await;
        let enabled_until = epoch_seconds(SystemTime::now());

        // The activation applies the switch it asks for, so the status a caller reads
        // straight after it is the one the activation put the switch in.
        let recorded = required_setting(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            recorded["LDAPSStatus"],
            json!(STATUS_ENABLED),
            "case {case:?}: expected the active status right after the activation, got {recorded}"
        );
        // Nothing failed on the way, so there is no state change to describe.
        assert!(
            recorded["LDAPSStatusReason"].is_null(),
            "case {case:?}: expected no LDAPSStatusReason on a switch nothing failed for, \
             got {recorded}"
        );
        let updated = recorded["LastUpdatedDateTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: expected a LastUpdatedDateTime, got {recorded}")
        });
        assert!(
            (enabled_from - 1.0..=enabled_until + 1.0).contains(&updated),
            "case {case:?}: expected LastUpdatedDateTime within the activation window \
             [{enabled_from}, {enabled_until}], got {updated}"
        );

        // A read does not change what the next caller sees.
        let again = required_setting(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            again, recorded,
            "case {case:?}: expected a repeated read to report the same setting, got {again}"
        );

        // An activation of LDAP security that is already active is refused rather than
        // written again, and the recorded setting is left alone.
        refused(
            &router,
            "EnableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "InvalidLDAPSStatusException",
            case,
        )
        .await;
        let untouched = required_setting(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            untouched, recorded,
            "case {case:?}: expected the refused activation to change nothing, got {untouched}"
        );
    }
}

/// The setting the store holds, which the case requires to be there.
async fn required_setting(
    router: &MultiServiceRouter,
    directory_id: &str,
    ldaps_type: &str,
    case: &str,
) -> Value {
    ldaps_setting(router, directory_id, ldaps_type, case)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a recorded LDAPS setting"))
}

#[tokio::test]
async fn an_activation_needs_a_certificate_in_service_securing_ldap_calls() {
    let router = service();

    for ldaps_type in LDAPS_TYPE_CATALOG {
        // The rows differ along why the directory holds no certificate that can secure
        // LDAP calls: none registered at all, only one performing the other modelled
        // function, several of those, and one that does secure LDAP calls but is still
        // being registered.
        let bare = directory_in_service(&router, "bare.example.com", "no certificate").await;

        let other_function =
            connector_in_service(&router, "other.example.com", "another function").await;
        certificate_in_service(
            &router,
            &other_function,
            "auth.example.com",
            TYPE_CLIENT_CERT_AUTH,
            "another function",
        )
        .await;

        let many = connector_in_service(&router, "many.example.com", "several of those").await;
        for index in 0..3 {
            certificate_in_service(
                &router,
                &many,
                &format!("auth-{index}.example.com"),
                TYPE_CLIENT_CERT_AUTH,
                "several of those",
            )
            .await;
        }

        let registering =
            directory_in_service(&router, "registering.example.com", "still registering").await;
        let (pem, _) = certificate_pem(Some("pending.example.com"), (2031, 2, 3));
        let pending = register(
            &router,
            json!({
                "DirectoryId": &registering,
                "CertificateData": pem,
                "Type": TYPE_CLIENT_LDAPS
            }),
            "still registering",
        )
        .await;

        let rows = [
            ("no certificate", &bare),
            ("another function", &other_function),
            ("several of those", &many),
            ("still registering", &registering),
        ];
        for (case, directory_id) in rows {
            refused(
                &router,
                "EnableLDAPS",
                json!({ "DirectoryId": directory_id, "Type": ldaps_type }),
                "NoAvailableCertificateException",
                case,
            )
            .await;
            // The refusal recorded nothing: no status a client could read as active.
            assert_eq!(
                ldaps_status(&router, directory_id, ldaps_type, case).await,
                None,
                "case {case:?}: expected the refused activation to record no setting"
            );
        }

        // The refusal was about the certificate, so the very same call is admitted once
        // the registration the last row made has been looked for and found: the
        // activation then applies the switch, and the next read reports it active.
        await_registered(&router, &registering, &pending, "the registration settled").await;
        enable(&router, &registering, ldaps_type, "the registration settled").await;
        assert_eq!(
            ldaps_status(
                &router,
                &registering,
                ldaps_type,
                "the registration settled"
            )
            .await
            .as_deref(),
            Some(STATUS_ENABLED),
            "case {:?}: expected LDAP security to reach the active status",
            "the registration settled"
        );

        // ...while the directories that still hold no LDAP-securing certificate are
        // refused exactly as before.
        for (case, directory_id) in [("no certificate", &bare), ("another function", &other_function)]
        {
            refused(
                &router,
                "EnableLDAPS",
                json!({ "DirectoryId": directory_id, "Type": ldaps_type }),
                "NoAvailableCertificateException",
                case,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn both_operations_need_a_directory_in_service_of_a_type_that_serves_ldaps() {
    let router = service();

    // A directory this account never created, in several forms the modelled
    // `DirectoryId` pattern `^d-[0-9a-f]{10}$` admits...
    let mut absent: Vec<String> = vec![
        "d-0000000000".to_string(),
        "d-00000000ff".to_string(),
        "d-abcdef0123".to_string(),
        "d-ffffffffff".to_string(),
    ];
    // ...and one that existed until it was deleted, so the refusal follows the store
    // rather than the shape of the id.
    let doomed = directory_in_service(&router, "doomed.example.com", "deleted").await;
    crate::harness::delete_directory_and_wait(&router, &doomed, "deleted").await;
    absent.push(doomed);

    for ldaps_type in LDAPS_TYPE_CATALOG {
        for directory_id in &absent {
            for operation in ["EnableLDAPS", "DisableLDAPS"] {
                let case = format!("{operation} against {directory_id}");
                let body = json!({ "DirectoryId": directory_id, "Type": ldaps_type });
                refused(
                    &router,
                    operation,
                    body.clone(),
                    "DirectoryDoesNotExistException",
                    &case,
                )
                .await;
                // The refusal is built from the request: it quotes the id the caller
                // asked for, so the caller can fix the request from the message alone.
                let (_, parsed) = call(&router, operation, body).await;
                let message = parsed["message"]
                    .as_str()
                    .or_else(|| parsed["Message"].as_str())
                    .unwrap_or_default();
                assert!(
                    message.contains(&format!("'{directory_id}'")),
                    "case {case:?}: expected the message to name the requested directory, \
                     got {parsed}"
                );
            }
        }

        // A directory still being built by the transition its create owes is not in
        // service, so neither operation reaches its LDAP security.
        let building = create_microsoft_ad(
            &router,
            minimal_microsoft_ad("building.example.com"),
            "still being built",
        )
        .await;
        for operation in ["EnableLDAPS", "DisableLDAPS"] {
            refused(
                &router,
                operation,
                json!({ "DirectoryId": &building, "Type": ldaps_type }),
                "DirectoryUnavailableException",
                &format!("{operation} on a directory still being built"),
            )
            .await;
        }

        // A Simple AD directory serves no LDAPS endpoint, so there is no switch to
        // activate or deactivate for it however long it has been in service.
        let simple = create_directory(
            &router,
            minimal_create("simple.example.com"),
            "a type that serves no LDAPS endpoint",
        )
        .await;
        await_in_service(&router, &simple, "a type that serves no LDAPS endpoint").await;
        for operation in ["EnableLDAPS", "DisableLDAPS"] {
            refused(
                &router,
                operation,
                json!({ "DirectoryId": &simple, "Type": ldaps_type }),
                "UnsupportedOperationException",
                &format!("{operation} on a type that serves no LDAPS endpoint"),
            )
            .await;
        }
        assert_eq!(
            ldaps_status(
                &router,
                &simple,
                ldaps_type,
                "a type that serves no LDAPS endpoint"
            )
            .await,
            None,
            "case {:?}: expected the refusals to record no setting",
            "a type that serves no LDAPS endpoint"
        );
    }
}

#[tokio::test]
async fn a_deactivation_needs_ldap_security_that_is_active() {
    let router = service();

    for ldaps_type in LDAPS_TYPE_CATALOG {
        let directory_id = directory_in_service(&router, "cycle.example.com", "cycle").await;
        certificate_in_service(
            &router,
            &directory_id,
            "cycle.example.com",
            TYPE_CLIENT_LDAPS,
            "cycle",
        )
        .await;

        // Nothing has activated LDAP security, so there is no switch to turn off.
        refused(
            &router,
            "DisableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "InvalidLDAPSStatusException",
            "never activated",
        )
        .await;
        assert_eq!(
            ldaps_status(&router, &directory_id, ldaps_type, "never activated").await,
            None,
            "case {:?}: expected the refused deactivation to record no setting",
            "never activated"
        );

        // LDAP security the activation just put in force is what there is a switch to
        // turn off, so the deactivation is admitted on the very next call and takes
        // effect at once: the modelled status enum carries no transient value either way.
        enable(&router, &directory_id, ldaps_type, "activated").await;
        let disabled_from = epoch_seconds(SystemTime::now());
        disable(&router, &directory_id, ldaps_type, "deactivated").await;
        let disabled_until = epoch_seconds(SystemTime::now());
        let recorded = required_setting(&router, &directory_id, ldaps_type, "deactivated")
            .await;
        assert_eq!(
            recorded["LDAPSStatus"],
            json!(STATUS_DISABLED),
            "case {:?}: expected the deactivation to take effect at once, got {recorded}",
            "deactivated"
        );
        let updated = recorded["LastUpdatedDateTime"].as_f64().unwrap_or_default();
        assert!(
            (disabled_from - 1.0..=disabled_until + 1.0).contains(&updated),
            "case {:?}: expected LastUpdatedDateTime within the deactivation window \
             [{disabled_from}, {disabled_until}], got {updated}",
            "deactivated"
        );

        // The clock owes a deactivated setting nothing: it reads the same however many
        // reads come after it.
        assert_eq!(
            ldaps_status(&router, &directory_id, ldaps_type, "deactivated")
                .await
                .as_deref(),
            Some(STATUS_DISABLED),
            "case {:?}: expected the inactive status to stay put",
            "deactivated"
        );

        // A second deactivation asks for the state the setting is already in.
        refused(
            &router,
            "DisableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "InvalidLDAPSStatusException",
            "deactivated twice",
        )
        .await;

        // A deactivated switch can be activated again, and the cycle puts it back in
        // force the same way the first activation did.
        enable(&router, &directory_id, ldaps_type, "reactivated").await;
        assert_eq!(
            ldaps_status(&router, &directory_id, ldaps_type, "reactivated")
                .await
                .as_deref(),
            Some(STATUS_ENABLED),
            "case {:?}: expected the reactivation to be in force at once",
            "reactivated"
        );
        refused(
            &router,
            "EnableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "InvalidLDAPSStatusException",
            "reactivated twice",
        )
        .await;
        assert_eq!(
            ldaps_status(&router, &directory_id, ldaps_type, "reactivated")
                .await
                .as_deref(),
            Some(STATUS_ENABLED),
            "case {:?}: expected the refused second activation to leave it active",
            "reactivated"
        );
    }
}

#[tokio::test]
async fn ldap_security_is_recorded_per_directory() {
    let router = service();

    let ldaps_type = LDAPS_TYPE_CATALOG[0];
    let first = directory_in_service(&router, "first.example.com", "per directory").await;
    let second = directory_in_service(&router, "second.example.com", "per directory").await;
    for directory_id in [&first, &second] {
        certificate_in_service(
            &router,
            directory_id,
            "shared.example.com",
            TYPE_CLIENT_LDAPS,
            "per directory",
        )
        .await;
    }

    enable(&router, &first, ldaps_type, "per directory").await;

    // The activation reached the directory it named and no other.
    assert_eq!(
        ldaps_status(&router, &first, ldaps_type, "per directory")
            .await
            .as_deref(),
        Some(STATUS_ENABLED),
        "case {:?}: expected the named directory's LDAP security to be active",
        "per directory"
    );
    assert_eq!(
        ldaps_status(&router, &second, ldaps_type, "per directory").await,
        None,
        "case {:?}: expected the other directory to hold no setting",
        "per directory"
    );

    // ...so deactivating it leaves the other directory's switch where it was, and the
    // other directory's own activation is independent.
    disable(&router, &first, ldaps_type, "per directory").await;
    enable(&router, &second, ldaps_type, "per directory").await;
    assert_eq!(
        ldaps_status(&router, &first, ldaps_type, "per directory")
            .await
            .as_deref(),
        Some(STATUS_DISABLED),
        "case {:?}: expected the deactivated directory to stay inactive",
        "per directory"
    );
    assert_eq!(
        ldaps_status(&router, &second, ldaps_type, "per directory")
            .await
            .as_deref(),
        Some(STATUS_ENABLED),
        "case {:?}: expected the other directory's own switch to be in force",
        "per directory"
    );
}

/// Which directory type a row of the switch table drives, both of which serve an LDAPS
/// endpoint (`state::LDAPS_SUPPORTED_DIRECTORY_TYPES`).
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `CreateMicrosoftAD`: AWS Managed Microsoft AD.
    MicrosoftAd,
    /// `ConnectDirectory`: an AD Connector.
    Connector,
}

/// One row of the switch table: the directory the switch is carried by, how many
/// certificates securing LDAP calls it holds, and how many activate/deactivate cycles it
/// is driven through.
struct SwitchCase {
    case: &'static str,
    kind: Kind,
    certificates: usize,
    cycles: usize,
}

/// The row's directory, created but not yet looked for: the rows are all created before
/// any of them is waited on, so their build windows overlap instead of adding up.
async fn switch_directory(
    router: &MultiServiceRouter,
    kind: Kind,
    name: &str,
    case: &str,
) -> String {
    match kind {
        Kind::MicrosoftAd => create_microsoft_ad(router, minimal_microsoft_ad(name), case).await,
        Kind::Connector => connect_directory(router, minimal_connect(name), case).await,
    }
}

/// The status reported for one directory's switch, which the case requires to be recorded.
async fn recorded_status(
    router: &MultiServiceRouter,
    directory_id: &str,
    ldaps_type: &str,
    case: &str,
) -> String {
    ldaps_status(router, directory_id, ldaps_type, case)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a recorded LDAPS setting"))
}

/// RULE: both toggles switch a setting of a directory this service already holds — neither
/// is modelled with a single output member (`smithy_query(query="operation",
/// operation="EnableLDAPS")` — input `{DirectoryId, Type}`, output `{}`), so neither hands
/// the caller a status to wait out — so each is applied by the call that makes it: the
/// FIRST `DescribeLDAPSSettings` after a toggle reports the position it put the switch in,
/// and nothing, no later call and no clock, moves it afterwards.
///
/// The rows span the class rather than one recorded call: both directory types that serve
/// an LDAPS endpoint, differing in how many certificates securing LDAP calls the directory
/// holds, each driven through a different NUMBER of cycles, with a directory in the same
/// store that is never toggled as the control — so a switch that is not per-directory, or
/// that needs a wait anywhere in the cycle, cannot pass.
#[tokio::test]
async fn every_ldaps_switch_takes_effect_in_the_call_that_makes_it() {
    for ldaps_type in LDAPS_TYPE_CATALOG {
        let router = service();

        let rows = [
            SwitchCase {
                case: "a Managed Microsoft AD with one certificate, one cycle",
                kind: Kind::MicrosoftAd,
                certificates: 1,
                cycles: 1,
            },
            SwitchCase {
                case: "a Managed Microsoft AD with three certificates, two cycles",
                kind: Kind::MicrosoftAd,
                certificates: 3,
                cycles: 2,
            },
            SwitchCase {
                case: "an AD Connector with one certificate, three cycles",
                kind: Kind::Connector,
                certificates: 1,
                cycles: 3,
            },
            SwitchCase {
                case: "the control directory, never toggled",
                kind: Kind::MicrosoftAd,
                certificates: 1,
                cycles: 0,
            },
        ];

        // Every row's directory lives in ONE store, so a switch that is not per-directory
        // cannot pass. All of them are created first and looked for afterwards, so the one
        // transition the clock owes here runs for every row at once.
        let mut created: Vec<(&SwitchCase, String)> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let id = switch_directory(
                &router,
                row.kind,
                &format!("switch{index}.example.com"),
                row.case,
            )
            .await;
            created.push((row, id));
        }
        for (row, id) in &created {
            await_in_service(&router, id, row.case).await;
            for certificate in 0..row.certificates {
                certificate_in_service(
                    &router,
                    id,
                    &format!("ldaps-{certificate}.example.com"),
                    TYPE_CLIENT_LDAPS,
                    row.case,
                )
                .await;
            }

            // Nothing has activated LDAP security yet, so the store holds no setting for
            // the directory at all.
            let before = ldaps_status(&router, id, ldaps_type, row.case).await;
            assert_eq!(
                before, None,
                "case {:?}: expected no LDAPS setting before any activation, got {before:?}",
                row.case
            );
        }

        for (row, id) in &created {
            for cycle in 1..=row.cycles {
                let case = format!("{} (cycle {cycle} activation)", row.case);
                enable(&router, id, ldaps_type, &case).await;
                let reported = recorded_status(&router, id, ldaps_type, &case).await;
                assert_eq!(
                    reported, STATUS_ENABLED,
                    "case {case:?}: expected the activation to be in force on the next \
                     read, got {reported:?}"
                );

                // A second activation asks for the position the first one already put the
                // switch in, and leaves it there.
                refused(
                    &router,
                    "EnableLDAPS",
                    json!({ "DirectoryId": id, "Type": ldaps_type }),
                    "InvalidLDAPSStatusException",
                    &case,
                )
                .await;
                let after_refusal = recorded_status(&router, id, ldaps_type, &case).await;
                assert_eq!(
                    after_refusal, STATUS_ENABLED,
                    "case {case:?}: expected the refused second activation to leave the \
                     switch in force, got {after_refusal:?}"
                );

                let case = format!("{} (cycle {cycle} deactivation)", row.case);
                disable(&router, id, ldaps_type, &case).await;
                let reported = recorded_status(&router, id, ldaps_type, &case).await;
                assert_eq!(
                    reported, STATUS_DISABLED,
                    "case {case:?}: expected the deactivation to be in force on the next \
                     read, got {reported:?}"
                );
                refused(
                    &router,
                    "DisableLDAPS",
                    json!({ "DirectoryId": id, "Type": ldaps_type }),
                    "InvalidLDAPSStatusException",
                    &case,
                )
                .await;
            }
        }

        // One closing activation per toggled row, then every row is read again with the
        // other rows' calls in between: each switch is exactly where its own last call
        // left it — the toggled ones in force, the control holding no setting — because a
        // toggle leaves nothing for a later call or the clock to settle.
        for (row, id) in &created {
            if row.cycles > 0 {
                let case = format!("{} (the closing activation)", row.case);
                enable(&router, id, ldaps_type, &case).await;
            }
        }
        for (row, id) in &created {
            let case = format!("{} (read again after every row's calls)", row.case);
            let reported = ldaps_status(&router, id, ldaps_type, &case).await;
            let expected = if row.cycles > 0 {
                Some(STATUS_ENABLED)
            } else {
                None
            };
            assert_eq!(
                reported.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }
}

/// RULE: the switch waits for the transitions the clock DOES owe — the directory's build
/// and the registration of the certificate it is served by — and for nothing else: refused
/// from both until they have finished, in force on the read after the call that is
/// admitted.
#[tokio::test]
async fn the_switch_waits_for_the_build_and_the_certificate_and_for_nothing_else() {
    for ldaps_type in LDAPS_TYPE_CATALOG {
        let router = service();
        let case = "a directory still being built";
        let directory_id = create_microsoft_ad(
            &router,
            minimal_microsoft_ad("waits.example.com"),
            case,
        )
        .await;

        // The create answers while the directory's servers are still coming up, so the
        // activation is refused for the directory being out of service.
        refused(
            &router,
            "EnableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "DirectoryUnavailableException",
            case,
        )
        .await;

        // Once the build has finished the refusal moves on to what the switch is served
        // by: a directory holding no certificate in service securing LDAP calls.
        await_in_service(&router, &directory_id, "a directory whose build has finished").await;
        let case = "a directory holding no certificate";
        refused(
            &router,
            "EnableLDAPS",
            json!({ "DirectoryId": &directory_id, "Type": ldaps_type }),
            "NoAvailableCertificateException",
            case,
        )
        .await;
        let refused_status = ldaps_status(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            refused_status, None,
            "case {case:?}: expected the refused activations to record no setting, got \
             {refused_status:?}"
        );

        // With both of those transitions behind it, the very same call is admitted and the
        // switch is in force for the request that follows it...
        let case = "a directory with its certificate registered";
        certificate_in_service(
            &router,
            &directory_id,
            "waits.example.com",
            TYPE_CLIENT_LDAPS,
            case,
        )
        .await;
        enable(&router, &directory_id, ldaps_type, case).await;
        let reported = recorded_status(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            reported, STATUS_ENABLED,
            "case {case:?}: expected the admitted activation to be in force at once, got \
             {reported:?}"
        );

        // ...and the deactivation that follows it the same way.
        disable(&router, &directory_id, ldaps_type, case).await;
        let reported = recorded_status(&router, &directory_id, ldaps_type, case).await;
        assert_eq!(
            reported, STATUS_DISABLED,
            "case {case:?}: expected the deactivation to be in force at once, got \
             {reported:?}"
        );
    }
}
