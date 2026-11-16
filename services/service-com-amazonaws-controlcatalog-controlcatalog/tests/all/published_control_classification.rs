// SPDX-License-Identifier: CC0-1.0

//! How a published control is CLASSIFIED when the catalog serves it.
//!
//! The rule under test: the Control Catalog is an Amazon Web Services-published,
//! account- and Region-independent read-only dataset, so the classification of a control
//! — the opaque identifier its published ARN carries, the designators it is published
//! under (`Aliases`), its `Severity`, the `GovernedResources` it governs and the
//! `ParameterRequirementSummary` of the parameters the implementing surface takes — is
//! read from that control's published record. None of the five is defaulted, derived from
//! the control's wording or minted by this emulator, and none of them depends on how the
//! control was selected or paged. `Severity` admits `LOW|MEDIUM|HIGH|CRITICAL` and
//! `ParameterRequirementSummary` `REQUIRED|OPTIONAL|NONE`, `GovernedResources` is "[a]
//! list of Amazon Web Services resource types that are governed by this control" and "[i]f
//! GovernedResources cannot be represented by available resource types, it's returned as
//! an empty list" (`smithy_query(query="shape", shape="ControlSummary",
//! model_path="services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json")`).
//!
//! The first test is a TABLE spanning the class rather than one control picked by hand:
//! its rows differ in designator family (`CONFIG.…`, `AWS-GR_…`, `SH.…`), implementing
//! surface, every severity the modeled enum admits, both parameter summaries an
//! implemented control carries here, and an empty against a non-empty governed-resource
//! list — including controls no recorded page named. A row also records the published name
//! or the published release instant where the catalog's own value for it is recorded, so a
//! control's `Name` and `CreateTime` are read against the catalog too. The remaining tests
//! take EVERY control the service serves.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::control_catalog::{catalog_arn, drain, strings, Emulator};

/// The AWS Config implementation type, as the catalog spells it.
const CONFIG_RULE: &str = "AWS::Config::ConfigRule";

/// The Security Hub implementation type, as the catalog spells it.
const SECURITY_HUB_CONTROL: &str = "AWS::SecurityHub::SecurityControl";

/// How the catalog classifies one control it publishes.
struct Classification {
    /// A designator the control is published under, which identifies it catalog-wide.
    alias: &'static str,
    /// The opaque identifier the control's published catalog ARN carries.
    identifier: &'static str,
    /// `ControlSeverity`.
    severity: &'static str,
    /// `ParameterRequirementSummary`.
    parameter_requirement_summary: &'static str,
    /// The resource types the control governs, empty where no available resource type
    /// represents what it governs.
    governed_resources: &'static [&'static str],
    /// `(ImplementationType, ImplementationIdentifier)`.
    implementation: (&'static str, &'static str),
    /// The display name the catalog publishes for the control, where this table records
    /// it; `None` leaves the name to the suite that owns a control's published text.
    name: Option<&'static str>,
    /// The instant the catalog publishes as the control's release time (`CreateTime`), in
    /// seconds since the Unix epoch, where this table records it.
    release_time: Option<i64>,
}

