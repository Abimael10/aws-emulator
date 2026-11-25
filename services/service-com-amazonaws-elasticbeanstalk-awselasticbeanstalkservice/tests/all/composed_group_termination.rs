// SPDX-License-Identifier: CC0-1.0

//! A COMPOSED environment is a first-class environment of its application: it is
//! MATERIALIZED, not merely answered, and it can be terminated on its own.
//!
//! The rule under test: `ComposeEnvironments` "creates or updates a group of
//! environments that each run a separate component of a single application" from
//! the environment manifest in each named version's source bundle, so every
//! environment the call composes is a stored environment of that application —
//! reported by `DescribeEnvironments` under the name the manifest gave it (a
//! manifest name ending in the `+` placeholder completed by the request's group
//! name), with the application it belongs to, the version whose bundle described
//! it, its own ARN and the manifest's tier; carrying the manifest's option
//! settings in `DescribeConfigurationSettings`; reaching `Ready` the way every
//! launch does; and terminable ONE at a time with `TerminateEnvironment`, after
//! which that one environment leaves the default (`IncludeDeleted` false) view
//! and the rest of the group stays in it. The response's `Environments` list is
//! not what proves any of this: the stored group a later reader sees is.
//!
//! The rows span what that rule generalises over: whether the manifest's
//! `EnvironmentName` is a group template or a name written out in full, which
//! tier the manifest declares and whether it declares one at all, which option
//! setting it carries and in which namespace, which published solution stack it
//! names, and — over the termination loop — which member of the group is
//! terminated while the others are still running.
//!
//! Every version's source bundle is uploaded to the bucket
//! `CreateStorageLocation` publishes, the documented first step of a deployment,
//! so the manifests this composition reads come out of object storage the way a
//! client's do.
//!
//! What a composition reads out of a manifest at all is `composed_environments.rs`'s
//! subject; what is added here is the MATERIALIZATION and the individual
//! termination of the composed group. The harness (the router, the form encoder,
//! the XML reader and the status polls) is the one `lifecycle.rs` owns in this
//! same test binary; the archive writer is the one
//! `application_version_processing.rs` owns.

use crate::application_version_processing::zip_of;
use crate::composed_environments::WORKER_TIER_TYPE;
use crate::harness::{wait_for_status, wait_for_version_status, Answer, Emulator};
use crate::lifecycle::{account, TEST_REGION};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application the group is composed under.
const APPLICATION: &str = "composed-group-app";

/// The group the composition names, which completes every manifest name ending
/// in the group-name placeholder.
const GROUP_NAME: &str = "stage";

/// The namespace and the option that name the IAM instance profile an
/// environment's instances are given, as Elastic Beanstalk's published
/// configuration-option reference spells them, and the profile every manifest
/// here associates with its environment — without one no launch can be
/// provisioned (the rule `environment_launch_authorization.rs` owns), so every
/// manifest below supplies one.
const LAUNCH_CONFIGURATION: &str = "aws:autoscaling:launchconfiguration";
/// See [`LAUNCH_CONFIGURATION`].
const IAM_INSTANCE_PROFILE: &str = "IamInstanceProfile";
/// See [`LAUNCH_CONFIGURATION`].
const INSTANCE_PROFILE: &str = "eb-ec2-role";

/// The option that names the instance type an environment's instances run, in
/// that same namespace, as the published configuration-option reference spells
/// it: a SECOND option beside the instance profile, so a row asserts a manifest
/// setting that shares its namespace with one the manifest has to carry anyway.
const INSTANCE_TYPE: &str = "InstanceType";

