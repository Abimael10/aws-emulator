// SPDX-License-Identifier: CC0-1.0

//! The published record a Security Hub-implemented catalog control is served with.
//!
//! The rule under test: the Control Catalog is an Amazon Web Services-published,
//! account- and Region-independent read-only dataset (its ARNs carry neither account nor
//! Region), so every member of a control's `ControlSummary` is the catalog's OWN
//! published content for that control — the opaque identifier its ARN carries, the
//! designators it is published under ("human-readable designators, such as `SH.S3.1`"),
//! its name, its description ("as it may appear in the console"), its severity, its
//! parameter-requirement summary, the instant it "was released (start of its life) as a
//! governance capability", and the resource types it governs — never a value the emulator
//! mints, derives from the control's implementation identifier or re-words
//! (`smithy_query(query="shape", shape="ControlSummary",
//! model_path="services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json")`).
//!
//! Each test is a TABLE rather than one control picked by hand: the rows are the Security
//! Hub controls whose WHOLE published record — identity, text and classification — this
//! catalog records, and they differ in
//! severity (all four modeled values), in designator form (`SH.` followed by the
//! control's own Security Hub id, and the catalog's own `SH.{SERVICE}.DT.{N}`), in
//! parameter-requirement summary, in how many resource types the control governs (none,
//! because no available resource type represents what it governs, exactly one, and
//! several), and in release instant (controls released together against controls released
//! on their own). Every row is crossed with the page sizes a client walks the listing at,
//! with the implementation-type filter a client narrows it by, and with `GetControl`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::control_catalog::{catalog_arn, drain, strings, Emulator};

/// One Security Hub control's published catalog record.
///
/// Source: the Control Catalog's own published control list — the ARN
/// `arn:{PARTITION}:controlcatalog:::control/{identifier}` it names the control by, the
/// designator it publishes the control under, the display name and console description it
/// publishes for it, its severity, its parameter-requirement summary, the instant it
/// records as the control's release time (`CreateTime`, in seconds since the Unix epoch)
/// and the resource types it reports as governed. `implementation_identifier` is the
/// Security Hub control id the catalog reports as the implementing surface's own
/// identifier.
struct PublishedSecurityHubControl {
    designator: &'static str,
    identifier: &'static str,
    implementation_identifier: &'static str,
    name: &'static str,
    description: &'static str,
    severity: &'static str,
    parameter_requirement_summary: &'static str,
    release_time: i64,
    governed_resources: &'static [&'static str],
}

/// The Security Hub implementation type, as the catalog spells it.
const SECURITY_HUB_CONTROL: &str = "AWS::SecurityHub::SecurityControl";

/// The designator prefix a Security Hub control is published under, as the `Aliases`
/// documentation's `SH.S3.1` example spells it.
const SECURITY_HUB_PREFIX: &str = "SH.";

