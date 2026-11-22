// SPDX-License-Identifier: CC0-1.0

//! The identity and metadata a published control is served with.
//!
//! The rules under test, read off the modeled `ControlSummary` / `GetControlResponse`
//! (`smithy_query(query="operation", operation="ListControls,GetControl")`) and the
//! documentation of their members:
//!
//! 1. The catalog is an Amazon Web Services-published, account-independent read-only
//!    dataset — its ARNs carry neither account nor Region — so every control is served
//!    with the catalog's OWN published identity and metadata: the opaque identifier its
//!    published ARN carries, the designators it is published under, its name, its
//!    severity and the resource types it governs ("If GovernedResources cannot be
//!    represented by available resource types, it's returned as an empty list").
//! 2. `Implementation` does not identify a control: the catalog publishes a SEPARATE
//!    control for every surface and scope it governs, so two controls may report one
//!    implementation type and identifier — an account-wide control and a log-archive
//!    one over the same AWS Config managed rule — each under its own ARN and its own
//!    designator, and neither may be dropped or merged into the other.
//! 3. `ParameterRequirementSummary` summarises the parameters a caller supplies when it
//!    enables the control, which are the parameters of the surface implementing it: a
//!    control over an AWS Config managed rule that takes input parameters accepts them
//!    (`OPTIONAL`), one over a rule that takes none supports none (`NONE`), and a
//!    control deployed as a policy or a hook summarises its exemption parameters.
//! 4. `CreateTime` is the instant the catalog publishes as the control's release time, so
//!    a control the catalog records one for is served with THAT instant. The whole of a
//!    control's published text and dating is `all/published_control_release_record.rs`;
//!    here it is asserted for the identity this suite records.
//!
//! Each test is a TABLE spanning the class rather than one control picked by hand: the
//! rows differ in designator family (`CONFIG.…`, `AWS-GR_…`, `CT.…`, `SH.…`),
//! implementation surface, severity, parameter summary, governed-resource shape and
//! whether the implementation is shared with another control.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::control_catalog::{catalog_arn, drain, strings, Emulator};

/// One published control, as the catalog publishes it.
///
/// Source: the Control Catalog's own published control list — each control's ARN
/// (`arn:{PARTITION}:controlcatalog:::control/{identifier}`), the designators it is
/// published under, its name, its severity and its governed resource types — together
/// with the input parameters of the AWS Config managed rule named in `implementation`,
/// which is what its `ParameterRequirementSummary` summarises.
struct PublishedControl {
    /// A designator the control is published under, which identifies it catalog-wide.
    alias: &'static str,
    /// The opaque identifier the control's published catalog ARN carries.
    identifier: &'static str,
    name: &'static str,
    severity: &'static str,
    parameter_requirement_summary: &'static str,
    governed_resources: &'static [&'static str],
    /// `(ImplementationType, ImplementationIdentifier)`.
    implementation: (&'static str, &'static str),
}

/// The AWS Config implementation type, as the catalog spells it.
const CONFIG_RULE: &str = "AWS::Config::ConfigRule";

/// The Security Hub implementation type, as the catalog spells it.
const SECURITY_HUB_CONTROL: &str = "AWS::SecurityHub::SecurityControl";