/// One component of the composed group: the version label whose bundle carries
/// its manifest, what that manifest declares, and what the environment the
/// composition materializes must then report.
struct Component {
    case: &'static str,
    version_label: &'static str,
    /// `EnvironmentName` as the manifest writes it — a group template when it
    /// ends in `+`.
    manifest_name: &'static str,
    /// The environment the group holds for that manifest.
    expected_name: &'static str,
    /// `EnvironmentTier` as the manifest declares it, or `None` when it declares
    /// none at all.
    tier: Option<(&'static str, &'static str)>,
    /// The tier the composed environment must report, which is the model's
    /// default where the manifest declared none.
    expected_tier: (&'static str, &'static str),
    /// One option setting the manifest carries, as `(namespace, option, value)`.
    setting: (&'static str, &'static str, &'static str),
}

/// The solution stack each component names: the first published stack for the
/// first two rows and the second for the third, read off the service's own
/// catalogue rather than written here, so each environment is asserted to have
/// taken its OWN manifest's platform.
fn solution_stack(index: usize) -> String {
    eb::state::SOLUTION_STACKS
        .get(index)
        .expect("the service publishes more than one solution stack")
        .name()
}

/// The group's components. Rows differ along the form of the manifest's
/// environment name, the tier it declares, and the namespace and value of the
/// option setting it carries.
fn components() -> Vec<Component> {
    vec![
        // A group template with no tier at all: the composed environment reports
        // the tier the model documents as the default, and an application
        // property out of the environment-properties namespace.
        Component {
            case: "a web template with no tier and an environment property",
            version_label: "cmpa-1",
            manifest_name: "cmpa+",
            expected_name: "cmpa-stage",
            tier: None,
            expected_tier: (
                eb::state::TIER_NAME_WEB_SERVER,
                eb::state::TIER_TYPE_STANDARD,
            ),
            setting: (
                eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                "GREETING",
                "hello from cmpa",
            ),
        },
        // A group template declaring the worker tier, and a setting in the same
        // namespace as the instance profile every manifest carries.
        Component {
            case: "a worker template with an instance type",
            version_label: "cmpb-1",
            manifest_name: "cmpb+",
            expected_name: "cmpb-stage",
            tier: Some((eb::state::TIER_NAME_WORKER, WORKER_TIER_TYPE)),
            expected_tier: (eb::state::TIER_NAME_WORKER, WORKER_TIER_TYPE),
            setting: (LAUNCH_CONFIGURATION, INSTANCE_TYPE, "t3.small"),
        },
        // A manifest that names its environment outright: no group completes it,
        // so the group holds it under the name as written.
        Component {
            case: "a manifest naming its environment outright",
            version_label: "cmpc-1",
            manifest_name: "cmpc-fixed",
            expected_name: "cmpc-fixed",
            tier: Some((
                eb::state::TIER_NAME_WEB_SERVER,
                eb::state::TIER_TYPE_STANDARD,
            )),
            expected_tier: (
                eb::state::TIER_NAME_WEB_SERVER,
                eb::state::TIER_TYPE_STANDARD,
            ),
            setting: (LAUNCH_CONFIGURATION, INSTANCE_TYPE, "t3.medium"),
        },
    ]
}

/// An environment manifest, as Elastic Beanstalk's environment-manifest
/// reference lays `env.yaml` out: the configuration-template version, the
/// environment's name, the solution stack, the tier, and the option settings as
/// a mapping of NAMESPACE to the options set in it.
///
/// The settings are grouped BY namespace — the layout the reference describes —
/// so a case can declare an option beside the instance profile in the
/// launch-configuration namespace instead of restating that namespace twice,
/// which is the one thing `composed_environments::manifest` cannot express.
fn manifest(
    environment_name: &str,
    solution_stack: &str,
    tier: Option<(&str, &str)>,
    settings: &[(&str, &str, &str)],
) -> Vec<u8> {
    let mut text = String::from("AWSConfigurationTemplateVersion: 1.1.0.0\n");
    text.push_str(&format!("EnvironmentName: {environment_name}\n"));
    text.push_str(&format!("SolutionStack: {solution_stack}\n"));
    if let Some((name, kind)) = tier {
        text.push_str(&format!(
            "EnvironmentTier:\n  Name: {name}\n  Type: {kind}\n"
        ));
    }
    // Every manifest associates the instance profile with its instances, ahead
    // of the case's own settings.
    let mut grouped: Vec<(&str, Vec<(&str, &str)>)> = vec![(
        LAUNCH_CONFIGURATION,
        vec![(IAM_INSTANCE_PROFILE, INSTANCE_PROFILE)],
    )];
    for (namespace, option_name, value) in settings {
        match grouped.iter_mut().find(|(known, _)| known == namespace) {
            Some((_, options)) => options.push((option_name, value)),
            None => grouped.push((namespace, vec![(option_name, value)])),
        }
    }
    text.push_str("OptionSettings:\n");
    for (namespace, options) in grouped {
        text.push_str(&format!("  {namespace}:\n"));
        for (option_name, value) in options {
            text.push_str(&format!("    {option_name}: '{value}'\n"));
        }
    }
    text.into_bytes()
}

/// The bucket `CreateStorageLocation` publishes for this account and region: the
/// documented place a client uploads a source bundle to, and so where this
/// suite's bundles live.
async fn given_storage_location(emulator: &Emulator, case: &str) -> String {
    let created = emulator.call("CreateStorageLocation", &[]).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateStorageLocation, got {} and body {}",
        created.status, created.body
    );
    created.expect("S3Bucket", case).to_string()
}

