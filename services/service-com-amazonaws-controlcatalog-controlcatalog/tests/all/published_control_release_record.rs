// SPDX-License-Identifier: CC0-1.0

//! The published text and release instant a catalog control is served with.
//!
//! The rule under test: for a control the Control Catalog publishes, `Name`,
//! `Description` and `CreateTime` are the catalog's own published content for that
//! control ARN — the display name and the console description worded by the surface that
//! publishes the control, and the instant that control was released — and the catalog is
//! an Amazon Web Services-published, account- and Region-independent read-only dataset
//! (its ARNs carry neither account nor Region), so one ARN carries one name, one
//! description and one release instant for every caller, on every read and on every run.
//! `Description` is "[a] description of the control, as it may appear in the console",
//! and `CreateTime` "notes the time when the control was released (start of its life) as
//! a governance capability in Amazon Web Services"
//! (`smithy_query(query="shape", shape="ControlSummary",
//! model_path="services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json")`).
//! Two consequences each have a test below: a control's release instant is a property of
//! the control rather than of its position in any listing, so controls released together
//! carry ONE instant; and nothing a read does may re-word or re-date a control.
//!
//! Each test is a TABLE rather than one control picked by hand. The rows of the published
//! table differ in publishing surface (Control Tower's own control description against
//! the implementing AWS Config managed rule's), in whether the description restates the
//! name, in whether the release instant is shared with another control, and the whole
//! catalog — including controls no recorded page named — is covered by the stability and
//! read-only tests, which take every control the service serves.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::control_catalog::{catalog_arn, drain, Emulator};

/// One control's published catalog record.
///
/// Source: the Control Catalog's own published control list — the ARN
/// `arn:{PARTITION}:controlcatalog:::control/{identifier}` it names the control by, the
/// display name and console description it publishes for it, and the instant it records
/// as the control's release time (`CreateTime`), in seconds since the Unix epoch.
struct PublishedRecord {
    identifier: &'static str,
    name: &'static str,
    description: &'static str,
    release_time: i64,
}