/// Controls whose published catalog identity the catalog records, spanning the
/// designator families the catalog publishes controls under (`CONFIG.…`, `AWS-GR_…` and
/// `SH.…`), both implementation surfaces behind them, every severity the rows carry,
/// both parameter summaries an AWS Config-implemented control can have, an empty and a
/// non-empty governed-resource list, and both members of each pair of controls that
/// share one Config rule.
const PUBLISHED_CONTROLS: &[PublishedControl] = &[
    PublishedControl {
        alias: "CONFIG.CLOUDTRAIL.DT.10",
        identifier: "d5kdbugbadk3r481vtwnvoynv",
        name: "Checks that there is at least one multi-region AWS CloudTrail",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (CONFIG_RULE, "MULTI_REGION_CLOUD_TRAIL_ENABLED"),
    },
    PublishedControl {
        alias: "CONFIG.S3.DT.8",
        identifier: "d4wgeffz6izrb627c2yb8nq8d",
        name: "Checks if S3 buckets have default encryption enabled or have bucket \
               policies that explicitly deny put-object requests without server side \
               encryption using AES-256 or AWS KMS",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_SERVER_SIDE_ENCRYPTION_ENABLED"),
    },
    PublishedControl {
        alias: "CONFIG.S3.DT.15",
        identifier: "e5efjudq0b347dgxn86o8fg2o",
        name: "Checks if S3 buckets have policies that require requests to use SSL/TLS",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_SSL_REQUESTS_ONLY"),
    },
    PublishedControl {
        alias: "CONFIG.EC2.DT.48",
        identifier: "22484xhzccf9twis5gmdt7uic",
        name: "Checks if Amazon Virtual Private Cloud (Amazon VPC) flow logs are found \
               and enabled for all Amazon VPCs",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::EC2::VPC"],
        implementation: (CONFIG_RULE, "VPC_FLOW_LOGS_ENABLED"),
    },
    PublishedControl {
        alias: "AWS-GR_S3_BUCKET_PUBLIC_READ_PROHIBITED",
        identifier: "8sw3pbid15t9cbww8d2w2qwgf",
        name: "Disallow public read access to S3 buckets",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_PUBLIC_READ_PROHIBITED"),
    },
    PublishedControl {
        alias: "AWS-GR_AUDIT_BUCKET_PUBLIC_READ_PROHIBITED",
        identifier: "9jzlkwys54gzgsx3pmkqnx8l1",
        name: "Disallow public read access to log archive",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_PUBLIC_READ_PROHIBITED"),
    },
    PublishedControl {
        alias: "AWS-GR_S3_BUCKET_PUBLIC_WRITE_PROHIBITED",
        identifier: "9j9nwxj789d82sypnukhyyowy",
        name: "Disallow public write access to S3 buckets",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_PUBLIC_WRITE_PROHIBITED"),
    },
    PublishedControl {
        alias: "AWS-GR_AUDIT_BUCKET_PUBLIC_WRITE_PROHIBITED",
        identifier: "ci9ov6vzjoi6vzkajgademvvo",
        name: "Disallow public write access to log archive",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::S3::Bucket"],
        implementation: (CONFIG_RULE, "S3_BUCKET_PUBLIC_WRITE_PROHIBITED"),
    },
    PublishedControl {
        alias: "AWS-GR_RDS_SNAPSHOTS_PUBLIC_PROHIBITED",
        identifier: "1h4eyqyyonp19dlrreqf1i3w0",
        name: "Disallow public access to RDS database snapshots",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        // The catalog publishes no available resource type for what this control
        // governs, so the list is served empty.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "RDS_SNAPSHOTS_PUBLIC_PROHIBITED"),
    },
    PublishedControl {
        alias: "AWS-GR_RDS_STORAGE_ENCRYPTED",
        identifier: "e34kieahgkm0lggs5g0s412jt",
        name: "Disallow RDS database instances without storage encryption",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::RDS::DBInstance"],
        implementation: (CONFIG_RULE, "RDS_STORAGE_ENCRYPTED"),
    },
    PublishedControl {
        alias: "SH.CloudTrail.1",
        identifier: "435u8u4w5bsf02tkxga5jisbc",
        name: "CloudTrail should be enabled and configured with at least one multi-Region \
               trail that includes read and write management events",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (SECURITY_HUB_CONTROL, "CloudTrail.1"),
    },
    PublishedControl {
        alias: "SH.IAM.4",
        identifier: "ezfflnh8xgshxb78grtt5sjc5",
        name: "IAM root user access key should not exist",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        // What this control governs — the account's own root user credentials — no
        // available resource type represents, so the catalog publishes the list empty.
        governed_resources: &[],
        implementation: (SECURITY_HUB_CONTROL, "IAM.4"),
    },
    PublishedControl {
        alias: "SH.IAM.5",
        identifier: "6g3gbim493yofkc9eg6vvnahm",
        name: "MFA should be enabled for all IAM users that have a console password",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::IAM::User"],
        implementation: (SECURITY_HUB_CONTROL, "IAM.5"),
    },
    // The AWS Config-implemented controls the catalog publishes under a `CONFIG.…`
    // designator, and the Control Tower controls an AWS Config managed rule implements.
    // They span both designator families over one implementing surface, a control whose
    // governed-resource list the catalog publishes empty because no available resource
    // type represents what it governs, every severity the catalog ranks them at, and the
    // TWO controls published over one Config rule (`CLOUD_TRAIL_ENABLED`), one per
    // governed scope.
    PublishedControl {
        alias: "CONFIG.ACCOUNT.DT.1",
        identifier: "4x11tie7h25sws4g7ilakhcb7",
        name: "Rule checks whether AWS account is part of AWS Organizations",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        // An account's membership of an organization has no resource type of its own.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "ACCOUNT_PART_OF_ORGANIZATIONS"),
    },
    PublishedControl {
        alias: "CONFIG.LOGS.DT.1",
        identifier: "497wrm2xnk1wxlf4obrdo7mej",
        name: "Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key \
               or a specified AWS KMS key Id",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::Logs::LogGroup"],
        implementation: (CONFIG_RULE, "CLOUDWATCH_LOG_GROUP_ENCRYPTED"),
    },
    PublishedControl {
        alias: "AWS-GR_DETECT_CLOUDTRAIL_ENABLED_ON_MEMBER_ACCOUNTS",
        identifier: "8sdhbqfe2fsmh3b1i2b24xaic",
        name: "Detect whether an AWS account has at least one AWS CloudTrail trail enabled \
               for a governed AWS Region.",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (CONFIG_RULE, "CLOUD_TRAIL_ENABLED"),
    },
    PublishedControl {
        alias: "AWS-GR_DETECT_CLOUDTRAIL_ENABLED_ON_SHARED_ACCOUNTS",
        identifier: "1ln29qglgkigm9ajfka6u2p2u",
        name: "Detect whether a shared AWS account in the Security organizational unit has \
               at least one AWS CloudTrail trail enabled for a governed AWS Region.",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (CONFIG_RULE, "CLOUD_TRAIL_ENABLED"),
    },
    PublishedControl {
        alias: "CONFIG.CLOUDTRAIL.DT.9",
        identifier: "85z4kpyxbo5nepzfwbfwwxjx7",
        name: "Checks if AWS CloudTrail creates a signed digest file with logs",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::CloudTrail::Trail"],
        implementation: (CONFIG_RULE, "CLOUD_TRAIL_LOG_FILE_VALIDATION_ENABLED"),
    },
    PublishedControl {
        alias: "AWS-GR_ENCRYPTED_VOLUMES",
        identifier: "503uicglhjkokaajywfpt6ros",
        name: "Enable encryption for EBS volumes attached to EC2 instances",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::EC2::Volume"],
        implementation: (CONFIG_RULE, "ENCRYPTED_VOLUMES"),
    },
    PublishedControl {
        alias: "CONFIG.GUARDDUTY.DT.9",
        identifier: "9xv17a25orapqa36e19o5b8lp",
        name: "Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region",
        severity: "HIGH",
        parameter_requirement_summary: "OPTIONAL",
        governed_resources: &["AWS::GuardDuty::Detector"],
        implementation: (CONFIG_RULE, "GUARDDUTY_ENABLED_CENTRALIZED"),
    },
    PublishedControl {
        alias: "CONFIG.IAM.DT.1",
        identifier: "dmb52g7tw004w9iede1pgb1fj",
        name: "Checks if the account password policy for AWS Identity and Access \
               Management (IAM) users meets the specified requirements indicated in the \
               parameters",
        severity: "MEDIUM",
        parameter_requirement_summary: "OPTIONAL",
        // The account's own password policy, which no available resource type represents.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "IAM_PASSWORD_POLICY"),
    },
    PublishedControl {
        alias: "CONFIG.IAM.DT.13",
        identifier: "9utgo6tl86bg3e9jaq073vmqw",
        name: "Checks if the root user access key is available",
        severity: "CRITICAL",
        parameter_requirement_summary: "NONE",
        // The account's own root-user credentials, which no available resource type
        // represents.
        governed_resources: &[],
        implementation: (CONFIG_RULE, "IAM_ROOT_ACCESS_KEY_CHECK"),
    },
    PublishedControl {
        alias: "AWS-GR_IAM_USER_MFA_ENABLED",
        identifier: "7z1uzm6s8qk7ym7m0sm56cq0u",
        name: "Disallow access to IAM users without MFA",
        severity: "MEDIUM",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::IAM::User"],
        implementation: (CONFIG_RULE, "IAM_USER_MFA_ENABLED"),
    },
    PublishedControl {
        alias: "AWS-GR_RESTRICTED_SSH",
        identifier: "6rilu41n0gb9w6mxrkyewoer4",
        name: "Disallow Internet connection via SSH",
        severity: "HIGH",
        parameter_requirement_summary: "NONE",
        governed_resources: &["AWS::EC2::SecurityGroup"],
        implementation: (CONFIG_RULE, "INCOMING_SSH_DISABLED"),
    },
];