/// Create the application the group is composed under.
async fn given_application(emulator: &Emulator, case: &str) {
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", APPLICATION)])
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplication, got {} and body {}",
        created.status, created.body
    );
}

/// Upload the component's source bundle into the storage location, create its
/// application version with pre-processing turned on, and wait until that version
/// has been successfully processed — what a client that is about to compose does,
/// since a composition reads the manifest pre-processing validated.
async fn given_version(
    emulator: &Emulator,
    bucket: &str,
    component: &Component,
    solution_stack: &str,
) {
    let case = component.case;
    let key = format!("bundles/{}.zip", component.version_label);
    let bundle = zip_of(&[
        (
            "env.yaml",
            manifest(
                component.manifest_name,
                solution_stack,
                component.tier,
                &[component.setting],
            )
            .as_slice(),
        ),
        ("app.js", b"console.log('component');\n"),
    ]);
    let stored = emulator
        .s3("PUT", &format!("/{bucket}/{key}"), &bundle)
        .await;
    assert_eq!(
        stored, 200,
        "case {case:?}: expected 200 from PutObject into s3://{bucket}/{key}, got {stored}"
    );
    let created = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", APPLICATION),
                ("VersionLabel", component.version_label),
                ("SourceBundle.S3Bucket", bucket),
                ("SourceBundle.S3Key", &key),
                ("Process", "true"),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion for {:?}, got {} and body {}",
        component.version_label, created.status, created.body
    );
    wait_for_version_status(
        emulator,
        case,
        APPLICATION,
        component.version_label,
        eb::state::VERSION_STATUS_PROCESSED,
    )
    .await;
}

/// Compose the whole group in one call.
async fn compose(emulator: &Emulator, components: &[Component]) -> Answer {
    let mut params: Vec<(String, String)> = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("GroupName".to_string(), GROUP_NAME.to_string()),
    ];
    for (index, component) in components.iter().enumerate() {
        params.push((
            format!("VersionLabels.member.{}", index + 1),
            component.version_label.to_string(),
        ));
    }
    let params: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.call("ComposeEnvironments", &params).await
}

/// The environments `DescribeEnvironments` reports for the application in its
/// DEFAULT view — the one a client that did not ask for deleted records sees.
async fn running_group(emulator: &Emulator, case: &str) -> (Vec<String>, Answer) {
    let described = emulator
        .call(
            "DescribeEnvironments",
            &[
                ("ApplicationName", APPLICATION),
                ("IncludeDeleted", "false"),
            ],
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200 from DescribeEnvironments, got {} and body {}",
        described.status, described.body
    );
    let mut names: Vec<String> = described
        .values_at("Environments/member/EnvironmentName")
        .into_iter()
        .map(str::to_string)
        .collect();
    names.sort();
    (names, described)
}