/// Controls whose published record the catalog records, spanning the publishing surfaces
/// the catalog serves: a control Control Tower publishes carries Control Tower's own
/// control description, a control published over an AWS Config managed rule carries that
/// rule's own title and description, and a control Security Hub implements carries the
/// text Security Hub publishes for it.
const PUBLISHED_RECORDS: &[PublishedRecord] = &[
    PublishedRecord {
        identifier: "d5kdbugbadk3r481vtwnvoynv",
        name: "Checks that there is at least one multi-region AWS CloudTrail",
        description: "Checks that there is at least one multi-region AWS CloudTrail. The \
                      rule is non-compliant if the trails do not match input parameters",
        release_time: 1_539_993_600,
    },
    PublishedRecord {
        identifier: "d4wgeffz6izrb627c2yb8nq8d",
        name: "Checks if S3 buckets have default encryption enabled or have bucket \
               policies that explicitly deny put-object requests without server side \
               encryption using AES-256 or AWS KMS",
        description: "Checks if S3 buckets have default encryption enabled or have bucket \
                      policies that explicitly deny put-object requests without server \
                      side encryption using AES-256 or AWS KMS. The rule is \
                      NON_COMPLIANT if an S3 bucket is not encrypted by default.",
        release_time: 1_515_110_400,
    },
    PublishedRecord {
        identifier: "e5efjudq0b347dgxn86o8fg2o",
        name: "Checks if S3 buckets have policies that require requests to use SSL/TLS",
        description: "Checks if S3 buckets have policies that require requests to use \
                      SSL/TLS. The rule is NON_COMPLIANT if any S3 bucket has policies \
                      allowing HTTP requests.",
        release_time: 1_479_772_800,
    },
    PublishedRecord {
        identifier: "22484xhzccf9twis5gmdt7uic",
        name: "Checks if Amazon Virtual Private Cloud (Amazon VPC) flow logs are found \
               and enabled for all Amazon VPCs",
        description: "Checks if Amazon Virtual Private Cloud (Amazon VPC) flow logs are \
                      found and enabled for all Amazon VPCs. The rule is NON_COMPLIANT \
                      if flow logs are not enabled for at least one Amazon VPC.",
        release_time: 1_536_710_400,
    },
    PublishedRecord {
        identifier: "8sw3pbid15t9cbww8d2w2qwgf",
        name: "Disallow public read access to S3 buckets",
        description: "Secure access to data stored in Amazon S3 buckets by disallowing \
                      public reads.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "9jzlkwys54gzgsx3pmkqnx8l1",
        name: "Disallow public read access to log archive",
        description: "Control access to your log archive's Amazon S3 bucket by \
                      disallowing public read access.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "9j9nwxj789d82sypnukhyyowy",
        name: "Disallow public write access to S3 buckets",
        description: "Secure access to data stored in Amazon S3 buckets by disallowing \
                      public writes.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "ci9ov6vzjoi6vzkajgademvvo",
        name: "Disallow public write access to log archive",
        description: "Control access to your log archive's Amazon S3 bucket by \
                      disallowing public write access.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "1h4eyqyyonp19dlrreqf1i3w0",
        name: "Disallow public access to RDS database snapshots",
        description: "Secure your Amazon RDS database snapshots by disallowing public \
                      access.",
        release_time: 1_564_444_800,
    },
    PublishedRecord {
        identifier: "e34kieahgkm0lggs5g0s412jt",
        name: "Disallow RDS database instances without storage encryption",
        description: "Secure your Amazon RDS database instances at rest by encrypting the \
                      underlying storage for database instances and their automated \
                      backups, Read Replicas, and snapshots.",
        release_time: 1_564_444_800,
    },
    PublishedRecord {
        identifier: "435u8u4w5bsf02tkxga5jisbc",
        name: "CloudTrail should be enabled and configured with at least one multi-Region \
               trail that includes read and write management events",
        description: "This AWS control checks that there is at least one multi-region AWS \
                      CloudTrail trail includes read and write management events.",
        release_time: 1_669_593_600,
    },
    // Controls an AWS Config managed rule implements: each carries the published text of
    // the surface that publishes it — the rule's own title and description for a control
    // the catalog publishes under a `CONFIG.…` designator, and Control Tower's own control
    // name and description for a control Control Tower publishes — together with the
    // instant the catalog records as its release time. These rows widen the class the
    // table spans: a description that restates the name and adds the rule's own
    // NON_COMPLIANT criterion, one that names the resource field the rule reads, one that
    // quotes a policy document, one that states no criterion at all, and two controls the
    // catalog released together.
    PublishedRecord {
        identifier: "9vnj728czsv4m6pflfqt21bw2",
        name: "Checks if AWS CloudTrail trails are configured to send logs to CloudWatch \
               logs",
        description: "Checks if AWS CloudTrail trails are configured to send logs to \
                      CloudWatch logs. The trail is NON_COMPLIANT if the \
                      CloudWatchLogsLogGroupArn property of the trail is empty.",
        release_time: 1_540_166_400,
    },
    PublishedRecord {
        identifier: "87qo8rsoettjrxjevmjqcw1tu",
        name: "Detect whether Amazon EBS snapshots are restorable by all AWS accounts",
        description: "Detects whether all AWS accounts have access to restore Amazon EBS \
                      snapshots. The rule is NON_COMPLIANT if any snapshots have the \
                      RestorableByUserIds field set to the value All. In that case, the \
                      Amazon EBS snapshots are public.",
        release_time: 1_638_316_800,
    },
    PublishedRecord {
        identifier: "aazzef7lptl5umta6690p6u5c",
        name: "Checks if Application Load Balancers and Network Load Balancers are \
               configured to use certificates from Amazon Certificate Manager (ACM)",
        description: "Checks if Application Load Balancers and Network Load Balancers \
                      are configured to use certificates from Amazon Certificate Manager \
                      (ACM). This rule is NON_COMPLIANT if at least 1 load balancer is \
                      configured without a certificate from ACM.",
        release_time: 1_594_771_200,
    },
    PublishedRecord {
        identifier: "byrg7iwyl4azhecyhqtq8ocw1",
        name: "Checks whether the default version of AWS Identity and Access Management \
               (IAM) policies do not have administrator access",
        description: "Checks whether the default version of AWS Identity and Access \
                      Management (IAM) policies do not have administrator access. If any \
                      statement has \"Effect\": \"Allow\" with \"Action\": \"*\" over \
                      \"Resource\": \"*\", the rule is non-compliant.",
        release_time: 1_537_228_800,
    },
    PublishedRecord {
        identifier: "84hd48gyrb6e5ribkmsoub44c",
        name: "Checks if Amazon Relational Database Service (Amazon RDS) DB snapshots are \
               encrypted",
        description: "Checks if Amazon Relational Database Service (Amazon RDS) DB \
                      snapshots are encrypted. The rule is NON_COMPLIANT if the Amazon \
                      RDS DB snapshots are not encrypted.",
        release_time: 1_590_364_800,
    },
    PublishedRecord {
        identifier: "1oxkwnc4hwhi2ndv6ekwy7np7",
        name: "Detect whether Amazon Redshift clusters are blocked from public access",
        description: "Detects whether Amazon Redshift clusters are blocked from public \
                      access. The rule is NON_COMPLIANT if the publiclyAccessible field \
                      is true in the cluster configuration item.",
        release_time: 1_638_230_400,
    },
    PublishedRecord {
        identifier: "df2ta5ytg2zatj1q7y5e09u32",
        name: "Disallow Internet connection via RDP",
        description: "Reduces a server's exposure to risk by removing unfettered \
                      connectivity to remote console services such as RDP.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "227ph3vim64tdwv5mfagjd22r",
        name: "Checks if your AWS account is enabled to use multi-factor authentication \
               (MFA) hardware device to sign in with root credentials",
        description: "Checks if your AWS account is enabled to use multi-factor \
                      authentication (MFA) hardware device to sign in with root \
                      credentials. The rule is NON_COMPLIANT if any virtual MFA devices \
                      are permitted for signing in with root credentials.",
        release_time: 1_535_587_200,
    },
    PublishedRecord {
        identifier: "66gfl06uj1v999z53szvu0exa",
        name: "Detect whether an Amazon SageMaker notebook instance allows direct \
               internet access",
        description: "Detects whether direct internet access is allowed for an Amazon \
                      SageMaker notebook instance. The rule is NON_COMPLIANT if Amazon \
                      SageMaker notebook instances allow direct internet access.",
        release_time: 1_638_230_400,
    },
    PublishedRecord {
        identifier: "4tf01viblttjxqhu35yz90vw1",
        name: "Checks whether AWS Secret Manager secret has rotation enabled",
        description: "Checks whether AWS Secret Manager secret has rotation enabled. If \
                      the maximumAllowedRotationFrequency or the \
                      maximumAllowedRotationFrequencyInHours parameter is specified, the \
                      rotation frequency of the secret is compared with the maximum \
                      allowed frequency.",
        release_time: 1_779_840_000,
    },
    // The remaining controls an AWS Config managed rule implements. They widen the class
    // the table spans in the ways the published text varies: a description that opens with
    // "Rule checks whether" rather than restating the name, a name and description the
    // rule words around the KMS key a resource may be encrypted with, TWO controls
    // published over one rule (`CLOUD_TRAIL_ENABLED`) whose descriptions differ only in the
    // account scope each governs, a description whose last sentence is a bare verdict
    // ("Otherwise, NON_COMPLIANT."), a description shorter than its name's sentence, a
    // Control Tower description that states a protection rather than a check criterion,
    // and a control whose published name and description are a sentence each while its
    // release instant is shared with four other controls.
    PublishedRecord {
        identifier: "4x11tie7h25sws4g7ilakhcb7",
        name: "Rule checks whether AWS account is part of AWS Organizations",
        description: "Rule checks whether AWS account is part of AWS Organizations. The \
                      rule is NON_COMPLIANT if the AWS account is not part of AWS \
                      Organizations or AWS Organizations master account ID does not match \
                      rule parameter MasterAccountId.",
        release_time: 1_594_166_400,
    },
    PublishedRecord {
        identifier: "497wrm2xnk1wxlf4obrdo7mej",
        name: "Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key \
               or a specified AWS KMS key Id",
        description: "Checks if Amazon CloudWatch Log Groups are encrypted with any AWS \
                      KMS key or a specified AWS KMS key Id. The rule is NON_COMPLIANT if \
                      a CloudWatch Log Group is not encrypted with a KMS key or is \
                      encrypted with a KMS key not supplied in the rule parameter.",
        release_time: 1_563_408_000,
    },
    PublishedRecord {
        identifier: "8sdhbqfe2fsmh3b1i2b24xaic",
        name: "Detect whether an AWS account has at least one AWS CloudTrail trail enabled \
               for a governed AWS Region.",
        description: "Detects whether an AWS account has AWS CloudTrail enabled has at \
                      least one AWS CloudTrail trail enabled for a governed AWS Region. \
                      This rule is NON_COMPLIANT if no CloudTrail trails are enabled in an \
                      AWS account for a governed AWS Region.",
        release_time: 1_657_584_000,
    },
    PublishedRecord {
        identifier: "1ln29qglgkigm9ajfka6u2p2u",
        name: "Detect whether a shared AWS account in the Security organizational unit has \
               at least one AWS CloudTrail trail enabled for a governed AWS Region.",
        description: "This control detects whether a shared AWS account in the Security \
                      organizational unit has at least one AWS CloudTrail trail enabled \
                      for a governed AWS Region. This rule is NON_COMPLIANT if no \
                      CloudTrail trails are enabled in an AWS account for a governed AWS \
                      Region.",
        release_time: 1_657_584_000,
    },
    PublishedRecord {
        identifier: "85z4kpyxbo5nepzfwbfwwxjx7",
        name: "Checks if AWS CloudTrail creates a signed digest file with logs",
        description: "Checks if AWS CloudTrail creates a signed digest file with logs. AWS \
                      recommends that the file validation must be enabled on all trails. \
                      The rule is NON_COMPLIANT if the validation is not enabled.",
        release_time: 1_534_982_400,
    },
    PublishedRecord {
        identifier: "503uicglhjkokaajywfpt6ros",
        name: "Enable encryption for EBS volumes attached to EC2 instances",
        description: "Secure EBS volumes attached to EC2 instances through encryption.",
        release_time: 1_542_240_000,
    },
    PublishedRecord {
        identifier: "9xv17a25orapqa36e19o5b8lp",
        name: "Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region",
        description: "Checks if Amazon GuardDuty is enabled in your AWS account and AWS \
                      Region. If you provide an AWS account for centralization, the rule \
                      evaluates the GuardDuty results in the centralized account. The rule \
                      is COMPLIANT when GuardDuty is enabled.",
        release_time: 1_534_809_600,
    },
    PublishedRecord {
        identifier: "dmb52g7tw004w9iede1pgb1fj",
        name: "Checks if the account password policy for AWS Identity and Access \
               Management (IAM) users meets the specified requirements indicated in the \
               parameters",
        description: "Checks if the account password policy for AWS Identity and Access \
                      Management (IAM) users meets the specified requirements indicated in \
                      the parameters. The rule is NON_COMPLIANT if the account password \
                      policy does not meet the specified requirements.",
        release_time: 1_474_416_000,
    },
    PublishedRecord {
        identifier: "9utgo6tl86bg3e9jaq073vmqw",
        name: "Checks if the root user access key is available",
        description: "Checks if the root user access key is available. The rule is \
                      COMPLIANT if the user access key does not exist. Otherwise, \
                      NON_COMPLIANT.",
        release_time: 1_534_377_600,
    },
    PublishedRecord {
        identifier: "7z1uzm6s8qk7ym7m0sm56cq0u",
        name: "Disallow access to IAM users without MFA",
        description: "Protect your account by requiring multi-factor authentication (MFA) \
                      for any AWS Identity and Access Management (IAM) user using the \
                      account. MFA adds an extra authentication code on top of a user name \
                      and password.",
        release_time: 1_564_444_800,
    },
    PublishedRecord {
        identifier: "6rilu41n0gb9w6mxrkyewoer4",
        name: "Disallow Internet connection via SSH",
        description: "Reduces a server's exposure to risk by removing unfettered \
                      connectivity to remote console services such as SSH.",
        release_time: 1_542_240_000,
    },
];

