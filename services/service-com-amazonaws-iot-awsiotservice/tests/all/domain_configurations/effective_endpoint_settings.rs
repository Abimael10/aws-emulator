// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what a domain configuration's reader reports for the
//! settings the endpoint ALWAYS has, and of the ARN a configuration carries, driven
//! against the `CreateDomainConfiguration` / `DescribeDomainConfiguration` /
//! `UpdateDomainConfiguration` helpers of the parent suite.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * an endpoint serves under SOME TLS security policy, speaks SOME application protocol
//!   and accepts SOME authentication type, so a request that pins none of them is served
//!   the service's defaults — `DEFAULT` for the two enums, whose documentation is "Use a
//!   combination of port and Application Layer Protocol Negotiation (ALPN) to specify …",
//!   and the service's current default policy for TLS — and those are the values the
//!   reader reports, not absent members;
//! * which domain a configuration configures is likewise always reported: the name the
//!   request gave, or — for a configuration of the AWS-managed domain, which names none —
//!   the account's own data endpoint;
//! * a configuration's ARN names it by its NAME
//!   (`arn:aws:iot:<region>:<account>:domainconfiguration/<domainConfigurationName>`), so
//!   it is derivable from the name by anyone writing an IAM resource statement, for every
//!   name `DomainConfigurationName` (`^[\w.-]+$`, 1..128) admits;
//! * an update replaces the settings it carries and leaves the rest, and one that carries
//!   a TLS configuration pinning no policy is served the default exactly as a create is.

use super::*;

/// The security policy a configuration nobody pinned one for serves TLS under.
const DEFAULT_SECURITY_POLICY: &str = "IoTSecurityPolicy_TLS13_1_2_2022_10";

/// One row of the settings table: what the create pinned, and what the reader must
/// report for each of the three settings an endpoint always has.
struct SettingsCase {
    case: &'static str,
    body: Value,
    application_protocol: &'static str,
    authentication_type: &'static str,
    security_policy: &'static str,
}