/// Every Security Hub control whose WHOLE published record this catalog records: its
/// published identity (identifier and release instant) together with the published text and
/// classification served with it. A control the catalog records no published identifier for
/// is served under a minted one and is left out of this table; its identity is covered by
/// `all/published_catalog_identity.rs`.
const PUBLISHED_SECURITY_HUB_CONTROLS: &[PublishedSecurityHubControl] = &[
    PublishedSecurityHubControl {
        designator: "SH.CloudTrail.4",
        identifier: "624yg0j9d8swiglwfc1m4kvnm",
        implementation_identifier: "CloudTrail.4",
        name: "CloudTrail log file validation should be enabled",
        description: "This AWS control checks whether CloudTrail log file validation is \
                      enabled.",
        severity: "LOW",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::CloudTrail::Trail"],
    },
    PublishedSecurityHubControl {
        designator: "SH.EC2.2",
        identifier: "dowsaumj3n874siszx3j94m9n",
        implementation_identifier: "EC2.2",
        name: "VPC default security groups should not allow inbound or outbound traffic",
        description: "This AWS control checks that the default security group of a VPC \
                      does not allow inbound or outbound traffic.",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::EC2::SecurityGroup"],
    },
    PublishedSecurityHubControl {
        designator: "SH.EFS.1",
        identifier: "5cksj6jdycadjupdbviikn1w7",
        implementation_identifier: "EFS.1",
        name: "Elastic File System should be configured to encrypt file data at-rest \
               using AWS KMS",
        description: "This AWS control checks whether Amazon Elastic File System (Amazon \
                      EFS) is configured to encrypt the file data using AWS Key \
                      Management Service (AWS KMS). The check will fail if the encrypted \
                      key is set to false on DescribeFileSystems or if the KmsKeyId key \
                      on DescribeFileSystems does not match the KmsKeyId parameter.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::EFS::FileSystem"],
    },
    PublishedSecurityHubControl {
        designator: "SH.GuardDuty.1",
        identifier: "62smpoz33dsy0oa7u1iwa58lz",
        implementation_identifier: "GuardDuty.1",
        name: "GuardDuty should be enabled",
        description: "This AWS control checks whether Amazon GuardDuty is enabled in \
                      your AWS account and region.",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::GuardDuty::Detector"],
    },
    PublishedSecurityHubControl {
        designator: "SH.IAM.1",
        identifier: "a7ertnvgba7449o25l6s75nrj",
        implementation_identifier: "IAM.1",
        name: "IAM policies should not allow full \"*\" administrative privileges",
        description: "This AWS control checks whether the default version of AWS \
                      Identity and Access Management (IAM) policies (also known as \
                      customer managed policies) do not have administrator access with a \
                      statement that has \"Effect\": \"Allow\" with \"Action\": \"*\" \
                      over \"Resource\": \"*\". It only checks for the Customer Managed \
                      Policies that you created, but not inline and AWS Managed \
                      Policies.",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::IAM::Policy"],
    },
    PublishedSecurityHubControl {
        designator: "SH.IAM.3",
        identifier: "bxh5ltpzbcr59zexx3llhg3l9",
        implementation_identifier: "IAM.3",
        name: "IAM users' access keys should be rotated every 90 days or less",
        description: "This AWS control checks whether the active access keys are rotated \
                      within 90 days.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::IAM::User"],
    },
    // The control over the account's own root user credentials, which no available
    // resource type represents — the second row whose governed-resource list is empty.
    PublishedSecurityHubControl {
        designator: "SH.IAM.4",
        identifier: "ezfflnh8xgshxb78grtt5sjc5",
        implementation_identifier: "IAM.4",
        name: "IAM root user access key should not exist",
        description: "This AWS control checks whether the root user access key is \
                      available.",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &[],
    },
    PublishedSecurityHubControl {
        designator: "SH.IAM.5",
        identifier: "6g3gbim493yofkc9eg6vvnahm",
        implementation_identifier: "IAM.5",
        name: "MFA should be enabled for all IAM users that have a console password",
        description: "This AWS control checks whether AWS Multi-Factor Authentication \
                      (MFA) is enabled for all AWS Identity and Access Management (IAM) \
                      users that use a console password.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::IAM::User"],
    },
    PublishedSecurityHubControl {
        designator: "SH.RDS.2",
        identifier: "bbns1svmi60sz11i658ot4amf",
        implementation_identifier: "RDS.2",
        name: "RDS DB Instances should prohibit public access, as determined by the \
               PubliclyAccessible configuration",
        description: "This AWS control checks whether RDS instances are publicly \
                      accessible by evaluating the publiclyAccessible field in the \
                      instance configuration item.",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::RDS::DBInstance"],
    },
    PublishedSecurityHubControl {
        designator: "SH.RDS.3",
        identifier: "dsrtfqf4g9gvlguj5hqahozla",
        implementation_identifier: "RDS.3",
        name: "RDS DB instances should have encryption at-rest enabled",
        description: "This AWS control checks whether storage encryption is enabled for \
                      your RDS DB instances.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::RDS::DBInstance"],
    },
    // The control whose check is on the ACCOUNT-level block public access settings, so
    // the catalog publishes every S3 resource type those settings restrict access to as
    // governed — the row that spans a governed-resource list of several types.
    PublishedSecurityHubControl {
        designator: "SH.S3.1",
        identifier: "alm61aw5oxpgfgh3bhmish87u",
        implementation_identifier: "S3.1",
        name: "S3 general purpose buckets should have block public access settings enabled",
        description: "This control checks whether the preceding Amazon S3 block public \
                      access settings are configured at the account level for an S3 \
                      general purpose bucket. The control fails if one or more of the \
                      block public access settings are set to false.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &[
            "AWS::S3::AccessPoint",
            "AWS::S3::Bucket",
            "AWS::S3::BucketPolicy",
            "AWS::S3::MultiRegionAccessPoint",
            "AWS::S3::MultiRegionAccessPointPolicy",
        ],
    },
    PublishedSecurityHubControl {
        designator: "SH.S3.5",
        identifier: "35hoe56okxxh2ujnsub92wkym",
        implementation_identifier: "S3.5",
        name: "S3 general purpose buckets should require requests to use SSL",
        description: "This control checks whether an Amazon S3 general purpose bucket has \
                      a policy that requires requests to use SSL. The control fails if \
                      the bucket policy doesn't require requests to use SSL.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_669_593_600,
        governed_resources: &["AWS::S3::Bucket"],
    },
    // Released after the body of controls above, so it carries its own release instant.
    PublishedSecurityHubControl {
        designator: "SH.KMS.4",
        identifier: "esv514s4zvnxdunuijbgerpn3",
        implementation_identifier: "KMS.4",
        name: "AWS KMS key rotation should be enabled",
        description: "This AWS control checks that key rotation is enabled for each AWS \
                      KMS key. It does not check KMS keys that have imported key \
                      material.",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        release_time: 1_701_043_200,
        governed_resources: &["AWS::KMS::Key"],
    },
    // The catalog publishes this one under a designator of its OWN rather than under the
    // control's Security Hub id, and Security Hub parameterises it, so it accepts
    // optional parameters.
    PublishedSecurityHubControl {
        designator: "SH.CONFIG.DT.1",
        identifier: "9r3or4f2x3aq1jrnma6ddfp2y",
        implementation_identifier: "Config.1",
        name: "AWS Config should be enabled and use the service-linked role for resource \
               recording",
        description: "This control checks whether AWS Config is enabled in your account \
                      in the current AWS Region, records all resources that correspond \
                      to controls that are enabled in the current Region, and uses the \
                      service-linked AWS Config role.",
        severity: "CRITICAL",
        parameter_requirement_summary: "OPTIONAL",
        release_time: 1_779_840_000,
        // The account's own configuration recorder, which no available resource type
        // represents, so the catalog publishes the list empty: "If GovernedResources
        // cannot be represented by available resource types, it's returned as an empty
        // list".
        governed_resources: &[],
    },
];