/// The implementation type the catalog publishes for a control an AWS Config managed rule
/// implements, as the catalog spells it
/// (`smithy_query(query="shape", shape="ImplementationType")`).
const CONFIG_RULE: &str = "AWS::Config::ConfigRule";

/// Every control the catalog serves, keyed by its ARN, listed with `max_results` as the
/// page size and `body` as the request.
async fn listing_by_arn(
    emulator: &Emulator,
    max_results: Option<u32>,
    body: &str,
) -> BTreeMap<String, Value> {
    let controls = drain(emulator, "/list-controls", "Controls", max_results, body).await;
    assert!(
        !controls.is_empty(),
        "case maxResults={max_results:?} {body}: ListControls must publish the catalog's \
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

/// A served control carries the published name, description and release instant `row`
/// records for it.
fn assert_published_record(case: &str, control: &Value, row: &PublishedRecord) {
    assert_eq!(
        control["Name"].as_str(),
        Some(row.name),
        "case {case}: expected the catalog's published Name {:?}, got {:?}",
        row.name,
        control["Name"]
    );
    assert_eq!(
        control["Description"].as_str(),
        Some(row.description),
        "case {case}: expected the catalog's published Description {:?}, got {:?}",
        row.description,
        control["Description"]
    );
    assert_eq!(
        create_time(case, control),
        row.release_time,
        "case {case}: expected the control's published release instant {}, got {:?}",
        row.release_time,
        control["CreateTime"]
    );
}

/// Every published control is served with the catalog's own name, description and
/// release instant, by `ListControls` whatever the page size and by `GetControl`.
#[tokio::test]
async fn every_published_control_carries_the_catalogs_own_text_and_release_time() {
    let emulator = Emulator::new();

    // Page sizes spanning one entry per page, a partial page and the modeled maximum of
    // `MaxListControlsResults` (@range 1..100): the record a control is served with
    // cannot depend on how it was paged.
    for page_size in [None, Some(1u32), Some(7), Some(100)] {
        let indexed = listing_by_arn(&emulator, page_size, "{}").await;
        for row in PUBLISHED_RECORDS {
            let arn = catalog_arn(row.identifier);
            let case = format!("{} maxResults={page_size:?}", row.identifier);
            let summary = indexed
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));

            assert_published_record(&case, summary, row);

            // The summary is a projection of the record GetControl answers with, so the
            // control reads back with the same three values.
            let read_back = emulator
                .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
                .await;
            assert_eq!(
                (
                    read_back["Name"].as_str(),
                    read_back["Description"].as_str(),
                    create_time(&case, &read_back),
                ),
                (Some(row.name), Some(row.description), row.release_time),
                "case {case}: GetControl must report the same published record, got \
                 {read_back:?}"
            );
        }
    }
}