/// RULE: the settings an endpoint always has are reported whether or not the request
/// pinned them.
#[tokio::test]
async fn the_settings_an_endpoint_always_has_are_always_reported() {
    let emulator = Emulator::new();

    // The rows span which of the three the request pins — none, each one alone, all
    // three — and, for the values, the members' own catalogs
    // (`smithy_query(query="shape",shape="ApplicationProtocol")`: SECURE_MQTT | MQTT_WSS
    // | HTTPS | DEFAULT; `AuthenticationType`: CUSTOM_AUTH_X509 | CUSTOM_AUTH | AWS_X509
    // | AWS_SIGV4 | DEFAULT) plus a published security policy that is not the default.
    let rows = [
        SettingsCase {
            case: "nothing pinned",
            body: json!({}),
            application_protocol: "DEFAULT",
            authentication_type: "DEFAULT",
            security_policy: DEFAULT_SECURITY_POLICY,
        },
        SettingsCase {
            case: "only the application protocol pinned",
            body: json!({ "applicationProtocol": "MQTT_WSS" }),
            application_protocol: "MQTT_WSS",
            authentication_type: "DEFAULT",
            security_policy: DEFAULT_SECURITY_POLICY,
        },
        SettingsCase {
            case: "only the authentication type pinned",
            body: json!({ "authenticationType": "AWS_SIGV4" }),
            application_protocol: "DEFAULT",
            authentication_type: "AWS_SIGV4",
            security_policy: DEFAULT_SECURITY_POLICY,
        },
        SettingsCase {
            case: "only the security policy pinned",
            body: json!({ "tlsConfig": { "securityPolicy": "IoTSecurityPolicy_TLS12_1_2_2019_07" } }),
            application_protocol: "DEFAULT",
            authentication_type: "DEFAULT",
            security_policy: "IoTSecurityPolicy_TLS12_1_2_2019_07",
        },
        SettingsCase {
            case: "a TLS configuration pinning no policy",
            body: json!({ "tlsConfig": {} }),
            application_protocol: "DEFAULT",
            authentication_type: "DEFAULT",
            security_policy: DEFAULT_SECURITY_POLICY,
        },
        SettingsCase {
            case: "all three pinned",
            body: json!({
                "applicationProtocol": "HTTPS",
                "authenticationType": "AWS_X509",
                "tlsConfig": { "securityPolicy": "IoTSecurityPolicy_TLS13_1_2_2022_10" },
            }),
            application_protocol: "HTTPS",
            authentication_type: "AWS_X509",
            security_policy: DEFAULT_SECURITY_POLICY,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("settings-{index}");
        let created = emulator.create_configuration(&name, &row.body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let read = emulator.describe_configuration(&name).await;
        let described = read.json();
        assert_eq!(
            described["applicationProtocol"],
            json!(row.application_protocol),
            "case {case}: expected applicationProtocol {}, got {described}",
            row.application_protocol
        );
        assert_eq!(
            described["authenticationType"],
            json!(row.authentication_type),
            "case {case}: expected authenticationType {}, got {described}",
            row.authentication_type
        );
        assert_eq!(
            described["tlsConfig"],
            json!({ "securityPolicy": row.security_policy }),
            "case {case}: expected the security policy {}, got {described}",
            row.security_policy
        );
    }
}

/// RULE: which domain a configuration configures is always reported — the name the
/// request gave, or the account's own data endpoint for a configuration of the
/// AWS-managed domain.
#[tokio::test]
async fn the_domain_a_configuration_configures_is_always_reported() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("domain name", &certificate_pem, &private_key_pem)
        .await;
    let trusted = trusted_validation_certificate(&emulator, "domain name").await;
    let account_endpoint = emulator.account_data_endpoint().await;

    // The rows span both kinds of domain and, for the customer-managed kind, the forms
    // `DomainName` (len 1..253, pattern `^[\s\S]*$`) admits: a plain host, a deep
    // subdomain, a mixed-case one and one at the modelled length.
    let long_label = "a".repeat(60);
    let long_domain = format!("{long_label}.{long_label}.{long_label}.example.com");
    let rows: Vec<(&str, Option<String>)> = vec![
        ("the AWS-managed domain", None),
        ("a plain host", Some("iot.example.com".to_string())),
        (
            "a deep subdomain",
            Some("devices.eu.iot.example.com".to_string()),
        ),
        ("a mixed-case host", Some("IoT.Example.COM".to_string())),
        ("a long host", Some(long_domain.clone())),
    ];

    for (index, (case, domain_name)) in rows.iter().enumerate() {
        let name = format!("domain-name-{index}");
        let mut body = json!({});
        if let Some(domain_name) = domain_name {
            body["domainName"] = json!(domain_name);
            body["serverCertificateArns"] = json!([held.clone()]);
            body["validationCertificateArn"] = json!(trusted.clone());
        }
        let created = emulator.create_configuration(&name, &body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let read = emulator.describe_configuration(&name).await;
        let described = read.json();
        let expected = domain_name.clone().unwrap_or_else(|| account_endpoint.clone());
        assert_eq!(
            described["domainName"],
            json!(expected),
            "case {case}: expected the domain {expected}, got {described}"
        );
        assert_eq!(
            described["domainType"],
            json!(if domain_name.is_some() {
                "CUSTOMER_MANAGED"
            } else {
                "AWS_MANAGED"
            }),
            "case {case}: the kind follows from whether the request named a domain, got \
             {described}"
        );
    }
}

/// RULE: a configuration's ARN names it by its NAME, for every name the create admits.
#[tokio::test]
async fn the_arn_of_a_configuration_names_it_by_its_name() {
    let emulator = Emulator::new();

    // The rows span the forms `DomainConfigurationName` (`^[\w.-]+$`, len 1..128)
    // admits: one character, dots, dashes, underscores, digits, mixed case and the
    // modelled maximum length.
    let longest = "n".repeat(128);
    let rows = [
        "a",
        "with.dots.in.it",
        "with-dashes-in-it",
        "with_underscores",
        "Mixed.Case-9",
        longest.as_str(),
    ];
    let mut minted: Vec<String> = Vec::new();
    for case in rows {
        let created = emulator
            .create_configuration(case, &json!({ "serviceType": "DATA" }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let reported = created.json()["domainConfigurationArn"].clone();
        assert_created_arn(case, case, &reported);
        let read = emulator.describe_configuration(case).await;
        assert_eq!(
            read.json()["domainConfigurationArn"], reported,
            "case {case}: expected the read to report the ARN the create minted, got {}",
            read.text()
        );
        minted.push(reported.to_string());
    }
    // Two configurations never share an ARN, because each ARN carries its own name.
    let mut distinct = minted.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "case every name: expected one ARN per name, got {minted:?}"
    );
}

/// RULE: an update replaces the settings it carries and leaves every other one as the
/// record holds it.
#[tokio::test]
async fn an_update_replaces_the_settings_it_carries() {
    let emulator = Emulator::new();
    let name = "updated.config";
    let created = emulator
        .create_configuration(
            name,
            &json!({
                "applicationProtocol": "MQTT_WSS",
                "authenticationType": "AWS_SIGV4",
                "tlsConfig": { "securityPolicy": "IoTSecurityPolicy_TLS12_1_2_2019_07" },
            }),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case the configuration to update: expected 200, got {} body {}",
        created.status,
        created.text()
    );

    // Each row updates ONE setting and states what the whole record must read as
    // afterwards, so what an update leaves alone is asserted as well as what it changes.
    let rows = [
        (
            "the application protocol alone",
            json!({ "applicationProtocol": "HTTPS" }),
            json!("HTTPS"),
            json!("AWS_SIGV4"),
            json!({ "securityPolicy": "IoTSecurityPolicy_TLS12_1_2_2019_07" }),
        ),
        (
            "the authentication type alone",
            json!({ "authenticationType": "AWS_X509" }),
            json!("HTTPS"),
            json!("AWS_X509"),
            json!({ "securityPolicy": "IoTSecurityPolicy_TLS12_1_2_2019_07" }),
        ),
        (
            "a TLS configuration pinning no policy",
            json!({ "tlsConfig": {} }),
            json!("HTTPS"),
            json!("AWS_X509"),
            json!({ "securityPolicy": DEFAULT_SECURITY_POLICY }),
        ),
        (
            "nothing at all",
            json!({}),
            json!("HTTPS"),
            json!("AWS_X509"),
            json!({ "securityPolicy": DEFAULT_SECURITY_POLICY }),
        ),
    ];
    for (case, body, application_protocol, authentication_type, tls_config) in &rows {
        let updated = emulator.update_configuration(name, body).await;
        assert_eq!(
            updated.status,
            200,
            "case {case}: expected the update to answer 200, got {} body {}",
            updated.status,
            updated.text()
        );
        assert_eq!(
            updated.json()["domainConfigurationArn"],
            created.json()["domainConfigurationArn"],
            "case {case}: the update must report the configuration's own ARN, got {}",
            updated.text()
        );
        let described = emulator.describe_configuration(name).await.json();
        assert_eq!(
            described["applicationProtocol"], *application_protocol,
            "case {case}: expected applicationProtocol {application_protocol}, got {described}"
        );
        assert_eq!(
            described["authenticationType"], *authentication_type,
            "case {case}: expected authenticationType {authentication_type}, got {described}"
        );
        assert_eq!(
            described["tlsConfig"], *tls_config,
            "case {case}: expected tlsConfig {tls_config}, got {described}"
        );
    }
}