/// Page sizes the listing is walked at: the service's own default, one control per page,
/// a partial page, and the modeled maximum of `MaxListControlsResults` (`@range 1..100`).
const PAGE_SIZES: [Option<u32>; 4] = [None, Some(1), Some(7), Some(100)];

/// The request bodies a client reads the Security Hub page of the catalog with: the whole
/// catalog, and the catalog narrowed to the Security Hub implementation type.
fn requests() -> Vec<String> {
    vec![
        "{}".to_string(),
        format!(r#"{{"Filter":{{"Implementations":{{"Types":["{SECURITY_HUB_CONTROL}"]}}}}}}"#),
    ]
}

/// The `Aliases` of a served control.
fn aliases(control: &Value) -> Vec<String> {
    strings(control, "Aliases")
}

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
        "case {case}: CreateTime must be a whole-second instant after the epoch, got \
         {seconds}"
    );
    seconds as i64
}

/// Every control a request serves, keyed by its ARN.
async fn listed_by_arn(
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

/// Assert that `served` carries every member of `row`'s published record.
fn assert_published_record(case: &str, row: &PublishedSecurityHubControl, served: &Value) {
    let expected_arn = catalog_arn(row.identifier);
    assert_eq!(
        served["Arn"].as_str(),
        Some(expected_arn.as_str()),
        "case {case}: expected the catalog's published ARN {expected_arn}, got {:?}",
        served["Arn"]
    );
    assert_eq!(
        aliases(served),
        vec![row.designator.to_string()],
        "case {case}: expected the designator the catalog publishes this control under, \
         got {:?}",
        served["Aliases"]
    );
    assert_eq!(
        served["Name"].as_str(),
        Some(row.name),
        "case {case}: expected the catalog's published Name {:?}, got {:?}",
        row.name,
        served["Name"]
    );
    assert_eq!(
        served["Description"].as_str(),
        Some(row.description),
        "case {case}: expected the catalog's published Description {:?}, got {:?}",
        row.description,
        served["Description"]
    );
    assert_eq!(
        served["Severity"].as_str(),
        Some(row.severity),
        "case {case}: expected the published Severity {}, got {:?}",
        row.severity,
        served["Severity"]
    );
    assert_eq!(
        served["ParameterRequirementSummary"].as_str(),
        Some(row.parameter_requirement_summary),
        "case {case}: expected ParameterRequirementSummary {}, got {:?}",
        row.parameter_requirement_summary,
        served["ParameterRequirementSummary"]
    );
    assert_eq!(
        create_time(case, served),
        row.release_time,
        "case {case}: expected the published release instant {}, got {:?}",
        row.release_time,
        served["CreateTime"]
    );
    assert_eq!(
        strings(served, "GovernedResources"),
        row.governed_resources,
        "case {case}: expected the governed resource types the catalog publishes, got \
         {:?}",
        served["GovernedResources"]
    );
    assert_eq!(
        (
            served["Implementation"]["Type"].as_str(),
            served["Implementation"]["Identifier"].as_str(),
        ),
        (
            Some(SECURITY_HUB_CONTROL),
            Some(row.implementation_identifier)
        ),
        "case {case}: expected the implementing surface \
         {SECURITY_HUB_CONTROL}/{}, got {:?}",
        row.implementation_identifier,
        served["Implementation"]
    );
}

/// Every recorded Security Hub control is served with the catalog's own published
/// record — at every page size, through the unfiltered listing and the type-filtered one,
/// and through `GetControl` by the ARN a client harvested.
#[tokio::test]
async fn every_recorded_security_hub_control_is_served_with_its_published_record() {
    let emulator = Emulator::new();

    for body in requests() {
        for page_size in PAGE_SIZES {
            let listed = listed_by_arn(&emulator, page_size, &body).await;
            for row in PUBLISHED_SECURITY_HUB_CONTROLS {
                let arn = catalog_arn(row.identifier);
                let case = format!("{} maxResults={page_size:?} {body}", row.designator);
                let served = listed
                    .get(&arn)
                    .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));
                assert_published_record(&case, row, served);
            }
        }
    }

    for row in PUBLISHED_SECURITY_HUB_CONTROLS {
        let arn = catalog_arn(row.identifier);
        let case = format!("{} read as {arn}", row.designator);
        let read_back = emulator
            .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
            .await;
        assert_published_record(&case, row, &read_back);
    }

    // None of the rows above is asserted over a narrow class: the table spans every
    // modeled severity, both parameter summaries a Security Hub control is published
    // with, controls released together and controls released on their own, and an empty
    // as well as a non-empty governed-resource list.
    let severities: BTreeSet<&str> = PUBLISHED_SECURITY_HUB_CONTROLS
        .iter()
        .map(|row| row.severity)
        .collect();
    for severity in ["LOW", "MEDIUM", "HIGH", "CRITICAL"] {
        assert!(
            severities.contains(severity),
            "case severity {severity}: the table must span this severity for the rule to \
             be tested, saw {severities:?}"
        );
    }
    let summaries: BTreeSet<&str> = PUBLISHED_SECURITY_HUB_CONTROLS
        .iter()
        .map(|row| row.parameter_requirement_summary)
        .collect();
    for summary in ["NONE", "OPTIONAL"] {
        assert!(
            summaries.contains(summary),
            "case summary {summary}: the table must span this parameter summary for the \
             rule to be tested, saw {summaries:?}"
        );
    }
    let instants: BTreeSet<i64> = PUBLISHED_SECURITY_HUB_CONTROLS
        .iter()
        .map(|row| row.release_time)
        .collect();
    assert!(
        instants.len() >= 3,
        "case release instants: the table must span controls released together and \
         controls released on their own, saw {instants:?}"
    );
    assert!(
        PUBLISHED_SECURITY_HUB_CONTROLS
            .iter()
            .any(|row| row.governed_resources.is_empty())
            && PUBLISHED_SECURITY_HUB_CONTROLS
                .iter()
                .any(|row| !row.governed_resources.is_empty()),
        "case governed resources: the table must span both an empty and a non-empty list"
    );
}