/// The published classification of each control this table covers.
///
/// Source: the Control Catalog's own published control list — each control's ARN
/// (`arn:{PARTITION}:controlcatalog:::control/{identifier}`), the designator it is
/// published under, the severity it is ranked at and the resource types it governs —
/// together with the input parameters of the implementing surface named in
/// `implementation`, which is what `ParameterRequirementSummary` summarises.
const PUBLISHED_CLASSIFICATIONS: &[Classification] = &[
    Classification {
        alias: "AWS-GR_EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK",
        identifier: "87qo8rsoettjrxjevmjqcw1tu",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::EC2::Volume"],
        implementation: (CONFIG_RULE, "EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK"),
        name: Some("Detect whether Amazon EBS snapshots are restorable by all AWS accounts"),
        release_time: None,
    },
    Classification {
        alias: "CONFIG.CLOUDTRAIL.DT.8",
        identifier: "9vnj728czsv4m6pflfqt21bw2",
        severity: "LOW",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (CONFIG_RULE, "CLOUD_TRAIL_CLOUD_WATCH_LOGS_ENABLED"),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "CONFIG.ELASTICLOADBALANCING.DT.3",
        identifier: "aazzef7lptl5umta6690p6u5c",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::ElasticLoadBalancingV2::LoadBalancer"],
        implementation: (CONFIG_RULE, "ELBV2_ACM_CERTIFICATE_REQUIRED"),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "CONFIG.IAM.DT.7",
        identifier: "byrg7iwyl4azhecyhqtq8ocw1",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::IAM::Policy"],
        implementation: (CONFIG_RULE, "IAM_POLICY_NO_STATEMENTS_WITH_ADMIN_ACCESS"),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "CONFIG.RDS.DT.39",
        identifier: "84hd48gyrb6e5ribkmsoub44c",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        // The catalog publishes no available resource type for what this control
        // governs, so the list is served empty.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "RDS_SNAPSHOT_ENCRYPTED"),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "AWS-GR_REDSHIFT_CLUSTER_PUBLIC_ACCESS_CHECK",
        identifier: "1oxkwnc4hwhi2ndv6ekwy7np7",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::Redshift::Cluster"],
        implementation: (CONFIG_RULE, "REDSHIFT_CLUSTER_PUBLIC_ACCESS_CHECK"),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "AWS-GR_RESTRICTED_COMMON_PORTS",
        identifier: "df2ta5ytg2zatj1q7y5e09u32",
        severity: "CRITICAL",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::EC2::SecurityGroup"],
        implementation: (CONFIG_RULE, "RESTRICTED_INCOMING_TRAFFIC"),
        name: Some("Disallow Internet connection via RDP"),
        release_time: None,
    },
    Classification {
        alias: "CONFIG.ACCOUNT.DT.2",
        identifier: "227ph3vim64tdwv5mfagjd22r",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        // The account's own root-user credentials, which no available resource type
        // represents.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "ROOT_ACCOUNT_HARDWARE_MFA_ENABLED"),
        name: None,
        release_time: Some(1_535_587_200),
    },
    Classification {
        alias: "AWS-GR_SAGEMAKER_NOTEBOOK_NO_DIRECT_INTERNET_ACCESS",
        identifier: "66gfl06uj1v999z53szvu0exa",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::SageMaker::NotebookInstance"],
        implementation: (
            CONFIG_RULE,
            "SAGEMAKER_NOTEBOOK_NO_DIRECT_INTERNET_ACCESS",
        ),
        name: None,
        release_time: None,
    },
    Classification {
        alias: "CONFIG.SECRETSMANAGER.DT.5",
        identifier: "4tf01viblttjxqhu35yz90vw1",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::SecretsManager::Secret"],
        implementation: (CONFIG_RULE, "SECRETSMANAGER_ROTATION_ENABLED_CHECK"),
        name: None,
        release_time: None,
    },
    // Controls the recorded pages this suite was written for never named, so the rule is
    // asserted beyond them: the lowest severity the catalog ranks a control at, a
    // Control Tower control whose governed-resource list the catalog publishes empty,
    // and a Security Hub-implemented control the implementing service parameterises.
    Classification {
        alias: "CONFIG.ACM-PCA.DT.2",
        identifier: "6yl9xxuky13vzaxi3n6lm73k5",
        severity: "LOW",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::ACMPCA::CertificateAuthority"],
        implementation: (CONFIG_RULE, "ACMPCA_CERTIFICATE_AUTHORITY_TAGGED"),
        name: None,
        release_time: Some(1_779_840_000),
    },
    Classification {
        alias: "AWS-GR_RDS_SNAPSHOTS_PUBLIC_PROHIBITED",
        identifier: "1h4eyqyyonp19dlrreqf1i3w0",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &[],
        implementation: (CONFIG_RULE, "RDS_SNAPSHOTS_PUBLIC_PROHIBITED"),
        name: Some("Disallow public access to RDS database snapshots"),
        release_time: Some(1_564_444_800),
    },
    Classification {
        alias: "SH.CONFIG.DT.1",
        identifier: "9r3or4f2x3aq1jrnma6ddfp2y",
        severity: "CRITICAL",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &[],
        implementation: (SECURITY_HUB_CONTROL, "Config.1"),
        name: None,
        release_time: Some(1_779_840_000),
    },
];