/// A published record is served through a narrowing filter too: a control selected by the
/// implementing surface's own identifier for it carries the published name, description and
/// release instant the catalog records for it — the record belongs to the control, not to
/// the request that found it. (Selecting by implementation TYPE is covered by the stability
/// test below, which compares every served control against the whole listing.)
#[tokio::test]
async fn every_published_control_keeps_its_record_when_a_filter_selects_it() {
    let emulator = Emulator::new();
    let whole_catalog = listing_by_arn(&emulator, None, "{}").await;

    for row in PUBLISHED_RECORDS {
        let arn = catalog_arn(row.identifier);
        let summary = whole_catalog.get(&arn).unwrap_or_else(|| {
            panic!("case {}: the catalog must publish {arn}", row.identifier)
        });
        let identifier = summary["Implementation"]["Identifier"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {}: a published control states the identifier the implementing \
                     surface knows it by, got {summary:?}",
                    row.identifier
                )
            });

        // The identifier filter, over the whole selection and then one control per page, so
        // the record is read on a page of its own as well as beside the controls the filter
        // selected with it.
        let body =
            format!(r#"{{"Filter":{{"Implementations":{{"Identifiers":["{identifier}"]}}}}}}"#);
        let requests = [(None, body.clone()), (Some(1u32), body)];

        for (page_size, body) in &requests {
            let selected = listing_by_arn(&emulator, *page_size, body).await;
            let case = format!("{} {body} maxResults={page_size:?}", row.identifier);
            let control = selected
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: the filter must select {arn}"));
            assert_published_record(&case, control, row);
            // The filter narrows to what it names, so the selection holds no control the
            // implementing surface knows by another identifier: without this the test would
            // pass against a filter that ignored its input.
            for (selected_arn, selected_control) in &selected {
                assert_eq!(
                    selected_control["Implementation"]["Identifier"].as_str(),
                    Some(identifier),
                    "case {case}: the identifier filter must select only controls that \
                     surface implements, got {selected_arn} with {:?}",
                    selected_control["Implementation"]
                );
            }
        }
    }
}