/// `GovernedResources` reports the resource types a control governs — EVERY one of them.
///
/// A control's check can bear on more than one resource type, and the catalog publishes
/// the whole set: the list is never reduced to the one type that names the implementing
/// service, never reordered out of the catalog's own order and never carries a type
/// twice. An empty list is reserved for a control whose governed resources "cannot be
/// represented by available resource types". The set is catalog content rather than a
/// per-request projection, so the same list comes back at every page size, through the
/// type-filtered listing and through `GetControl`.
#[tokio::test]
async fn a_control_governing_several_resource_types_publishes_every_one_of_them() {
    let emulator = Emulator::new();

    // One entry per (control, way the client read it): the case name, the control's
    // published row, and the governed-resource list that read served.
    let mut served_lists: Vec<(String, &PublishedSecurityHubControl, Vec<String>)> = Vec::new();
    for body in requests() {
        for page_size in PAGE_SIZES {
            let listed = listed_by_arn(&emulator, page_size, &body).await;
            for row in PUBLISHED_SECURITY_HUB_CONTROLS {
                let arn = catalog_arn(row.identifier);
                let case = format!("{} maxResults={page_size:?} {body}", row.designator);
                let served = listed
                    .get(&arn)
                    .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));
                served_lists.push((case, row, strings(served, "GovernedResources")));
            }
        }
    }
    for row in PUBLISHED_SECURITY_HUB_CONTROLS {
        let arn = catalog_arn(row.identifier);
        let read_back = emulator
            .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
            .await;
        served_lists.push((
            format!("{} read as {arn}", row.designator),
            row,
            strings(&read_back, "GovernedResources"),
        ));
    }

    for (case, row, served_types) in &served_lists {
        let expected = row.governed_resources;
        // Not one published type is dropped...
        for resource_type in expected {
            assert!(
                served_types.iter().any(|served| served == resource_type),
                "case {case}: expected the governed resource type {resource_type} among \
                 the published list, got {served_types:?}"
            );
        }
        // ...nothing beyond them is reported, and the catalog's own order is kept...
        assert_eq!(
            served_types, expected,
            "case {case}: expected the published governed resource types, got \
             {served_types:?}"
        );
        // ...and no type is reported twice.
        let distinct: BTreeSet<&String> = served_types.iter().collect();
        assert_eq!(
            distinct.len(),
            served_types.len(),
            "case {case}: expected each governed resource type once, got {served_types:?}"
        );
    }

    // The rows span the multiplicities the rule generalises over, so no case above is
    // read as evidence for a narrower rule than the one under test.
    let multiplicities: BTreeSet<usize> = PUBLISHED_SECURITY_HUB_CONTROLS
        .iter()
        .map(|row| row.governed_resources.len())
        .collect();
    for expected in [0usize, 1] {
        assert!(
            multiplicities.contains(&expected),
            "case governed-resource count {expected}: the table must span it for the rule \
             to be tested, saw {multiplicities:?}"
        );
    }
    assert!(
        multiplicities.iter().any(|count| *count > 1),
        "case governed-resource count >1: the table must span a control governing several \
         resource types for the rule to be tested, saw {multiplicities:?}"
    );
}