/// The instant the catalog publishes as a control's release time (`CreateTime`), in
/// seconds since the Unix epoch, for the control this suite's table records one for:
/// `(identifier, release instant)`.
///
/// Source: the Control Catalog's own published control list, which dates each control it
/// publishes with the instant that control was released as a governance capability. The
/// wider rule — every control's published text and instant, and controls released
/// together carrying ONE instant — is owned by `all/published_control_release_record.rs`;
/// this row is here because the identity it belongs to is this suite's subject.
const PUBLISHED_RELEASE_INSTANTS: &[(&str, i64)] = &[("4x11tie7h25sws4g7ilakhcb7", 1_594_166_400)];

/// Page sizes the filtered walks below are driven with: the service's own default, one
/// control per page, a partial page and the modeled maximum of `MaxListControlsResults`
/// (`@range 1..100`).
const PAGE_SIZES: [Option<u32>; 4] = [None, Some(1), Some(7), Some(100)];

/// What a control's parameter summary must be, for controls beyond the identity table
/// above: `(designator, ParameterRequirementSummary)`.
///
/// Source: the input parameters of the surface that implements each control — the named
/// AWS Config managed rule's own input parameters, which it gives defaults for, and the
/// exemption parameters of a control deployed as a policy or a hook.
const PUBLISHED_PARAMETER_SUMMARIES: &[(&str, &str)] = &[
    // ACCOUNT_PART_OF_ORGANIZATIONS takes an input parameter.
    ("CONFIG.ACCOUNT.DT.1", "OPTIONAL"),
    // ENCRYPTED_VOLUMES takes the KMS key to check volumes against.
    ("AWS-GR_ENCRYPTED_VOLUMES", "OPTIONAL"),
    // CLOUD_TRAIL_ENABLED, behind the Control Tower member-account control, takes the
    // trail destination to check against.
    (
        "AWS-GR_DETECT_CLOUDTRAIL_ENABLED_ON_MEMBER_ACCOUNTS",
        "OPTIONAL",
    ),
    // INCOMING_SSH_DISABLED takes no input parameter.
    ("AWS-GR_RESTRICTED_SSH", "NONE"),
    // A Security Hub control is not parameterised through the catalog.
    ("SH.S3.4", "NONE"),
    // A control deployed as a service control policy, exempting principals.
    ("CT.IAM.PV.1", "OPTIONAL"),
];

