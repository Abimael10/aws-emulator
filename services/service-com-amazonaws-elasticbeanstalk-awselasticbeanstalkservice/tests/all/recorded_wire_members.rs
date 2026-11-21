// SPDX-License-Identifier: CC0-1.0

//! The members this service's awsQuery responses carry BEYOND the on-disk
//! model, driven end to end over the HTTP surface.
//!
//! The rules under test:
//!
//! * a configuration-template WRITE reports the location of the template
//!   document — the address this emulator is REACHED on, the account's storage
//!   bucket for the request's region, the saved-configuration prefix, and the
//!   application and template the request named — so a create and a later update
//!   of one template report one location, and the document that location names
//!   is one the emulator's own object storage really holds;
//! * a READ of a configuration set attributes every option of the RESOLVED set:
//!   one detail entry per reported setting, naming the option and the source its
//!   effective value came from — the API where the configuration itself holds
//!   the setting, the service where the value is the platform version's own
//!   default or one it supplies, and NO source where the option is in force at
//!   no value — beside the set's link points and the environment tier it is for;
//! * every published configuration option carries the option reference's own
//!   description and the restrictions that option has, and reports
//!   `UserDefined` false, because the catalog publishes what the PLATFORM
//!   VERSION defines rather than what a user defined; the catalog is reported
//!   for the tier the configuration it was asked about is for;
//! * the account's quotas carry one entry per resource Elastic Beanstalk bounds,
//!   health groups included;
//! * an `EnvironmentId` is minted in the form the real service issues one in.
//!
//! Each table row is a different point in those classes — a different region, a
//! different pair of names, a configuration set that writes no option and one
//! that writes two, a template and a running environment, a web-server tier and
//! a worker tier — not a renamed copy of the row before it.
//!
//! The HTTP harness (`Emulator`, `Answer`) is the one `all/lifecycle.rs` owns
//! and the option encoders and readers are `all/configuration_templates.rs`'s,
//! so nothing here reaches into the store and no second copy of either can
//! drift.

use std::collections::BTreeSet;

use crate::configuration_templates::{
    as_params, created, option_params, published_stack_of_category,
};
use crate::lifecycle::{Answer, Emulator, TEST_REGION};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The namespace of the environment variables an application reads — options the
/// CALLER names, which the published catalog therefore does not define.
const APP_ENV: &str = "aws:elasticbeanstalk:application:environment";
/// The Auto Scaling group namespace, which holds the two sibling options the
/// recorded option descriptions are about.
const ASG: &str = "aws:autoscaling:asg";
/// A scheduled scaling action: the namespace whose settings carry a
/// `ResourceName` of their own, which the detail of such a setting names.
const SCHEDULED: &str = "aws:autoscaling:scheduledaction";
/// The template parameter the service puts the instance type's FAMILY in force
/// as — the one supplied option a value is in force for.
const INSTANCE_TYPE_FAMILY: &str = "InstanceTypeFamily";

/// The worker tier's two members, as a request names them.
const WORKER_TIER: &[(&str, &str)] = &[("Tier.Name", "Worker"), ("Tier.Type", "SQS/HTTP")];

/// One `OptionSettingDetails` entry as the answer reports it: the triple that
/// NAMES the option (`ResourceName`, `Namespace`, `OptionName`) and the
/// `OptionSource`'s `Identifier` and `Origin`, read member by member off ONE
/// element so a detail attributed to the wrong option fails rather than matching
/// a flat list. An entry carrying no `OptionSource` reads as two empty strings.
type ReportedDetail = (String, String, String, String, String);

