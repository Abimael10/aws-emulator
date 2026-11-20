// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the domain configurations the SERVICE manages for the
//! account — the settings of the account's own endpoints, which exist without anyone
//! creating them — driven against the `CreateDomainConfiguration` /
//! `DescribeDomainConfiguration` / `ListDomainConfigurations` /
//! `UpdateDomainConfiguration` helpers of the parent suite.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * an account holds the configuration of each of its own endpoints from the moment it
//!   exists, addressable by that endpoint's RESERVED name — which is why every
//!   single-configuration reader takes a `ReservedDomainConfigurationName` whose pattern
//!   `^[\w.:-]+$` admits the `:` such a name carries, where the create and the delete take
//!   a `DomainConfigurationName` (`^[\w.-]+$`) that cannot spell one
//!   (`smithy_query(query="operation",operation="DescribeDomainConfiguration")`);
//! * such a configuration reports every member the response models, of `ENDPOINT` domain
//!   type, with the ARN naming it by its reserved name, and the account's own data
//!   endpoint as the domain it configures;
//! * it is reported by the listing beside the configurations a caller created, and reading
//!   it changes nothing a later caller sees;
//! * it is not settings a caller supplied, so it cannot be updated — "Domain
//!   configurations for default endpoints can't be updated"
//!   (`UpdateDomainConfiguration`) — and a name that merely LOOKS like a reserved one is
//!   no configuration of the account at all.

use super::*;

/// The members `DescribeDomainConfigurationResponse` models for a configuration of one of
/// the account's own endpoints, as a real account reports them: the endpoint serves
/// connections, delivers its endpoint's service, presents no certificate of the
/// customer's, and is pinned to no protocol or authentication type
/// (`smithy_query(query="operation",operation="DescribeDomainConfiguration")`).
const ENDPOINT_DOMAIN_TYPE: &str = "ENDPOINT";
/// The security policy a configuration nobody pinned one for serves TLS under.
const DEFAULT_SECURITY_POLICY: &str = "IoTSecurityPolicy_TLS13_1_2_2022_10";

/// RULE: every configuration the service manages for the account is readable under its
/// reserved name, reporting the modelled members of an `ENDPOINT` configuration, and a
/// read of one changes nothing a later caller sees.
#[tokio::test]
async fn every_reserved_configuration_is_readable_under_its_reserved_name() {
    let emulator = Emulator::new();
    let account_endpoint = emulator.account_data_endpoint().await;

    for reserved in RESERVED {
        let case = reserved.name;
        let read = emulator.describe_configuration(case).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: expected the account to hold this configuration, got {} body {}",
            read.status,
            read.text()
        );
        let described = read.json();
        assert_eq!(
            described["domainConfigurationName"],
            json!(case),
            "case {case}: the read must name the configuration, got {described}"
        );
        assert_eq!(
            described["domainConfigurationArn"],
            json!(format!(
                "arn:aws:iot:{REGION}:{ACCOUNT_ID}:domainconfiguration/{case}"
            )),
            "case {case}: the ARN must name the configuration by its name, got {described}"
        );
        assert_eq!(
            described["domainType"],
            json!(ENDPOINT_DOMAIN_TYPE),
            "case {case}: expected an {ENDPOINT_DOMAIN_TYPE} configuration, got {described}"
        );
        assert_eq!(
            described["serviceType"],
            json!(reserved.service_type),
            "case {case}: expected service type {}, got {described}",
            reserved.service_type
        );
        assert_eq!(
            described["domainConfigurationStatus"],
            json!("ENABLED"),
            "case {case}: the account's own endpoint serves connections, got {described}"
        );
        assert_eq!(
            described["authenticationType"],
            json!("DEFAULT"),
            "case {case}: expected the default authentication type, got {described}"
        );
        assert_eq!(
            described["applicationProtocol"],
            json!("DEFAULT"),
            "case {case}: expected the default application protocol, got {described}"
        );
        assert_eq!(
            described["tlsConfig"],
            json!({ "securityPolicy": DEFAULT_SECURITY_POLICY }),
            "case {case}: expected the service's default security policy, got {described}"
        );
        assert_eq!(
            described["serverCertificates"],
            json!([]),
            "case {case}: an endpoint AWS runs presents no certificate of the customer's, got \
             {described}"
        );
        assert_eq!(
            described["domainName"],
            json!(account_endpoint),
            "case {case}: expected the account's own data endpoint as the domain, got {described}"
        );
        assert!(
            described["lastStatusChangeDate"].is_number(),
            "case {case}: the read must report when the status last changed, got {described}"
        );

        // A read reports the account as it IS: the same read twice answers identically,
        // the instant the status last changed included, and the account holds no new
        // configuration because somebody read one.
        let listed_before = emulator.list_configurations("").await.json();
        let again = emulator.describe_configuration(case).await;
        assert_eq!(
            again.json(),
            described,
            "case {case}: two identical reads must answer identically, got {}",
            again.text()
        );
        let listed_after = emulator.list_configurations("").await;
        assert_eq!(
            listed_after.json(),
            listed_before,
            "case {case}: reading a configuration must change no listing, got {}",
            listed_after.text()
        );
    }
}