/// The values the modeled `ControlSeverity` admits.
const MODELED_SEVERITIES: [&str; 4] = ["LOW", "MEDIUM", "HIGH", "CRITICAL"];

/// The values the modeled `ParameterRequirementSummary` admits.
const MODELED_PARAMETER_SUMMARIES: [&str; 3] = ["REQUIRED", "OPTIONAL", "NONE"];

/// Page sizes the listings below are driven with: the service's own default, one control
/// per page, a partial page and the modeled maximum of `MaxListControlsResults`
/// (`@range 1..100`).
const PAGE_SIZES: [Option<u32>; 4] = [None, Some(1), Some(7), Some(100)];

/// The `CreateTime` of a served control, in whole seconds since the Unix epoch.
fn create_time(case: &str, control: &Value) -> i64 {
    let seconds = control["CreateTime"].as_f64().unwrap_or_else(|| {
        panic!(
            "case {case}: CreateTime must be a timestamp, got {:?}",
            control["CreateTime"]
        )
    });
    assert!(
        seconds > 0.0 && seconds.fract() == 0.0,
        "case {case}: CreateTime must be a whole-second instant after the epoch, got {seconds}"
    );
    seconds as i64
}

/// Every control the catalog serves for `body` at `page_size`, keyed by ARN.
async fn listing(
    emulator: &Emulator,
    page_size: Option<u32>,
    body: &str,
) -> BTreeMap<String, Value> {
    let controls = drain(emulator, "/list-controls", "Controls", page_size, body).await;
    assert!(
        !controls.is_empty(),
        "case maxResults={page_size:?} {body}: ListControls must publish the catalog's \
         controls"
    );
    controls
        .into_iter()
        .map(|control| {
            let arn = control["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {control:?}: a control carries its ARN"))
                .to_string();
            (arn, control)
        })
        .collect()
}

/// The classification of a served control, as the four members this suite is about.
fn classification(control: &Value) -> (Option<&str>, Option<&str>, Vec<String>, Vec<String>) {
    (
        control["Severity"].as_str(),
        control["ParameterRequirementSummary"].as_str(),
        strings(control, "GovernedResources"),
        strings(control, "Aliases"),
    )
}