/// Every option-setting detail in `answer`, in document order.
fn reported_details(answer: &Answer) -> Vec<ReportedDetail> {
    answer
        .elements_at("OptionSettingDetails/member")
        .iter()
        .map(|member| {
            let source = member.elements_at("OptionSource");
            let source = source.first();
            (
                member.value("ResourceName").unwrap_or_default().to_string(),
                member.value("Namespace").unwrap_or_default().to_string(),
                member.value("OptionName").unwrap_or_default().to_string(),
                source
                    .and_then(|source| source.value("Identifier"))
                    .unwrap_or_default()
                    .to_string(),
                source
                    .and_then(|source| source.value("Origin"))
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The identity triple of each reported `OptionSettings` entry —
/// `(ResourceName, Namespace, OptionName)` — with whether that setting carries a
/// VALUE: an option the set reports in force at NO value carries no `Value`
/// member at all.
fn reported_settings(answer: &Answer) -> Vec<(String, String, String, bool)> {
    answer
        .elements_at("OptionSettings/member")
        .iter()
        .map(|member| {
            (
                member.value("ResourceName").unwrap_or_default().to_string(),
                member.value("Namespace").unwrap_or_default().to_string(),
                member.value("OptionName").unwrap_or_default().to_string(),
                member.value("Value").is_some(),
            )
        })
        .collect()
}

/// The identity triples of the reported `OptionSettings` list.
fn reported_setting_keys(answer: &Answer) -> Vec<(String, String, String)> {
    reported_settings(answer)
        .into_iter()
        .map(|(resource_name, namespace, option_name, _)| (resource_name, namespace, option_name))
        .collect()
}

/// The `Tier` an answer reports at `path`, as `(Name, Type, Version)`.
fn reported_tier(answer: &Answer, path: &str) -> (String, String, String) {
    let tier = answer.elements_at(path);
    let tier = tier.first();
    (
        tier.and_then(|tier| tier.value("Name"))
            .unwrap_or_default()
            .to_string(),
        tier.and_then(|tier| tier.value("Type"))
            .unwrap_or_default()
            .to_string(),
        tier.and_then(|tier| tier.value("Version"))
            .unwrap_or_default()
            .to_string(),
    )
}

/// Call `action` in `region` and require a 200, as [`created`] does for the
/// test region.
async fn created_in_region(
    emulator: &Emulator,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    let answer = emulator.call_in_region(region, action, params).await;
    assert_eq!(
        answer.status, 200,
        "expected 200 from {action} in {region}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

// ---------------------------------------------------------------------------
// The stored template's location
// ---------------------------------------------------------------------------

/// One row of the template-location table: where the template is created and
/// what it and its application are called.
struct LocationCase {
    case: &'static str,
    region: &'static str,
    application: &'static str,
    template: &'static str,
}

#[tokio::test]
async fn a_template_write_reports_the_stored_templates_s3_location() {
    // Rows differ along the two variable parts of the location: the region the
    // account's storage bucket is in, and the application and template names
    // the request supplies (a one-character name, a name carrying the
    // separators a saved configuration's name may carry).
    let cases = [
        LocationCase {
            case: "the signing region",
            region: TEST_REGION,
            application: "location-app",
            template: "python-template",
        },
        LocationCase {
            case: "another region",
            region: "eu-central-1",
            application: "location-app",
            template: "t",
        },
        LocationCase {
            case: "names carrying separators",
            region: "ap-southeast-2",
            application: "location.app-2",
            template: "web_server.v2-template",
        },
    ];

    for case in cases {
        let emulator = Emulator::with_object_storage();
        let stack = published_stack_of_category("Python").name();
        created_in_region(
            &emulator,
            case.region,
            "CreateApplication",
            &[("ApplicationName", case.application)],
        )
        .await;
        // The fixed parts of the location — the address this emulator is
        // REACHED on, the `elasticbeanstalk-<region>-<account>` bucket form and
        // the saved-configuration prefix — are written out here rather than
        // read back from the service, so a service that changed any of them
        // fails. A production Amazon S3 hostname is not one this emulator
        // answers on, so a client that followed it would never reach the
        // document this service stored.
        let key = format!(
            "{}/{}",
            eb::state::storage_bucket_name(case.region),
            eb::state::configuration_template_key(case.application, case.template)
        );
        let expected = format!("{}/{key}", smithy_server_runtime::UNSERVED_URL_BASE);

        let template_created = created_in_region(
            &emulator,
            case.region,
            "CreateConfigurationTemplate",
            &[
                ("ApplicationName", case.application),
                ("TemplateName", case.template),
                ("SolutionStackName", &stack),
                ("Description", "a described saved configuration"),
            ],
        )
        .await;
        assert_eq!(
            template_created.value("TemplateUrl"),
            Some(expected.as_str()),
            "case {:?}: the create must report the stored template's location, got {}",
            case.case,
            template_created.body
        );

        // The location is FOLLOWED: the document it names is one the emulator's
        // own object storage really holds, so the bytes a client downloads are
        // the configuration this service stored — its description and the
        // platform version it is for.
        let platform = template_created
            .value("PlatformArn")
            .expect("a created template reports the platform version it is for")
            .to_string();
        let downloaded = emulator
            .s3_in_region(case.region, "GET", &format!("/{key}"), &[])
            .await;
        assert_eq!(
            downloaded.status, 200,
            "case {:?}: the reported location must hand back the stored document, got {} and \
             body {}",
            case.case, downloaded.status, downloaded.body
        );
        assert!(
            downloaded.body.contains("a described saved configuration")
                && downloaded.body.contains(&platform),
            "case {:?}: the stored document must be this template's own configuration, got {}",
            case.case,
            downloaded.body
        );

        // The same location on the update path: one template, one location,
        // whatever the write — and the document that location names is the
        // configuration the update settled.
        let updated = created_in_region(
            &emulator,
            case.region,
            "UpdateConfigurationTemplate",
            &[
                ("ApplicationName", case.application),
                ("TemplateName", case.template),
                ("Description", "an updated saved configuration"),
            ],
        )
        .await;
        assert_eq!(
            updated.value("TemplateUrl"),
            Some(expected.as_str()),
            "case {:?}: the update must report the same location, got {}",
            case.case,
            updated.body
        );
        let downloaded = emulator
            .s3_in_region(case.region, "GET", &format!("/{key}"), &[])
            .await;
        assert!(
            downloaded.status == 200
                && downloaded.body.contains("an updated saved configuration"),
            "case {:?}: the location must hand back the UPDATED document, got {} and body {}",
            case.case,
            downloaded.status,
            downloaded.body
        );

        // A READ of the configuration set is not a template write: it reports
        // where every option's value came from (below) and no stored location.
        let described = created_in_region(
            &emulator,
            case.region,
            "DescribeConfigurationSettings",
            &[
                ("ApplicationName", case.application),
                ("TemplateName", case.template),
            ],
        )
        .await;
        assert_eq!(
            described.value("TemplateUrl"),
            None,
            "case {:?}: a read must report no template location, got {}",
            case.case,
            described.body
        );

        // A DELETED saved configuration has no document: the location the
        // writes reported stops handing one back.
        created_in_region(
            &emulator,
            case.region,
            "DeleteConfigurationTemplate",
            &[
                ("ApplicationName", case.application),
                ("TemplateName", case.template),
            ],
        )
        .await;
        let gone = emulator
            .s3_in_region(case.region, "GET", &format!("/{key}"), &[])
            .await;
        assert_eq!(
            gone.status, 404,
            "case {:?}: a deleted template's document must be gone, got {} and body {}",
            case.case, gone.status, gone.body
        );
    }
}

// ---------------------------------------------------------------------------
// What a read of a configuration set attributes
// ---------------------------------------------------------------------------

/// One row of the attribution table: which configuration set is read, the
/// settings the set itself holds, and the tier it is for.
struct AttributionCase {
    case: &'static str,
    /// `true` for a saved template, `false` for a running environment's
    /// configuration.
    template: bool,
    /// The settings the configuration set writes, as
    /// `(Namespace, OptionName, Value, ResourceName)`.
    written: &'static [(&'static str, &'static str, &'static str, &'static str)],
    /// The tier the environment is created in, where the row is an environment.
    tier: &'static [(&'static str, &'static str)],
    /// `(Name, Type, Version)` the read must report the configuration's tier as.
    reported_tier: (&'static str, &'static str, &'static str),
}

#[tokio::test]
async fn a_read_of_a_configuration_set_attributes_every_option_and_names_its_tier() {
    // Rows differ along what the configuration set holds and which tier it is
    // for: a template that writes nothing (every value the platform's own), a
    // template that overrides a published option AND adds one the catalog does
    // not define, a web-server environment that set one option, and a worker
    // environment, whose tier is reported as its own.
    let cases = [
        AttributionCase {
            case: "a template that writes no option",
            template: true,
            written: &[],
            tier: &[],
            reported_tier: ("WebServer", "Standard", "1.0"),
        },
        AttributionCase {
            case: "a template that overrides one option and adds another",
            template: true,
            written: &[
                (ASG, "MinSize", "3", ""),
                (APP_ENV, "SPRING_PROFILES_ACTIVE", "prod", ""),
            ],
            tier: &[],
            reported_tier: ("WebServer", "Standard", "1.0"),
        },
        AttributionCase {
            case: "a web server environment that set one option",
            template: false,
            written: &[(APP_ENV, "SPRING_PROFILES_ACTIVE", "prod", "")],
            tier: &[],
            reported_tier: ("WebServer", "Standard", "1.0"),
        },
        AttributionCase {
            case: "a template whose setting names a resource of its own",
            template: true,
            written: &[(SCHEDULED, "MaxSize", "8", "peak-hours")],
            tier: &[],
            reported_tier: ("WebServer", "Standard", "1.0"),
        },
        AttributionCase {
            case: "a worker environment",
            template: false,
            written: &[("aws:elasticbeanstalk:sqsd", "HttpPath", "/work", "")],
            tier: WORKER_TIER,
            reported_tier: ("Worker", "SQS/HTTP", "1.0"),
        },
    ];

    for case in cases {
        let emulator = Emulator::new();
        let stack = published_stack_of_category("Python").name();
        let application = "attribution-app";
        created(
            &emulator,
            "CreateApplication",
            &[("ApplicationName", application)],
        )
        .await;

        let name = "configuration-set";
        let mut params: Vec<(String, String)> = vec![
            ("ApplicationName".to_string(), application.to_string()),
            ("SolutionStackName".to_string(), stack.clone()),
        ];
        params.extend(option_params(case.written));
        let read = if case.template {
            params.push(("TemplateName".to_string(), name.to_string()));
            created(
                &emulator,
                "CreateConfigurationTemplate",
                &as_params(&params),
            )
            .await;
            created(
                &emulator,
                "DescribeConfigurationSettings",
                &[("ApplicationName", application), ("TemplateName", name)],
            )
            .await
        } else {
            params.push(("EnvironmentName".to_string(), name.to_string()));
            params.extend(
                case.tier
                    .iter()
                    .map(|(member, value)| ((*member).to_string(), (*value).to_string())),
            );
            created(&emulator, "CreateEnvironment", &as_params(&params)).await;
            created(
                &emulator,
                "DescribeConfigurationSettings",
                &[("ApplicationName", application), ("EnvironmentName", name)],
            )
            .await
        };

        // One detail per reported setting, naming the same option in the same
        // position: a client reads the two lists together.
        let settings = reported_setting_keys(&read);
        let details = reported_details(&read);
        assert!(
            !settings.is_empty(),
            "case {:?}: expected a resolved option set, got {}",
            case.case,
            read.body
        );
        assert_eq!(
            details
                .iter()
                .map(|(resource_name, namespace, option_name, _, _)| (
                    resource_name.clone(),
                    namespace.clone(),
                    option_name.clone()
                ))
                .collect::<Vec<(String, String, String)>>(),
            settings,
            "case {:?}: every reported setting must carry its detail, got {}",
            case.case,
            read.body
        );

        // The set carries the options the platform version SUPPLIES to the
        // environment it provisions — read from the service, so the row names
        // none of them — and the service puts the instance type's family in
        // force among them.
        let stack_of_the_set = eb::state::solution_stack_named(&stack);
        let supplied = eb::state::platform_supplied_options(stack_of_the_set);
        assert!(
            !supplied.is_empty(),
            "case {:?}: the platform version must supply options of its own",
            case.case
        );
        for (namespace, option_name) in &supplied {
            assert!(
                settings.iter().any(|(_, reported_namespace, reported_name)| {
                    reported_namespace == namespace && reported_name == option_name
                }),
                "case {:?}: {namespace}/{option_name} is supplied by the platform, so the \
                 resolved set must carry it, got {}",
                case.case,
                read.body
            );
        }
        let instance_type_family = reported_settings(&read)
            .into_iter()
            .find(|(_, _, option_name, _)| option_name == INSTANCE_TYPE_FAMILY)
            .map(|(_, _, _, carries_a_value)| carries_a_value);
        assert_eq!(
            instance_type_family,
            Some(true),
            "case {:?}: the instance type's family is a value the service puts in force, got {}",
            case.case,
            read.body
        );

        // The source of each value: the API where the configuration set itself
        // holds the setting, the service where the value is the platform
        // version's own default or one it supplies — and NO source at all where
        // the option is in force at no value, because no layer put one there.
        let written: BTreeSet<(String, String)> = case
            .written
            .iter()
            .map(|(namespace, option_name, _, _)| {
                ((*namespace).to_string(), (*option_name).to_string())
            })
            .collect();
        for ((_, namespace, option_name, identifier, origin), (_, _, _, carries_a_value)) in
            details.iter().zip(reported_settings(&read))
        {
            let expected = if !carries_a_value {
                ""
            } else if written.contains(&(namespace.clone(), option_name.clone())) {
                eb::state::OPTION_SOURCE_API
            } else {
                eb::state::OPTION_SOURCE_SERVICE
            };
            assert_eq!(
                (identifier.as_str(), origin.as_str()),
                (expected, expected),
                "case {:?}: unexpected source for {namespace}/{option_name}, got {}",
                case.case,
                read.body
            );
        }
        // At least one option is in force at no value on every platform
        // version this service publishes — the parameters it supplies no value
        // for — so the no-source half of the rule above is really exercised.
        assert!(
            reported_settings(&read)
                .iter()
                .any(|(_, _, _, carries_a_value)| !carries_a_value),
            "case {:?}: expected an option in force at no value, got {}",
            case.case,
            read.body
        );
        // Every option the request set is in the resolved set, so the detail
        // above really did cover it.
        for (namespace, option_name) in &written {
            assert!(
                settings.iter().any(|(_, reported_namespace, reported_name)| {
                    reported_namespace == namespace && reported_name == option_name
                }),
                "case {:?}: {namespace}/{option_name} must be in the resolved set, got {}",
                case.case,
                read.body
            );
        }

        // An element present and EMPTY is the recorded answer here, which is a
        // different answer from an absent member — so the reader that keeps an
        // empty element's text is the one this asserts through.
        assert_eq!(
            read.values_at_including_empty("ConfigurationSettings/member/EnvironmentLinkPoints"),
            vec![""],
            "case {:?}: the read must report the set's link points, empty for a configuration \
             set that links to no environment, got {}",
            case.case,
            read.body
        );
        let (name_reported, type_reported, version) =
            reported_tier(&read, "ConfigurationSettings/member/Tier");
        assert_eq!(
            (
                name_reported.as_str(),
                type_reported.as_str(),
                version.as_str()
            ),
            case.reported_tier,
            "case {:?}: unexpected Tier, got {}",
            case.case,
            read.body
        );
    }
}

// ---------------------------------------------------------------------------
// What a published option description carries
// ---------------------------------------------------------------------------

/// One row of the published-option table: the option, the upper bound its
/// description publishes, and the text it carries where the text is pinned here
/// rather than read from the service's own table.
struct PublishedOptionCase {
    case: &'static str,
    namespace: &'static str,
    name: &'static str,
    max_value: Option<&'static str>,
    /// The recorded sentence, for the two options the recorded wire responses
    /// state it for; `None` for an option whose sentence is asserted against
    /// the service's own table instead.
    description: Option<&'static str>,
}

#[tokio::test]
async fn every_published_option_carries_its_description_and_is_published_by_the_platform() {
    let emulator = Emulator::new();
    // A Python platform version, so the rows can span the general catalog and a
    // runtime's own options.
    let stack = published_stack_of_category("Python").name();
    let answer = created(
        &emulator,
        "DescribeConfigurationOptions",
        &[("SolutionStackName", &stack)],
    )
    .await;

    // Rows differ along where the option comes from and what bound it
    // publishes: the two Auto Scaling group sizes the recorded responses state
    // the sentence and the 10000 bound for, a deployment option the issue does
    // not cite (its own, smaller bound), and a runtime's own option.
    let cases = [
        PublishedOptionCase {
            case: "the Auto Scaling maximum",
            namespace: ASG,
            name: "MaxSize",
            max_value: Some("10000"),
            description: Some(
                "Maximum number of instances you want running. Must be a positive integer that \
                 is less than 10000.",
            ),
        },
        PublishedOptionCase {
            case: "the Auto Scaling minimum",
            namespace: ASG,
            name: "MinSize",
            max_value: Some("10000"),
            description: Some(
                "Minimum number of instances you want running. Must be a positive integer no \
                 greater than MaxSize.",
            ),
        },
        PublishedOptionCase {
            case: "a deployment option with a bound of its own",
            namespace: "aws:elasticbeanstalk:command",
            name: "Timeout",
            max_value: Some("3600"),
            description: None,
        },
        PublishedOptionCase {
            case: "a runtime's own option",
            namespace: "aws:elasticbeanstalk:container:python",
            name: "NumThreads",
            max_value: Some("1000"),
            description: None,
        },
    ];

    let options = answer.elements_at("Options/member");
    for case in cases {
        let option = options
            .iter()
            .find(|option| {
                option.value("Namespace") == Some(case.namespace)
                    && option.value("Name") == Some(case.name)
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a description of {} / {}, got {}",
                    case.case, case.namespace, case.name, answer.body
                )
            });
        assert_eq!(
            option.value("MaxValue"),
            case.max_value,
            "case {:?}: unexpected MaxValue, got {}",
            case.case,
            option.body
        );
        // The sentence the recorded responses state, where they state one;
        // otherwise the one the service's own option table holds for that
        // option, so the wire and the table cannot disagree.
        let expected = case
            .description
            .or_else(|| eb::state::option_description(case.namespace, case.name));
        assert_eq!(
            option.value("Description"),
            expected,
            "case {:?}: unexpected Description, got {}",
            case.case,
            option.body
        );
    }

    // The whole catalog, not just the rows above: every option a platform
    // version defines carries a sentence, and reports `UserDefined` false —
    // what the catalog publishes is what the PLATFORM defines, never what a
    // user defined.
    for option in &options {
        let name = format!(
            "{}/{}",
            option.value("Namespace").unwrap_or_default(),
            option.value("Name").unwrap_or_default()
        );
        assert!(
            option.value("Description").is_some(),
            "case {name:?}: every published option carries a description, got {}",
            option.body
        );
        assert_eq!(
            option.value("UserDefined"),
            Some("false"),
            "case {name:?}: a platform-published option is not user defined, got {}",
            option.body
        );
    }

    // The catalog is reported for the tier the configuration it was asked about
    // is for: the tier every configuration starts in for a platform version
    // named outright, and an environment's own tier for a request that names
    // one.
    assert_eq!(
        reported_tier(&answer, "Tier"),
        (
            "WebServer".to_string(),
            "Standard".to_string(),
            "1.0".to_string()
        ),
        "a platform version's catalog is the web server tier's, got {}",
        answer.body
    );

    let application = "options-tier-app";
    created(
        &emulator,
        "CreateApplication",
        &[("ApplicationName", application)],
    )
    .await;
    let mut params: Vec<(String, String)> = vec![
        ("ApplicationName".to_string(), application.to_string()),
        ("EnvironmentName".to_string(), "worker-env".to_string()),
        ("SolutionStackName".to_string(), stack.clone()),
    ];
    params.extend(
        WORKER_TIER
            .iter()
            .map(|(member, value)| ((*member).to_string(), (*value).to_string())),
    );
    created(&emulator, "CreateEnvironment", &as_params(&params)).await;
    let worker = created(
        &emulator,
        "DescribeConfigurationOptions",
        &[
            ("ApplicationName", application),
            ("EnvironmentName", "worker-env"),
        ],
    )
    .await;
    assert_eq!(
        reported_tier(&worker, "Tier"),
        (
            "Worker".to_string(),
            "SQS/HTTP".to_string(),
            "1.0".to_string()
        ),
        "a worker environment's catalog is the worker tier's, got {}",
        worker.body
    );
}

// ---------------------------------------------------------------------------
// The account's quotas
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_account_quotas_carry_one_entry_per_bounded_resource() {
    let emulator = Emulator::new();
    let answer = created(&emulator, "DescribeAccountAttributes", &[]).await;

    // One row per resource this service bounds, each with the bound it enforces
    // for it, read from the service's own constants rather than restated.
    let cases = [
        ("ApplicationQuota", eb::state::MAX_APPLICATIONS),
        (
            "ApplicationVersionQuota",
            eb::state::MAX_APPLICATION_VERSIONS,
        ),
        ("EnvironmentQuota", eb::state::MAX_ENVIRONMENTS),
        (
            "ConfigurationTemplateQuota",
            eb::state::MAX_CONFIGURATION_TEMPLATES,
        ),
        ("CustomPlatformQuota", eb::state::MAX_PLATFORM_VERSIONS),
        // A health group is the unit enhanced health reports an environment's
        // instances under — one per environment — so the account holds as many
        // as it holds environments.
        ("HealthGroupQuota", eb::state::MAX_ENVIRONMENTS),
    ];

    for (quota, maximum) in cases {
        let reported = answer.value(&format!("ResourceQuotas/{quota}/Maximum"));
        assert_eq!(
            reported,
            Some(maximum.to_string().as_str()),
            "case {quota:?}: expected the bound this service enforces, got {}",
            answer.body
        );
    }
}

// ---------------------------------------------------------------------------
// The form of a minted environment id
// ---------------------------------------------------------------------------

/// How many characters the body of an `EnvironmentId` carries in the form the
/// real service issues one in: `e-` followed by a short fixed-width token of
/// lowercase alphanumerics, as the recorded responses carry it.
const RECORDED_ENVIRONMENT_ID_BODY: usize = 10;

#[tokio::test]
async fn a_minted_environment_id_has_the_published_form() {
    let emulator = Emulator::new();
    let stack = published_stack_of_category("Python").name();
    let application = "id-form-app";
    created(
        &emulator,
        "CreateApplication",
        &[("ApplicationName", application)],
    )
    .await;

    // Rows differ along what is being launched — the tier, and whether the
    // request sets an option — so the id's form cannot depend on the request.
    let cases: [(&str, &[(&str, &str)]); 3] = [
        ("a web server environment", &[]),
        ("a worker environment", WORKER_TIER),
        (
            "an environment that set an option",
            &[("OptionSettings.member.1.Namespace", APP_ENV)],
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (index, (case, extra)) in cases.iter().enumerate() {
        let name = format!("id-env-{index}");
        let mut params: Vec<(String, String)> = vec![
            ("ApplicationName".to_string(), application.to_string()),
            ("EnvironmentName".to_string(), name.clone()),
            ("SolutionStackName".to_string(), stack.clone()),
        ];
        params.extend(
            extra
                .iter()
                .map(|(member, value)| ((*member).to_string(), (*value).to_string())),
        );
        if case.contains("set an option") {
            params.push((
                "OptionSettings.member.1.OptionName".to_string(),
                "STAGE".to_string(),
            ));
            params.push((
                "OptionSettings.member.1.Value".to_string(),
                "qa".to_string(),
            ));
        }
        let answer = created(&emulator, "CreateEnvironment", &as_params(&params)).await;
        let id = answer.expect("EnvironmentId", case).to_string();
        let body = id
            .strip_prefix("e-")
            .unwrap_or_else(|| panic!("case {case:?}: expected an e- prefixed id, got {id:?}"));
        assert_eq!(
            body.len(),
            RECORDED_ENVIRONMENT_ID_BODY,
            "case {case:?}: expected an id body of the published width, got {id:?}"
        );
        assert!(
            body.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "case {case:?}: expected a lowercase alphanumeric id body, got {id:?}"
        );
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: {id:?} was already issued to another environment"
        );
    }
}