/// Controls the catalog released together are served with ONE release instant: the
/// instant belongs to the control, not to its position in a listing.
#[tokio::test]
async fn controls_released_together_share_one_release_instant() {
    let emulator = Emulator::new();
    let indexed = listing_by_arn(&emulator, None, "{}").await;

    // The release instants the published table records for more than one control.
    let mut grouped: BTreeMap<i64, Vec<&PublishedRecord>> = BTreeMap::new();
    for row in PUBLISHED_RECORDS {
        grouped.entry(row.release_time).or_default().push(row);
    }
    let shared: Vec<(i64, Vec<&PublishedRecord>)> = grouped
        .into_iter()
        .filter(|(_, rows)| rows.len() > 1)
        .collect();
    assert!(
        shared.len() >= 2,
        "case catalog: the table must record more than one shared release instant for \
         the rule to be tested, got {}",
        shared.len()
    );

    for (release_time, rows) in shared {
        let mut served: Vec<(String, i64)> = Vec::new();
        for row in &rows {
            let arn = catalog_arn(row.identifier);
            let case = format!("release {release_time} control {}", row.identifier);
            let summary = indexed
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: the catalog must publish {arn}"));
            served.push((arn, create_time(&case, summary)));
        }
        for (arn, actual) in &served {
            assert_eq!(
                *actual, release_time,
                "case release {release_time} control {arn}: controls released together \
                 must share one CreateTime, got {served:?}"
            );
        }
    }
}