/// The `Aliases` of a served control summary.
fn aliases(control: &Value) -> Vec<String> {
    strings(control, "Aliases")
}

/// Every control the catalog serves, keyed by each designator it is published under.
async fn by_alias(emulator: &Emulator) -> BTreeMap<String, Value> {
    let controls = drain(emulator, "/list-controls", "Controls", None, "{}").await;
    assert!(
        !controls.is_empty(),
        "case catalog: ListControls must publish the catalog's controls"
    );
    let mut indexed: BTreeMap<String, Value> = BTreeMap::new();
    for control in controls {
        for alias in aliases(&control) {
            assert!(
                indexed.insert(alias.clone(), control.clone()).is_none(),
                "case alias {alias}: expected one control to carry it, got a second: \
                 {control:?}"
            );
        }
    }
    indexed
}

/// Every published control is served under its own catalog ARN, with the catalog's own
/// metadata, and reads back the same way through `GetControl`.
#[tokio::test]
async fn every_published_control_is_served_with_the_catalogs_own_identity() {
    let emulator = Emulator::new();
    let indexed = by_alias(&emulator).await;

    for row in PUBLISHED_CONTROLS {
        let case = row.alias;
        let summary = indexed
            .get(row.alias)
            .unwrap_or_else(|| panic!("case {case}: the catalog must publish this control"));
        let expected_arn = catalog_arn(row.identifier);

        assert_eq!(
            summary["Arn"].as_str(),
            Some(expected_arn.as_str()),
            "case {case}: expected the catalog's ARN {expected_arn}, got {:?}",
            summary["Arn"]
        );
        assert_eq!(
            summary["Name"].as_str(),
            Some(row.name),
            "case {case}: expected the catalog's name, got {:?}",
            summary["Name"]
        );
        assert_eq!(
            summary["Severity"].as_str(),
            Some(row.severity),
            "case {case}: expected severity {}, got {:?}",
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
        // A client that harvests the ARN reads the same control back by it.
        let read_back = emulator
            .ok(
                "/get-control",
                &format!(r#"{{"ControlArn":"{expected_arn}"}}"#),
            )
            .await;
        assert_eq!(
            read_back["Arn"].as_str(),
            Some(expected_arn.as_str()),
            "case {case}: GetControl must resolve the catalog's ARN, got {read_back:?}"
        );
        assert_eq!(
            read_back["Name"].as_str(),
            summary["Name"].as_str(),
            "case {case}: GetControl and ListControls must report one name, got \
             {read_back:?}"
        );
        assert!(
            aliases(&read_back).contains(&row.alias.to_string()),
            "case {case}: GetControl must report the designator the control is published \
             under, got {:?}",
            aliases(&read_back)
        );
    }
}

/// A shared implementation publishes every control over it: both controls are served,
/// each under its own ARN and designator, and the `Identifiers` filter answers with all
/// of them whatever the page size.
#[tokio::test]
async fn a_shared_implementation_publishes_each_control_over_it() {
    let emulator = Emulator::new();
    let indexed = by_alias(&emulator).await;

    // The implementations this table publishes more than one control over, each with
    // the designators of the controls that share it.
    let mut shared: BTreeMap<&str, Vec<&PublishedControl>> = BTreeMap::new();
    for row in PUBLISHED_CONTROLS {
        shared.entry(row.implementation.1).or_default().push(row);
    }
    let shared: Vec<(&str, Vec<&PublishedControl>)> = shared
        .into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .collect();
    assert!(
        shared.len() >= 2,
        "case catalog: the table must span more than one shared implementation for the \
         rule to be tested, got {}",
        shared.len()
    );

    for (identifier, rows) in shared {
        let expected_arns: BTreeSet<String> = rows
            .iter()
            .map(|row| catalog_arn(row.identifier))
            .collect();
        assert_eq!(
            expected_arns.len(),
            rows.len(),
            "case identifier {identifier}: each control over one implementation has its \
             own ARN, got {expected_arns:?}"
        );

        let body = format!(r#"{{"Filter":{{"Implementations":{{"Identifiers":["{identifier}"]}}}}}}"#);
        // Page size 1 as well, so a shared implementation is walked across pages.
        for page_size in [None, Some(1u32)] {
            let filtered = drain(&emulator, "/list-controls", "Controls", page_size, &body).await;
            let served: BTreeSet<String> = filtered
                .iter()
                .filter_map(|control| control["Arn"].as_str().map(str::to_string))
                .collect();
            assert_eq!(
                served, expected_arns,
                "case identifier {identifier} maxResults={page_size:?}: expected every \
                 control published over this implementation, got {served:?}"
            );
        }

        // Each side keeps its own designator and its own name, so a client can tell the
        // account-wide control from the log-archive one.
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for row in &rows {
            let summary = indexed.get(row.alias).unwrap_or_else(|| {
                panic!("case identifier {identifier}: {} must be published", row.alias)
            });
            assert_eq!(
                summary["Arn"].as_str(),
                Some(catalog_arn(row.identifier).as_str()),
                "case {}: expected its own catalog ARN, got {:?}",
                row.alias,
                summary["Arn"]
            );
            assert!(
                names.insert(row.name),
                "case identifier {identifier}: the controls over it must be named \
                 distinctly, got {} twice",
                row.name
            );
        }
    }
}

/// The parameter summary follows the surface that implements the control, across the
/// designator families and implementation surfaces the catalog publishes.
#[tokio::test]
async fn the_parameter_summary_follows_the_implementing_surface() {
    let emulator = Emulator::new();
    let indexed = by_alias(&emulator).await;
    let mut summaries_seen: BTreeSet<&str> = BTreeSet::new();

    let rows = PUBLISHED_PARAMETER_SUMMARIES.iter().copied().chain(
        PUBLISHED_CONTROLS
            .iter()
            .map(|row| (row.alias, row.parameter_requirement_summary)),
    );
    for (alias, expected) in rows {
        let summary = indexed
            .get(alias)
            .unwrap_or_else(|| panic!("case {alias}: the catalog must publish this control"));
        assert_eq!(
            summary["ParameterRequirementSummary"].as_str(),
            Some(expected),
            "case {alias}: expected ParameterRequirementSummary {expected}, got {:?}",
            summary["ParameterRequirementSummary"]
        );
        summaries_seen.insert(expected);
    }

    for value in ["NONE", "OPTIONAL"] {
        assert!(
            summaries_seen.contains(value),
            "case {value}: the table must span this summary for the rule to be tested, \
             saw {summaries_seen:?}"
        );
    }
}

/// Every control the catalog serves — not only the rows above — is served under a
/// well-formed catalog ARN of its own.
#[tokio::test]
async fn every_served_control_carries_its_own_catalog_identifier() {
    let emulator = Emulator::new();
    let controls = drain(&emulator, "/list-controls", "Controls", None, "{}").await;
    assert!(
        !controls.is_empty(),
        "case catalog: ListControls must publish the catalog's controls"
    );

    // The global `controlcatalog` form the modeled `ControlArn` pattern admits, which
    // carries neither account nor Region: `arn:{PARTITION}:controlcatalog:::control/
    // {CONTROL_CATALOG_OPAQUE_ID}` (`smithy_query(query="shape", shape="ControlArn")`).
    const PREFIX: &str = "arn:aws:controlcatalog:::control/";

    let mut identifiers: BTreeSet<String> = BTreeSet::new();
    for control in &controls {
        let arn = control["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {control:?}: a control carries its ARN"));
        let identifier = arn
            .strip_prefix(PREFIX)
            .unwrap_or_else(|| panic!("case {arn}: expected the global form {PREFIX}…"));
        // The identifier is opaque published content, and the modeled `ControlArn` pattern
        // bounds its character class (`[0-9a-zA-Z_\-]+`) without fixing a length — the
        // catalog's own published identifiers are not all one length — so what is
        // asserted is that one is served, not how long it is.
        assert!(
            !identifier.is_empty(),
            "case {arn}: expected the catalog's opaque identifier after {PREFIX}"
        );
        assert!(
            identifier
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit()),
            "case {arn}: a catalog identifier is lower-case alphanumeric, got {identifier}"
        );
        assert!(
            identifiers.insert(identifier.to_string()),
            "case {arn}: two controls must not share one catalog identifier"
        );
    }
}

/// An `Implementations.Identifiers` filter — the request a client makes when it walks the
/// catalog one implementing rule at a time — serves EXACTLY the controls the catalog
/// publishes over that implementation, each under the catalog's own ARN: no control the
/// catalog publishes is dropped and none the catalog does not publish is added, whatever
/// page size walks the filtered set.
#[tokio::test]
async fn an_implementation_filter_serves_exactly_the_catalogs_controls_over_it() {
    let emulator = Emulator::new();

    // The table's controls grouped by the implementation they are published over, which
    // is the whole set the catalog publishes over each of these implementations.
    let mut per_implementation: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for row in PUBLISHED_CONTROLS {
        per_implementation
            .entry(row.implementation.1)
            .or_default()
            .insert(catalog_arn(row.identifier));
    }
    let shared = per_implementation
        .values()
        .filter(|arns| arns.len() > 1)
        .count();
    assert!(
        shared >= 2,
        "case catalog: the table must span more than one implementation published under \
         several controls for that half of the rule to be tested, got {shared}"
    );

    for (identifier, expected) in &per_implementation {
        let body = format!(r#"{{"Filter":{{"Implementations":{{"Identifiers":["{identifier}"]}}}}}}"#);
        for page_size in PAGE_SIZES {
            let case = format!("{identifier} maxResults={page_size:?}");
            let selected = drain(&emulator, "/list-controls", "Controls", page_size, &body).await;
            let served: BTreeSet<String> = selected
                .iter()
                .map(|control| {
                    control["Arn"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case}: a served control carries its ARN, got {control:?}")
                        })
                        .to_string()
                })
                .collect();
            assert_eq!(
                &served, expected,
                "case {case}: expected exactly the catalog's controls over this \
                 implementation, got {served:?}"
            );

            // Each ARN the page carries resolves to the same control, so a client that
            // harvested it from the filtered page can read the control back by it.
            for arn in &served {
                let read_back = emulator
                    .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
                    .await;
                assert_eq!(
                    read_back["Arn"].as_str(),
                    Some(arn.as_str()),
                    "case {case}: GetControl must resolve {arn}, got {read_back:?}"
                );
                assert_eq!(
                    read_back["Implementation"]["Identifier"].as_str(),
                    Some(*identifier),
                    "case {case}: GetControl must report the implementing surface the \
                     filter selected, got {read_back:?}"
                );
            }
        }
    }
}