/// Every control in the table is served under the catalog's own ARN with the catalog's
/// own designator, severity, governed resources and parameter summary, and reads back the
/// same way through `GetControl`.
#[tokio::test]
async fn every_published_control_is_served_with_the_catalogs_classification() {
    let emulator = Emulator::new();
    let indexed = listing(&emulator, None, "{}").await;

    let mut severities_seen: BTreeSet<&str> = BTreeSet::new();
    let mut summaries_seen: BTreeSet<&str> = BTreeSet::new();
    let mut empty_governed = 0usize;
    let mut populated_governed = 0usize;
    let mut names_recorded = 0usize;
    let mut release_times_recorded = 0usize;

    for row in PUBLISHED_CLASSIFICATIONS {
        let case = row.alias;
        let arn = catalog_arn(row.identifier);
        let summary = indexed.get(&arn).unwrap_or_else(|| {
            panic!("case {case}: the catalog must publish this control under {arn}")
        });

        assert!(
            strings(summary, "Aliases").contains(&row.alias.to_string()),
            "case {case}: expected the designator the control is published under, got {:?}",
            summary["Aliases"]
        );
        assert_eq!(
            summary["Severity"].as_str(),
            Some(row.severity),
            "case {case}: expected the catalog's severity {}, got {:?}",
            row.severity,
            summary["Severity"]
        );
        assert_eq!(
            summary["ParameterRequirementSummary"].as_str(),
            Some(row.parameter_requirement_summary),
            "case {case}: expected ParameterRequirementSummary {}, got {:?}",
            row.parameter_requirement_summary,
            summary["ParameterRequirementSummary"]
        );
        assert_eq!(
            strings(summary, "GovernedResources"),
            row.governed_resources,
            "case {case}: expected the governed resource types the catalog publishes, \
             got {:?}",
            summary["GovernedResources"]
        );
        if let Some(name) = row.name {
            assert_eq!(
                summary["Name"].as_str(),
                Some(name),
                "case {case}: expected the catalog's published name {name:?}, got {:?}",
                summary["Name"]
            );
        }
        if let Some(release_time) = row.release_time {
            assert_eq!(
                create_time(case, summary),
                release_time,
                "case {case}: expected the control's published release instant \
                 {release_time}, got {:?}",
                summary["CreateTime"]
            );
        }
        let (implementation_type, implementation_identifier) = row.implementation;
        assert_eq!(
            (
                summary["Implementation"]["Type"].as_str(),
                summary["Implementation"]["Identifier"].as_str(),
            ),
            (Some(implementation_type), Some(implementation_identifier)),
            "case {case}: expected the implementing surface \
             {implementation_type}/{implementation_identifier}, got {:?}",
            summary["Implementation"]
        );

        // The summary is a projection of the record GetControl answers with, so the
        // control reads back with the same classification under the same ARN.
        let read_back = emulator
            .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
            .await;
        assert_eq!(
            read_back["Arn"].as_str(),
            Some(arn.as_str()),
            "case {case}: GetControl must resolve the catalog's ARN, got {read_back:?}"
        );
        assert_eq!(
            classification(&read_back),
            classification(summary),
            "case {case}: GetControl and ListControls must report one classification, \
             got {read_back:?}"
        );
        assert_eq!(
            create_time(case, &read_back),
            create_time(case, summary),
            "case {case}: GetControl and ListControls must report one release instant, \
             got {read_back:?}"
        );

        severities_seen.insert(row.severity);
        summaries_seen.insert(row.parameter_requirement_summary);
        names_recorded += usize::from(row.name.is_some());
        release_times_recorded += usize::from(row.release_time.is_some());
        if row.governed_resources.is_empty() {
            empty_governed += 1;
        } else {
            populated_governed += 1;
        }
    }

    // None of the assertions above is vacuous: the table spans every severity the model
    // admits, more than one parameter summary, and both governed-resource shapes.
    for severity in MODELED_SEVERITIES {
        assert!(
            severities_seen.contains(severity),
            "case {severity}: the table must span this severity for the rule to be \
             tested, saw {severities_seen:?}"
        );
    }
    for summary in ["NONE", "OPTIONAL"] {
        assert!(
            summaries_seen.contains(summary),
            "case {summary}: the table must span this parameter summary for the rule to \
             be tested, saw {summaries_seen:?}"
        );
    }
    assert!(
        empty_governed >= 1 && populated_governed >= 1,
        "case catalog: the table must span an empty and a populated governed-resource \
         list, saw {empty_governed} empty and {populated_governed} populated"
    );
    assert!(
        names_recorded >= 2 && release_times_recorded >= 2,
        "case catalog: the table must record a published name and a published release \
         instant for more than one control for those rules to be tested, saw \
         {names_recorded} name(s) and {release_times_recorded} instant(s)"
    );
}