/// The designator a control is published under is the catalog's own content: it is not
/// computed from the control's Security Hub id, so a control the catalog designates
/// otherwise is served — and read back — under that published designator, and each
/// designator names exactly one control.
#[tokio::test]
async fn the_published_designator_identifies_the_control_it_is_published_for() {
    let emulator = Emulator::new();
    let listed = listed_by_arn(&emulator, None, "{}").await;

    let mut by_designator: BTreeMap<String, String> = BTreeMap::new();
    for (arn, control) in &listed {
        for alias in aliases(control) {
            if let Some(first) = by_designator.insert(alias.clone(), arn.clone()) {
                panic!(
                    "case designator {alias}: expected one control to carry it, got both \
                     {first} and {arn}"
                );
            }
        }
    }

    let mut designators_matching_the_id = 0usize;
    let mut designators_of_the_catalogs_own_form = 0usize;
    for row in PUBLISHED_SECURITY_HUB_CONTROLS {
        let case = row.designator;
        let expected_arn = catalog_arn(row.identifier);
        assert_eq!(
            by_designator.get(row.designator).map(String::as_str),
            Some(expected_arn.as_str()),
            "case {case}: expected this designator on the control the catalog publishes \
             it for, got {:?}",
            by_designator.get(row.designator)
        );

        let derived_from_the_id = format!("{SECURITY_HUB_PREFIX}{}", row.implementation_identifier);
        if row.designator == derived_from_the_id {
            designators_matching_the_id += 1;
        } else {
            designators_of_the_catalogs_own_form += 1;
            // The designator a client would compute from the control's Security Hub id
            // names nothing in the catalog: the published designator is the only one.
            assert!(
                !by_designator.contains_key(&derived_from_the_id),
                "case {case}: no control may be published under the designator \
                 {derived_from_the_id} computed from its Security Hub id, got {:?}",
                by_designator.get(&derived_from_the_id)
            );
        }

        // The designator comes back from GetControl too, so a client that read the
        // control by its ARN joins it to the same Security Hub control.
        let read_back = emulator
            .ok("/get-control", &format!(r#"{{"ControlArn":"{expected_arn}"}}"#))
            .await;
        assert_eq!(
            aliases(&read_back),
            vec![row.designator.to_string()],
            "case {case}: GetControl must report the published designator, got {:?}",
            read_back["Aliases"]
        );
    }

    assert!(
        designators_matching_the_id >= 1 && designators_of_the_catalogs_own_form >= 1,
        "case designator forms: the table must span both published forms for the rule to \
         be tested, saw {designators_matching_the_id} of the id form and \
         {designators_of_the_catalogs_own_form} of the catalog's own form"
    );
}

/// A control's `Description` is the catalog's own published sentence for that control,
/// served byte for byte.
///
/// `Description` is "[a] description of the control, as it may appear in the console"
/// (`smithy_query(query="shape", shape="ControlSummary")`) — catalog CONTENT owned by
/// Amazon Web Services, so what a client reads is the published text character for
/// character: never re-worded, re-ordered, abridged, or extended with the implementing
/// surface's own vocabulary (a backing rule identifier, an extra failure condition, a
/// service-name expansion or contraction, a re-cased configuration-item field). Because
/// the catalog is account- and Region-independent and read-only, the same text comes back
/// through `ListControls` and `GetControl`, at every page size, through the type-filtered
/// listing, and in a second session over its own engine.
///
/// The rows are the whole table above rather than one control: they span the text forms
/// the rule generalises over — a single sentence, several sentences, a sentence carrying
/// quoted JSON fragments, and one carrying a parenthesised expansion of a service name.
#[tokio::test]
async fn a_controls_published_description_is_served_byte_for_byte() {
    let emulator = Emulator::new();

    for body in requests() {
        for page_size in PAGE_SIZES {
            let listed = listed_by_arn(&emulator, page_size, &body).await;
            for row in PUBLISHED_SECURITY_HUB_CONTROLS {
                let arn = catalog_arn(row.identifier);
                let case = format!("{} maxResults={page_size:?} {body}", row.designator);
                let served = listed
                    .get(&arn)
                    .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));
                assert_eq!(
                    served["Description"].as_str(),
                    Some(row.description),
                    "case {case}: expected the catalog's published Description {:?}, got \
                     {:?}",
                    row.description,
                    served["Description"]
                );
            }
        }
    }

    // The rule holds for the WHOLE catalogue, not only the rows above: for every control
    // the catalog publishes, the text `ListControls` serves is the text `GetControl`
    // serves, and a second session over its own engine publishes it again unchanged — so
    // no part of the text is derived from the request, the session or the run.
    let second_session = Emulator::new();
    let listed = listed_by_arn(&emulator, None, "{}").await;
    let in_the_second_session = listed_by_arn(&second_session, Some(1), "{}").await;
    for (arn, control) in &listed {
        let published = control["Description"].as_str().unwrap_or_else(|| {
            panic!(
                "case {arn}: a control carries its required Description, got {:?}",
                control["Description"]
            )
        });
        assert!(
            !published.trim().is_empty(),
            "case {arn}: expected the catalog's published description, got {published:?}"
        );
        let read_back = emulator
            .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
            .await;
        assert_eq!(
            read_back["Description"].as_str(),
            Some(published),
            "case {arn}: GetControl must report the published description {published:?}, \
             got {:?}",
            read_back["Description"]
        );
        let elsewhere = in_the_second_session.get(arn).unwrap_or_else(|| {
            panic!("case {arn}: every session must publish this control")
        });
        assert_eq!(
            elsewhere["Description"].as_str(),
            Some(published),
            "case {arn}: a second session must publish the same description \
             {published:?}, got {:?}",
            elsewhere["Description"]
        );
    }

    // The table spans the published text forms the rule generalises over, so no case
    // above is read as evidence for a narrower rule than the one under test.
    let sentences = |description: &str| description.matches(". ").count() + 1;
    assert!(
        PUBLISHED_SECURITY_HUB_CONTROLS
            .iter()
            .any(|row| sentences(row.description) == 1),
        "case text form: the table must span a one-sentence published description"
    );
    assert!(
        PUBLISHED_SECURITY_HUB_CONTROLS
            .iter()
            .any(|row| sentences(row.description) > 1),
        "case text form: the table must span a multi-sentence published description"
    );
    assert!(
        PUBLISHED_SECURITY_HUB_CONTROLS
            .iter()
            .any(|row| row.description.contains('"')),
        "case text form: the table must span a published description carrying quoted \
         JSON fragments"
    );
    assert!(
        PUBLISHED_SECURITY_HUB_CONTROLS
            .iter()
            .any(|row| row.description.contains('(')),
        "case text form: the table must span a published description carrying a \
         parenthesised service-name expansion"
    );
}