/// A control the catalog records a release instant for is served with THAT instant, by
/// `ListControls` whatever the page size and by `GetControl`: `CreateTime` is the
/// published instant the control was released, not a value minted per read.
#[tokio::test]
async fn a_published_control_is_dated_with_the_catalogs_release_instant() {
    let emulator = Emulator::new();

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

    for page_size in PAGE_SIZES {
        let controls = drain(&emulator, "/list-controls", "Controls", page_size, "{}").await;
        let indexed: BTreeMap<String, Value> = controls
            .into_iter()
            .map(|control| {
                let arn = control["Arn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {control:?}: a control carries its ARN"))
                    .to_string();
                (arn, control)
            })
            .collect();

        for (identifier, release_time) in PUBLISHED_RELEASE_INSTANTS {
            let arn = catalog_arn(identifier);
            let case = format!("{identifier} maxResults={page_size:?}");
            let summary = indexed
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));
            assert_eq!(
                create_time(&case, summary),
                *release_time,
                "case {case}: expected the control's published release instant \
                 {release_time}, got {:?}",
                summary["CreateTime"]
            );

            let read_back = emulator
                .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
                .await;
            assert_eq!(
                create_time(&case, &read_back),
                *release_time,
                "case {case}: GetControl must report the same release instant, got \
                 {read_back:?}"
            );
        }
    }
}