/// RULE: the listing reports the configurations the service manages beside the ones the
/// account created, each projected from the same record the reader projects.
#[tokio::test]
async fn a_reserved_configuration_is_listed_beside_the_accounts_own() {
    let emulator = Emulator::new();

    // An account nobody created a configuration in still holds the ones the service
    // manages for it.
    let listed = emulator.list_configurations("").await;
    let names: Vec<String> = summary_names(&listed);
    let reserved_names: Vec<String> = RESERVED
        .iter()
        .map(|reserved| reserved.name.to_string())
        .collect();
    assert_eq!(
        names, reserved_names,
        "case an account with no configurations of its own: expected {reserved_names:?}, got \
         {names:?}"
    );

    // ... and one a caller creates is listed beside them, in the one order the listing
    // pages over.
    for name in ["aaa-first", "zzz-last"] {
        let created = emulator
            .create_configuration(name, &json!({ "serviceType": "DATA" }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {name}: the configuration must be creatable, got {} body {}",
            created.status,
            created.text()
        );
    }
    let listed = emulator.list_configurations("").await;
    let names = summary_names(&listed);
    let mut expected = reserved_names.clone();
    expected.push("aaa-first".to_string());
    expected.push("zzz-last".to_string());
    assert_eq!(
        names, expected,
        "case the whole listing: expected {expected:?}, got {names:?}"
    );

    // Each summary carries what the single-configuration reader reports for the same
    // name, reserved or not.
    for name in &names {
        let read = emulator.describe_configuration(name).await;
        let summary = listed.json()["domainConfigurations"]
            .as_array()
            .unwrap_or_else(|| panic!("no domainConfigurations in {}", listed.text()))
            .iter()
            .find(|summary| summary["domainConfigurationName"] == json!(name))
            .cloned()
            .unwrap_or_else(|| panic!("case {name}: not in {}", listed.text()));
        assert_eq!(
            summary["domainConfigurationArn"], read.json()["domainConfigurationArn"],
            "case {name}: the summary must carry the ARN the reader reports, got {summary}"
        );
        assert_eq!(
            summary["serviceType"], read.json()["serviceType"],
            "case {name}: the summary must carry the service type the reader reports, got \
             {summary}"
        );
    }
}

/// RULE: a configuration the service manages is the account's own endpoint and not
/// settings a caller supplied, so an update addressed to one is refused and changes
/// nothing.
#[tokio::test]
async fn a_reserved_configuration_cannot_be_deleted() {
    let emulator = Emulator::new();
    for reserved in RESERVED {
        let case = reserved.name;
        let deleted = emulator.delete_configuration(case).await;
        assert!(
            (400..500).contains(&deleted.status),
            "case {case}: expected the delete of a service-managed configuration to be refused, \
             got {} body {}",
            deleted.status,
            deleted.text()
        );
        assert_eq!(
            emulator.describe_configuration(case).await.status,
            200,
            "case {case}: the configuration must still be there"
        );
    }
}

/// RULE: a configuration the service manages is the account's own endpoint and not
/// settings a caller supplied, so an update addressed to one is refused and changes
/// nothing.
#[tokio::test]
async fn a_reserved_configuration_cannot_be_updated() {
    let emulator = Emulator::new();

    // The rows span what an update can ask of a configuration: a status change, a TLS
    // policy, an authentication type and the removal of an authorizer configuration.
    let rows = [
        ("the status", json!({ "domainConfigurationStatus": "DISABLED" })),
        (
            "the security policy",
            json!({ "tlsConfig": { "securityPolicy": "IoTSecurityPolicy_TLS12_1_2_2019_07" } }),
        ),
        (
            "the authentication type",
            json!({ "authenticationType": "AWS_SIGV4" }),
        ),
        (
            "the authorizer configuration",
            json!({ "removeAuthorizerConfig": true }),
        ),
    ];
    for reserved in RESERVED {
        let before = emulator.describe_configuration(reserved.name).await.json();
        for (asked, body) in &rows {
            let case = format!("{} of {}", asked, reserved.name);
            let updated = emulator.update_configuration(reserved.name, body).await;
            assert_eq!(
                updated.status,
                400,
                "case {case}: expected the update to be refused with 400, got {} body {}",
                updated.status,
                updated.text()
            );
            assert_eq!(
                updated.error_code(),
                "InvalidRequestException",
                "case {case}: got {}",
                updated.text()
            );
            assert!(
                updated.error_message().contains(reserved.name),
                "case {case}: the refusal must name the configuration, got {}",
                updated.error_message()
            );
            assert_eq!(
                emulator.describe_configuration(reserved.name).await.json(),
                before,
                "case {case}: a refused update must change nothing"
            );
        }
    }
}

/// RULE: a name that merely LOOKS like one of the service's own is no configuration of
/// the account, so a read of it refuses exactly as any other miss does.
#[tokio::test]
async fn a_name_that_is_no_reserved_name_is_still_a_miss() {
    let emulator = Emulator::new();

    // The rows span the ways a name can miss the reserved ones while still satisfying
    // `ReservedDomainConfigurationName` (`^[\w.:-]+$`): a different case, a prefix, a
    // suffix, another namespace and a name carrying no `:` at all.
    let reserved = RESERVED
        .first()
        .expect("the service manages at least one configuration")
        .name;
    let cases = [
        reserved.to_ascii_lowercase(),
        reserved.to_ascii_uppercase(),
        reserved.trim_end_matches("-ATS").to_string(),
        format!("{reserved}-extra"),
        format!("not{reserved}"),
        reserved.replace(':', "-"),
    ];
    for case in &cases {
        if case.as_str() == reserved {
            continue;
        }
        let read = emulator.describe_configuration(case).await;
        assert_eq!(
            read.status,
            404,
            "case {case}: expected a miss to answer 404, got {} body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case}: got {}",
            read.text()
        );
        assert_eq!(
            read.error_message(),
            format!(
                "Could not find domain configuration for given account ID and domain \
                 configuration name, AccountId: {ACCOUNT_ID}, DomainConfigurationName: {case}"
            ),
            "case {case}: got {}",
            read.error_message()
        );
    }
}

/// The names one listing reports, in the order it reports them.
fn summary_names(listed: &Reply) -> Vec<String> {
    listed.json()["domainConfigurations"]
        .as_array()
        .unwrap_or_else(|| panic!("no domainConfigurations in {}", listed.text()))
        .iter()
        .map(|summary| {
            summary["domainConfigurationName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}