/// The one environment `DescribeEnvironments` reports for `name`, deleted records
/// included, so a composed environment can be read back whatever status it is in.
async fn described_environment(emulator: &Emulator, case: &str, name: &str) -> Answer {
    let described = emulator
        .call(
            "DescribeEnvironments",
            &[
                ("EnvironmentNames.member.1", name),
                ("IncludeDeleted", "true"),
            ],
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200 from DescribeEnvironments for {name:?}, got {} and body {}",
        described.status, described.body
    );
    let mut members = described.elements_at("Environments/member");
    assert_eq!(
        members.len(),
        1,
        "case {case:?}: expected exactly one environment named {name:?}, got {} and body {}",
        members.len(),
        described.body
    );
    members.remove(0)
}

/// The value of one option setting of the configuration the environment `name`
/// runs now.
async fn configured_value(
    emulator: &Emulator,
    case: &str,
    name: &str,
    namespace: &str,
    option_name: &str,
) -> String {
    let described = emulator
        .call(
            "DescribeConfigurationSettings",
            &[("ApplicationName", APPLICATION), ("EnvironmentName", name)],
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200 from DescribeConfigurationSettings, got {} and body {}",
        described.status, described.body
    );
    described
        .elements_at("ConfigurationSettings/member/OptionSettings/member")
        .iter()
        .find(|setting| {
            setting.value("Namespace") == Some(namespace)
                && setting.value("OptionName") == Some(option_name)
        })
        .and_then(|setting| setting.value("Value").map(str::to_string))
        .unwrap_or_default()
}

/// The group a composition creates is the group every later reader sees: each
/// environment is stored under the name its manifest gave it, reports the
/// application and the version it was composed from, its own ARN and its
/// manifest's tier, runs its manifest's configuration, and reaches `Ready`.
#[tokio::test]
async fn a_composition_materializes_one_environment_per_version_label() {
    let fixture = "the composed group";
    let components = components();
    let emulator = Emulator::with_object_storage();
    let bucket = given_storage_location(&emulator, fixture).await;
    given_application(&emulator, fixture).await;
    for (index, component) in components.iter().enumerate() {
        // The first two components share the first published stack; the third
        // names the second one, so each environment's platform is its own
        // manifest's.
        let stack = solution_stack(usize::from(index == 2));
        given_version(&emulator, &bucket, component, &stack).await;
    }

    let composed = compose(&emulator, &components).await;
    assert_eq!(
        composed.status, 200,
        "case {fixture:?}: expected 200 from ComposeEnvironments, got {} and body {}",
        composed.status, composed.body
    );

    // Whatever the call answered, the environments it composed must APPEAR: one
    // per version label, under the name the manifest and the group named.
    for component in &components {
        wait_for_status(
            &emulator,
            component.case,
            component.expected_name,
            eb::state::STATUS_READY,
        )
        .await;
    }
    let (running, answer) = running_group(&emulator, fixture).await;
    let mut expected: Vec<String> = components
        .iter()
        .map(|component| component.expected_name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        running, expected,
        "case {fixture:?}: expected exactly the composed group, got body {}",
        answer.body
    );

    for (index, component) in components.iter().enumerate() {
        let case = component.case;
        let name = component.expected_name;
        let environment = described_environment(&emulator, case, name).await;
        assert_eq!(
            environment.value("ApplicationName"),
            Some(APPLICATION),
            "case {case:?}: {name} must belong to the composed application, got body {}",
            environment.body
        );
        assert_eq!(
            environment.value("VersionLabel"),
            Some(component.version_label),
            "case {case:?}: {name} must run the version whose manifest described it, got body {}",
            environment.body
        );
        let stack = solution_stack(usize::from(index == 2));
        assert_eq!(
            environment.value("SolutionStackName"),
            Some(stack.as_str()),
            "case {case:?}: {name} must run its own manifest's solution stack, got body {}",
            environment.body
        );
        assert_eq!(
            (
                environment.value("Tier/Name"),
                environment.value("Tier/Type"),
            ),
            (
                Some(component.expected_tier.0),
                Some(component.expected_tier.1),
            ),
            "case {case:?}: {name} must report the manifest's tier, got body {}",
            environment.body
        );
        // The ARN of an environment of this application, in the region the call
        // was signed for: the form written out here rather than read off the
        // service, so the reported value is held to the published shape.
        assert_eq!(
            environment.value("EnvironmentArn"),
            Some(
                format!(
                    "arn:aws:elasticbeanstalk:{TEST_REGION}:{}:environment/{APPLICATION}/{name}",
                    account()
                )
                .as_str()
            ),
            "case {case:?}: {name} must report its own ARN, got body {}",
            environment.body
        );
        assert!(
            environment
                .value("EnvironmentId")
                .is_some_and(|id| !id.is_empty()),
            "case {case:?}: {name} must report an environment id, got body {}",
            environment.body
        );
        // And the configuration it runs is the one its manifest declared.
        let (namespace, option_name, value) = component.setting;
        let configured = configured_value(&emulator, case, name, namespace, option_name).await;
        assert_eq!(
            configured, value,
            "case {case:?}: expected {namespace}/{option_name} to be the manifest's {value:?} on \
             {name}, got {configured:?}"
        );
    }
}