/// A control's classification is a property of the control, not of the request: it is the
/// same whatever page size walked the listing and whatever filter selected the control,
/// for EVERY control the catalog serves.
#[tokio::test]
async fn a_controls_classification_does_not_depend_on_how_it_was_selected() {
    let emulator = Emulator::new();
    let first = listing(&emulator, None, "{}").await;

    // Requests that select the catalog differently: the whole catalog at each page size,
    // each implementation type the served controls report, and each implementation
    // identifier the table above names.
    let mut requests: Vec<(Option<u32>, String)> = PAGE_SIZES
        .iter()
        .map(|page_size| (*page_size, "{}".to_string()))
        .collect();
    let mut types: BTreeSet<String> = BTreeSet::new();
    for control in first.values() {
        if let Some(implementation_type) = control["Implementation"]["Type"].as_str() {
            types.insert(implementation_type.to_string());
        }
    }
    assert!(
        types.len() >= 2,
        "case catalog: the catalog must publish more than one implementation type for \
         the filter cases to narrow anything, got {types:?}"
    );
    for implementation_type in &types {
        requests.push((
            None,
            format!(r#"{{"Filter":{{"Implementations":{{"Types":["{implementation_type}"]}}}}}}"#),
        ));
    }
    for row in PUBLISHED_CLASSIFICATIONS {
        requests.push((
            Some(1),
            format!(
                r#"{{"Filter":{{"Implementations":{{"Identifiers":["{}"]}}}}}}"#,
                row.implementation.1
            ),
        ));
    }

    for (page_size, body) in &requests {
        let again = listing(&emulator, *page_size, body).await;
        for (arn, control) in &again {
            let case = format!("{arn} maxResults={page_size:?} {body}");
            let expected = first
                .get(arn)
                .unwrap_or_else(|| panic!("case {case}: the unfiltered listing must hold {arn}"));
            assert_eq!(
                classification(control),
                classification(expected),
                "case {case}: a control's classification must not depend on the request, \
                 got {control:?}"
            );
        }
    }
}

/// Every control the catalog serves — not only the rows above — is classified with
/// modeled values, under one identifier of its own: a severity and a parameter summary
/// the model admits, governed resource types in the published `Service::Resource` form,
/// and no identifier shared with another control.
#[tokio::test]
async fn every_served_control_carries_a_modeled_classification() {
    let emulator = Emulator::new();
    let indexed = listing(&emulator, None, "{}").await;

    let severities: BTreeSet<&str> = MODELED_SEVERITIES.iter().copied().collect();
    let summaries: BTreeSet<&str> = MODELED_PARAMETER_SUMMARIES.iter().copied().collect();
    let mut identifiers: BTreeSet<String> = BTreeSet::new();

    for (arn, control) in &indexed {
        let severity = control["Severity"]
            .as_str()
            .unwrap_or_else(|| panic!("case {arn}: a control states its severity"));
        assert!(
            severities.contains(severity),
            "case {arn}: expected a modeled severity ({severities:?}), got {severity}"
        );
        let summary = control["ParameterRequirementSummary"]
            .as_str()
            .unwrap_or_else(|| panic!("case {arn}: a control states its parameter summary"));
        assert!(
            summaries.contains(summary),
            "case {arn}: expected a modeled parameter summary ({summaries:?}), got {summary}"
        );
        // A governed resource type is named in the form the provider that offers it
        // publishes resource types in, so the form is checked against the provider the
        // control reports governing rather than assumed to be the Amazon Web Services
        // one.
        let governs_aws = strings(control, "GovernedProviders").contains(&"AWS".to_string());
        for resource in strings(control, "GovernedResources") {
            assert!(
                !resource.is_empty() && !resource.chars().any(char::is_whitespace),
                "case {arn}: a governed resource type is a published type name, got \
                 {resource:?}"
            );
            if governs_aws {
                let segments: Vec<&str> = resource.split("::").collect();
                assert!(
                    segments.len() >= 3
                        && segments.iter().all(|segment| {
                            !segment.is_empty()
                                && segment
                                    .chars()
                                    .all(|character| character.is_ascii_alphanumeric())
                        }),
                    "case {arn}: an Amazon Web Services resource type is published as \
                     Provider::Service::Resource, got {resource}"
                );
            }
        }
        let identifier = arn
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(
            identifiers.insert(identifier.clone()),
            "case {arn}: two controls must not share the catalog identifier {identifier}"
        );
    }
}