/// Every control the catalog serves — not only the rows above — is served with the same
/// name, description and release instant whatever filter selected it and however often
/// it is read: reading the catalog neither re-words nor re-dates a control.
#[tokio::test]
async fn a_controls_published_record_is_stable_across_reads_and_filters() {
    let emulator = Emulator::new();
    let first = listing_by_arn(&emulator, None, "{}").await;

    // Requests that select the catalog differently: the whole catalog again, the same
    // catalog paged one control at a time, and each implementation type the served
    // controls report, which narrows to a subset of them.
    let mut requests: Vec<(Option<u32>, String)> =
        vec![(None, "{}".to_string()), (Some(1), "{}".to_string())];
    let mut types: Vec<String> = first
        .values()
        .filter_map(|control| control["Implementation"]["Type"].as_str().map(str::to_string))
        .collect();
    types.sort();
    types.dedup();
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

    for (page_size, body) in &requests {
        let again = listing_by_arn(&emulator, *page_size, body).await;
        for (arn, control) in &again {
            let case = format!("{arn} maxResults={page_size:?} {body}");
            let expected = first
                .get(arn)
                .unwrap_or_else(|| panic!("case {case}: the unfiltered listing must hold {arn}"));
            for member in ["Name", "Description"] {
                assert_eq!(
                    control[member], expected[member],
                    "case {case} member {member}: a control's published text must not \
                     change between reads, got {:?}",
                    control[member]
                );
            }
            assert_eq!(
                create_time(&case, control),
                create_time(&case, expected),
                "case {case}: a control's release instant must not change between reads"
            );
        }
    }
}