/// Each environment of a composed group is terminated on its OWN: the one
/// terminated leaves the default view once its termination has completed, and
/// every other member of the group is still in it.
#[tokio::test]
async fn each_composed_environment_is_terminated_on_its_own() {
    let fixture = "the composed group, terminated one component at a time";
    let components = components();
    let emulator = Emulator::with_object_storage();
    let bucket = given_storage_location(&emulator, fixture).await;
    given_application(&emulator, fixture).await;
    for (index, component) in components.iter().enumerate() {
        let stack = solution_stack(usize::from(index == 2));
        given_version(&emulator, &bucket, component, &stack).await;
    }
    let composed = compose(&emulator, &components).await;
    assert_eq!(
        composed.status, 200,
        "case {fixture:?}: expected 200 from ComposeEnvironments, got {} and body {}",
        composed.status, composed.body
    );
    for component in &components {
        wait_for_status(
            &emulator,
            component.case,
            component.expected_name,
            eb::state::STATUS_READY,
        )
        .await;
    }

    // One row per member of the group, terminated while the ones after it are
    // still running.
    for (index, component) in components.iter().enumerate() {
        let case = component.case;
        let name = component.expected_name;
        let terminated = emulator
            .call("TerminateEnvironment", &[("EnvironmentName", name)])
            .await;
        assert_eq!(
            terminated.status, 200,
            "case {case:?}: expected 200 from TerminateEnvironment for {name:?}, got {} and \
             body {}",
            terminated.status, terminated.body
        );
        assert_eq!(
            terminated.value("EnvironmentName"),
            Some(name),
            "case {case:?}: the termination must answer the environment it terminated, got \
             body {}",
            terminated.body
        );
        wait_for_status(&emulator, case, name, eb::state::STATUS_TERMINATED).await;

        // The terminated environment has left the default view, and exactly the
        // members not yet terminated are still in it.
        let (running, answer) = running_group(&emulator, case).await;
        let mut expected: Vec<String> = components[index + 1..]
            .iter()
            .map(|component| component.expected_name.to_string())
            .collect();
        expected.sort();
        assert_eq!(
            running, expected,
            "case {case:?}: after terminating {name} the group must hold exactly \
             {expected:?}, got body {}",
            answer.body
        );
        // It is still a record of this application, reported when deleted
        // records are asked for.
        let environment = described_environment(&emulator, case, name).await;
        assert_eq!(
            environment.value("Status"),
            Some(eb::state::STATUS_TERMINATED),
            "case {case:?}: {name} must still be reported as terminated, got body {}",
            environment.body
        );
    }
}