/// Every control an AWS Config managed rule implements carries the catalog's own published
/// name, description and release instant when the implementation-TYPE filter selects it,
/// at any page size: the published record belongs to the control ARN, not to the rule
/// behind it, the page it landed on or the filter that found it.
#[tokio::test]
async fn controls_an_aws_config_rule_implements_carry_their_published_record() {
    let emulator = Emulator::new();
    let whole_catalog = listing_by_arn(&emulator, None, "{}").await;
    let body = format!(r#"{{"Filter":{{"Implementations":{{"Types":["{CONFIG_RULE}"]}}}}}}"#);

    // The rows of the published table an AWS Config managed rule implements, read off the
    // served `Implementation` rather than listed again here.
    let config_rule_rows: Vec<&PublishedRecord> = PUBLISHED_RECORDS
        .iter()
        .filter(|row| {
            whole_catalog
                .get(&catalog_arn(row.identifier))
                .and_then(|control| control["Implementation"]["Type"].as_str())
                == Some(CONFIG_RULE)
        })
        .collect();
    assert!(
        config_rule_rows.len() >= 10,
        "case config rules: the published table must record the controls an AWS Config \
         managed rule implements for the rule to be tested, got {}",
        config_rule_rows.len()
    );

    // Page sizes spanning one control per page, a partial page and the modeled maximum of
    // `MaxListControlsResults` (@range 1..100).
    for page_size in [None, Some(1u32), Some(7), Some(100)] {
        let selected = listing_by_arn(&emulator, page_size, &body).await;

        // The filter narrows to the surface it names: without this the test would pass
        // against a filter that ignored its input.
        for (arn, control) in &selected {
            assert_eq!(
                control["Implementation"]["Type"].as_str(),
                Some(CONFIG_RULE),
                "case {arn} maxResults={page_size:?}: the type filter must select only \
                 controls that surface implements, got {:?}",
                control["Implementation"]
            );
        }

        for row in &config_rule_rows {
            let arn = catalog_arn(row.identifier);
            let case = format!("{} type-filtered maxResults={page_size:?}", row.identifier);
            let control = selected
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case}: the type filter must select {arn}"));
            assert_published_record(&case, control, row);

            // The same record reads back through GetControl, which is the read a client
            // makes with an ARN harvested from the filtered page.
            let read_back = emulator
                .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
                .await;
            assert_eq!(
                (
                    read_back["Name"].as_str(),
                    read_back["Description"].as_str(),
                    create_time(&case, &read_back),
                ),
                (Some(row.name), Some(row.description), row.release_time),
                "case {case}: GetControl must report the same published record, got \
                 {read_back:?}"
            );
        }
    }
}
