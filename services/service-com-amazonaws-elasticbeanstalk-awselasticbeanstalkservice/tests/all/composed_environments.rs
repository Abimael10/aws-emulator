// SPDX-License-Identifier: CC0-1.0

//! `ComposeEnvironments` composes the environments the ENVIRONMENT MANIFESTS in
//! the named versions' source bundles describe.
//!
//! The rule under test: this operation "creates or updates a group of
//! environments that each run a separate component of a single application" and
//! "takes a list of version labels that specify application source bundles for
//! each of the environments to create or update", where "the name of each
//! environment and other required information must be included in the source
//! bundles in an environment manifest named `env.yaml`". So one environment is
//! composed per version label, and everything about it — its name (completed by
//! `GroupName` when the manifest's name ends in `+`), its solution stack, its
//! tier, its configuration and its links to the other environments of the group
//! — is read out of that version's own manifest. Each composed environment is a
//! launch like any other: it starts `Launching` and reaches `Ready`, provided
//! its manifest associates an IAM instance profile with the environment's
//! instances — which no launch can be provisioned without, the rule
//! `environment_launch_authorization.rs` owns — so every manifest below
//! supplies one. A label that describes no environment composes none.
//!
//! A manifest is what a version's PRE-PROCESSING reads and validates, so only a
//! version that "was successfully pre-processed and validated" can be composed
//! from: a version of the application in any other processing status is refused
//! with that status, and a request none of whose labels names a version of the
//! application at all is refused with the service's fixed sentence for that
//! class. Both refusals are the subject of the suites at the end of this file.
//!
//! The table spans what that rule generalises over: how many labels one call
//! names, whether a manifest's name is completed by a group name, which tier it
//! declares and whether it declares one at all, which published solution stack
//! it names, the configuration and the links it carries, a second composition of
//! the same group (the UPDATE half of the rule), each way a label can describe no
//! environment at all, and each processing status a version can be in.
//!
//! The harness (the router, the form encoder, the XML reader, the object-storage
//! upload and the poll for a settled status) is the one `lifecycle.rs` owns in
//! this same test binary; the archive writer is the one
//! `application_version_processing.rs` owns.

use crate::application_version_processing::zip_of;
use crate::harness::{wait_for_status, wait_for_version_status, Answer, Emulator, SOLUTION_STACK};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every group below is composed under.
const APPLICATION: &str = "composed-app";

/// A SECOND application, so a composition can name a version label that exists
/// but belongs to another application than the one the request names.
const OTHER_APPLICATION: &str = "composed-other-app";

/// The version label that second application carries.
const FOREIGN_LABEL: &str = "v-foreign";

/// The version label the refusal suites create under [`APPLICATION`].
const VERSION_LABEL: &str = "v1";

/// The bucket a version's source bundle is uploaded into: one per version, so
/// each upload creates the bucket it writes into and no suite depends on the
/// order its uploads ran in.
fn bundle_bucket(version_label: &str) -> String {
    format!("eb-composed-bundles-{version_label}")
}

/// The key every source bundle is uploaded under in its own bucket.
const BUNDLE_KEY: &str = "bundles/bundle.zip";

/// The group name a composition passes, where it passes one.
const GROUP_NAME: &str = "dev";

/// The `EnvironmentStatus` a launch starts in and the one it reaches, taken from
/// the service's own constants so an expectation cannot drift from what the
/// service reports.
const LAUNCHING: &str = eb::state::STATUS_LAUNCHING;
/// See [`LAUNCHING`].
const READY: &str = eb::state::STATUS_READY;

/// An option setting a manifest declares, in a namespace the published option
/// catalogue carries: the environment-properties namespace
/// (`eb::state::NAMESPACE_APPLICATION_ENVIRONMENT`) and a property name of the
/// caller's own choosing, which is what that namespace holds.
const PROPERTY_NAME: &str = "GREETING";
/// See [`PROPERTY_NAME`].
const PROPERTY_VALUE: &str = "hello from the manifest";
/// The value the SECOND composition of the same group declares instead, so the
/// update is visible in the configuration the environment then runs.
const UPDATED_PROPERTY_VALUE: &str = "hello again";

/// The `EnvironmentTier$Type` of the worker tier, from that member's own
/// documentation ("For Worker tier — `SQS/HTTP`"): the type a manifest declares
/// beside `EnvironmentTier$Name` `Worker`.
const WORKER_TIER_TYPE: &str = "SQS/HTTP";

/// The namespace and the option that name the IAM instance profile an
/// environment's instances are given, as Elastic Beanstalk's published
/// configuration-option reference spells them
/// (`aws:autoscaling:launchconfiguration` / `IamInstanceProfile`), and the
/// profile every manifest here associates with its environment.
///
/// A composed environment is configured by its manifest alone, so a manifest
/// that associates no instance profile composes a launch that cannot be
/// provisioned. That rule is `environment_launch_authorization.rs`'s subject;
/// every manifest below supplies a profile, so what decides each case here is
/// the rule the case is about.
const LAUNCH_CONFIGURATION: &str = "aws:autoscaling:launchconfiguration";
/// See [`LAUNCH_CONFIGURATION`].
const IAM_INSTANCE_PROFILE: &str = "IamInstanceProfile";
/// See [`LAUNCH_CONFIGURATION`].
const INSTANCE_PROFILE: &str = "eb-ec2-role";

/// The second published solution stack, so one group's two manifests name two
/// different platforms and each environment is asserted to have taken its OWN
/// manifest's.
fn other_solution_stack() -> String {
    eb::state::SOLUTION_STACKS
        .get(1)
        .expect("the service publishes more than one solution stack")
        .name()
}

/// An environment manifest, as Elastic Beanstalk's environment-manifest
/// reference lays `env.yaml` out: the configuration-template version, the
/// environment's name, the solution stack to use, the tier, the option settings
/// by namespace and the links to the other environments of the group.
///
/// Every manifest associates [`INSTANCE_PROFILE`] with its environment's
/// instances, ahead of the case's own settings, since a launch with no instance
/// profile is one this service cannot provision.
///
/// Built here rather than held as a constant because the solution stack a
/// manifest names is read off the service's own published catalogue.
fn manifest(
    environment_name: &str,
    solution_stack: Option<&str>,
    tier: Option<(&str, &str)>,
    settings: &[(&str, &str, &str)],
    links: &[(&str, &str)],
) -> Vec<u8> {
    let mut text = String::from("AWSConfigurationTemplateVersion: 1.1.0.0\n");
    text.push_str(&format!("EnvironmentName: {environment_name}\n"));
    if let Some(solution_stack) = solution_stack {
        text.push_str(&format!("SolutionStack: {solution_stack}\n"));
    }
    if let Some((name, kind)) = tier {
        text.push_str(&format!(
            "EnvironmentTier:\n  Name: {name}\n  Type: {kind}\n"
        ));
    }
    text.push_str("OptionSettings:\n");
    text.push_str(&format!(
        "  {LAUNCH_CONFIGURATION}:\n    {IAM_INSTANCE_PROFILE}: '{INSTANCE_PROFILE}'\n"
    ));
    for (namespace, option_name, value) in settings {
        text.push_str(&format!(
            "  {namespace}:\n    {option_name}: '{value}'\n"
        ));
    }
    if !links.is_empty() {
        text.push_str("EnvironmentLinks:\n");
        for (link_name, target) in links {
            text.push_str(&format!("  \"{link_name}\": \"{target}\"\n"));
        }
    }
    text.into_bytes()
}

/// Create the application every version below is created under.
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

/// Create [`OTHER_APPLICATION`] with one version `version_label` under it: a
/// label that names a real application version, but not one of the composed
/// application's.
async fn given_foreign_version(emulator: &Emulator, case: &str, version_label: &str) {
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", OTHER_APPLICATION)])
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplication for {OTHER_APPLICATION}, got {} and \
         body {}",
        created.status, created.body
    );
    let version = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", OTHER_APPLICATION),
                ("VersionLabel", version_label),
            ],
        )
        .await;
    assert_eq!(
        version.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion for {version_label:?}, got {} \
         and body {}",
        version.status, version.body
    );
}

/// Create a version of the composed application that nothing pre-processed: no
/// source bundle and no `Process`, so it sits at `Unprocessed`.
async fn given_sampled_version(emulator: &Emulator, case: &str, version_label: &str) {
    let created = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", APPLICATION),
                ("VersionLabel", version_label),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion for {version_label:?}, got {} \
         and body {}",
        created.status, created.body
    );
}

/// Upload a source bundle carrying `files`, create the application version
/// `version_label` from it with pre-processing turned on, and wait until the
/// version has been successfully processed — the way a client that is about to
/// compose does, since a composition reads the manifest that pre-processing
/// validated.
async fn given_version(
    emulator: &Emulator,
    case: &str,
    version_label: &str,
    files: &[(&str, &[u8])],
) {
    let bucket = bundle_bucket(version_label);
    emulator
        .put_object(&bucket, BUNDLE_KEY, &zip_of(files))
        .await;
    let created = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", APPLICATION),
                ("VersionLabel", version_label),
                ("SourceBundle.S3Bucket", &bucket),
                ("SourceBundle.S3Key", BUNDLE_KEY),
                ("Process", "true"),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion for {version_label:?}, got {} \
         and body {}",
        created.status, created.body
    );
    wait_for_version_status(
        emulator,
        case,
        APPLICATION,
        version_label,
        eb::state::VERSION_STATUS_PROCESSED,
    )
    .await;
}

/// Compose the group the `version_labels` describe, with `group_name` when the
/// request carries one.
async fn compose(emulator: &Emulator, group_name: Option<&str>, version_labels: &[&str]) -> Answer {
    let mut params: Vec<(String, String)> = vec![(
        "ApplicationName".to_string(),
        APPLICATION.to_string(),
    )];
    if let Some(group_name) = group_name {
        params.push(("GroupName".to_string(), group_name.to_string()));
    }
    for (index, version_label) in version_labels.iter().enumerate() {
        params.push((
            format!("VersionLabels.member.{}", index + 1),
            (*version_label).to_string(),
        ));
    }
    let params: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.call("ComposeEnvironments", &params).await
}

/// The environments `DescribeEnvironments` reports for this application, as the
/// elements they were read from, so a round trip asserts on the STORED record
/// rather than on what the composition answered.
async fn described_environments(emulator: &Emulator, case: &str) -> (Vec<Answer>, Answer) {
    let described = emulator
        .call(
            "DescribeEnvironments",
            &[("ApplicationName", APPLICATION)],
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200 from DescribeEnvironments, got {} and body {}",
        described.status, described.body
    );
    let environments = described.elements_at("Environments/member");
    (environments, described)
}

/// The value of one option setting of the configuration the environment
/// `environment_name` runs now.
async fn configured_value(
    emulator: &Emulator,
    case: &str,
    environment_name: &str,
    namespace: &str,
    option_name: &str,
) -> String {
    let described = emulator
        .call(
            "DescribeConfigurationSettings",
            &[
                ("ApplicationName", APPLICATION),
                ("EnvironmentName", environment_name),
            ],
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

/// One call composes one environment per version label, each carrying what its
/// OWN manifest declared, each starting `Launching` and reaching `Ready`.
#[tokio::test]
async fn compose_environments_launches_one_environment_per_manifest() {
    let case = "a two-component group composed from two manifests";
    let front_stack = SOLUTION_STACK.as_str().to_string();
    let worker_stack = other_solution_stack();
    let emulator = Emulator::with_object_storage();
    given_application(&emulator, case).await;
    given_version(
        &emulator,
        case,
        "front-1",
        &[
            (
                "env.yaml",
                &manifest(
                    "c-front+",
                    Some(&front_stack),
                    // No `EnvironmentTier` member at all, so the composed
                    // environment reports the tier the model documents as the
                    // default.
                    None,
                    &[(
                        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                        PROPERTY_NAME,
                        PROPERTY_VALUE,
                    )],
                    &[("WORKERQUEUE", "c-worker+")],
                ),
            ),
            ("app.js", b"console.log('front');\n"),
        ],
    )
    .await;
    given_version(
        &emulator,
        case,
        "worker-1",
        &[(
            "env.yaml",
            &manifest(
                "c-worker+",
                Some(&worker_stack),
                Some((eb::state::TIER_NAME_WORKER, WORKER_TIER_TYPE)),
                &[],
                &[],
            ),
        )],
    )
    .await;

    let composed = compose(&emulator, Some(GROUP_NAME), &["front-1", "worker-1"]).await;
    assert_eq!(
        composed.status, 200,
        "case {case:?}: expected 200 from ComposeEnvironments, got {} and body {}",
        composed.status, composed.body
    );
    let answered = composed.elements_at("Environments/member");
    assert_eq!(
        answered.len(),
        2,
        "case {case:?}: expected one environment per version label, got {} and body {}",
        answered.len(),
        composed.body
    );
    // In the order the labels were named, each environment named by its own
    // manifest with the group name completing it.
    let expected: [(&str, &str, &str, &str, &str); 2] = [
        (
            "c-front-dev",
            "front-1",
            front_stack.as_str(),
            eb::state::TIER_NAME_WEB_SERVER,
            eb::state::TIER_TYPE_STANDARD,
        ),
        (
            "c-worker-dev",
            "worker-1",
            worker_stack.as_str(),
            eb::state::TIER_NAME_WORKER,
            WORKER_TIER_TYPE,
        ),
    ];
    for (answered, (name, version_label, stack, tier_name, tier_type)) in
        answered.iter().zip(expected)
    {
        assert_eq!(
            answered.value("EnvironmentName"),
            Some(name),
            "case {case:?}: expected the manifest's name completed by the group, got body {}",
            composed.body
        );
        assert_eq!(
            answered.value("VersionLabel"),
            Some(version_label),
            "case {case:?}: {name} must run the version whose manifest described it, got body {}",
            composed.body
        );
        assert_eq!(
            answered.value("SolutionStackName"),
            Some(stack),
            "case {case:?}: {name} must run the manifest's own solution stack, got body {}",
            composed.body
        );
        assert_eq!(
            answered.value("Status"),
            Some(LAUNCHING),
            "case {case:?}: {name} must start {LAUNCHING}, got body {}",
            composed.body
        );
        assert_eq!(
            (
                answered.value("Tier/Name"),
                answered.value("Tier/Type"),
                answered.value("Tier/Version"),
            ),
            (
                Some(tier_name),
                Some(tier_type),
                Some(eb::state::DEFAULT_TIER_VERSION)
            ),
            "case {case:?}: {name} must report the manifest's tier, got body {}",
            composed.body
        );
    }
    // The link the front manifest declared, with its target completed by the
    // same group name the environments themselves were.
    let links = answered[0].elements_at("EnvironmentLinks/member");
    assert_eq!(
        links
            .iter()
            .map(|link| (link.value("LinkName"), link.value("EnvironmentName")))
            .collect::<Vec<_>>(),
        vec![(Some("WORKERQUEUE"), Some("c-worker-dev"))],
        "case {case:?}: expected the manifest's link to the group's worker, got body {}",
        composed.body
    );
    assert!(
        answered[1]
            .elements_at("EnvironmentLinks/member")
            .is_empty(),
        "case {case:?}: a manifest declaring no links composes an environment with none, got \
         body {}",
        composed.body
    );

    // Both composed environments are launches, so both reach `Ready`.
    for name in ["c-front-dev", "c-worker-dev"] {
        wait_for_status(&emulator, case, name, READY).await;
    }

    // And the group is the STORED state every later reader sees.
    // `DescribeEnvironments` reports them newest first, so the second manifest's
    // environment heads the list.
    let (described, answer) = described_environments(&emulator, case).await;
    assert_eq!(
        described
            .iter()
            .map(|environment| environment.value("EnvironmentName").unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["c-worker-dev", "c-front-dev"],
        "case {case:?}: expected exactly the composed group, got body {}",
        answer.body
    );
    assert_eq!(
        described[1]
            .elements_at("EnvironmentLinks/member")
            .iter()
            .map(|link| link.value("EnvironmentName"))
            .collect::<Vec<_>>(),
        vec![Some("c-worker-dev")],
        "case {case:?}: the stored environment must carry the manifest's link, got body {}",
        answer.body
    );
    // The configuration the manifest declared is the configuration the composed
    // environment runs.
    let configured = configured_value(
        &emulator,
        case,
        "c-front-dev",
        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
        PROPERTY_NAME,
    )
    .await;
    assert_eq!(
        configured, PROPERTY_VALUE,
        "case {case:?}: expected the manifest's option setting on the composed environment, got \
         {configured:?}"
    );
}

/// One row per way a manifest name and a request's group name combine.
///
/// "Specify a group name only if the environment name defined in each target
/// environment's manifest ends with a + (plus) character": the placeholder is
/// what the group name completes, so a name without it is the environment's name
/// as written whatever the request carries, and a placeholder with no group name
/// to complete it leaves the name it was written with.
#[tokio::test]
async fn a_manifest_name_is_completed_only_where_it_asks_to_be() {
    let rows: [(&str, &str, Option<&str>, &str); 4] = [
        (
            "a placeholder name and a group name",
            "c-api+",
            Some(GROUP_NAME),
            "c-api-dev",
        ),
        (
            "a placeholder name and no group name",
            "c-api+",
            None,
            "c-api",
        ),
        (
            "a plain name and a group name",
            "c-api-fixed",
            Some(GROUP_NAME),
            "c-api-fixed",
        ),
        ("a plain name and no group name", "c-api-fixed", None, "c-api-fixed"),
    ];
    for (case, manifest_name, group_name, expected) in rows {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        given_version(
            &emulator,
            case,
            "v1",
            &[(
                "env.yaml",
                &manifest(manifest_name, Some(SOLUTION_STACK.as_str()), None, &[], &[]),
            )],
        )
        .await;

        let composed = compose(&emulator, group_name, &["v1"]).await;
        assert_eq!(
            composed.status, 200,
            "case {case:?}: expected 200 from ComposeEnvironments, got {} and body {}",
            composed.status, composed.body
        );
        assert_eq!(
            composed.values_at("Environments/member/EnvironmentName"),
            vec![expected],
            "case {case:?}: expected the environment to be named {expected:?}, got body {}",
            composed.body
        );
        let (described, answer) = described_environments(&emulator, case).await;
        assert_eq!(
            described
                .iter()
                .map(|environment| environment.value("EnvironmentName").unwrap_or_default())
                .collect::<Vec<_>>(),
            vec![expected],
            "case {case:?}: the stored environment must carry that name, got body {}",
            answer.body
        );
    }
}

/// Composing the same group again does not duplicate it: "create OR UPDATE a
/// group of environments" — the environment the group already runs under that
/// name is the one the new version is deployed to, so it keeps its id and
/// reports `Updating` until the clock settles it back to `Ready`.
#[tokio::test]
async fn composing_a_group_again_deploys_the_new_version_to_it() {
    let case = "the same group composed twice";
    let emulator = Emulator::with_object_storage();
    given_application(&emulator, case).await;
    let stack = SOLUTION_STACK.as_str().to_string();
    given_version(
        &emulator,
        case,
        "v1",
        &[(
            "env.yaml",
            &manifest(
                "c-api+",
                Some(&stack),
                None,
                &[(
                    eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                    PROPERTY_NAME,
                    PROPERTY_VALUE,
                )],
                &[],
            ),
        )],
    )
    .await;
    given_version(
        &emulator,
        case,
        "v2",
        &[(
            "env.yaml",
            &manifest(
                "c-api+",
                Some(&stack),
                None,
                &[(
                    eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
                    PROPERTY_NAME,
                    UPDATED_PROPERTY_VALUE,
                )],
                &[],
            ),
        )],
    )
    .await;

    let first = compose(&emulator, Some(GROUP_NAME), &["v1"]).await;
    let environment_id = first
        .value("Environments/member/EnvironmentId")
        .unwrap_or_default()
        .to_string();
    assert!(
        !environment_id.is_empty(),
        "case {case:?}: the composition must report the environment's id, got body {}",
        first.body
    );
    wait_for_status(&emulator, case, "c-api-dev", READY).await;

    let again = compose(&emulator, Some(GROUP_NAME), &["v2"]).await;
    assert_eq!(
        again.status, 200,
        "case {case:?}: expected 200 from the second ComposeEnvironments, got {} and body {}",
        again.status, again.body
    );
    assert_eq!(
        again.value("Environments/member/EnvironmentId"),
        Some(environment_id.as_str()),
        "case {case:?}: the group's environment must be UPDATED, not replaced, got body {}",
        again.body
    );
    assert_eq!(
        again.value("Environments/member/VersionLabel"),
        Some("v2"),
        "case {case:?}: the composition must deploy the newly named version, got body {}",
        again.body
    );
    assert_eq!(
        again.value("Environments/member/Status"),
        Some(eb::state::STATUS_UPDATING),
        "case {case:?}: an updated environment reports {} while the deployment runs, got body {}",
        eb::state::STATUS_UPDATING,
        again.body
    );

    wait_for_status(&emulator, case, "c-api-dev", READY).await;
    let (described, answer) = described_environments(&emulator, case).await;
    assert_eq!(
        described.len(),
        1,
        "case {case:?}: the group must still hold one environment, got body {}",
        answer.body
    );
    assert_eq!(
        described[0].value("VersionLabel"),
        Some("v2"),
        "case {case:?}: the settled environment must run the deployed version, got body {}",
        answer.body
    );
    let configured = configured_value(
        &emulator,
        case,
        "c-api-dev",
        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
        PROPERTY_NAME,
    )
    .await;
    assert_eq!(
        configured, UPDATED_PROPERTY_VALUE,
        "case {case:?}: the deployed configuration must be the new manifest's, got {configured:?}"
    );
}

/// One way a version label can describe no environment: what its bundle
/// carries, if the version exists at all, and the labels the composition names.
struct Undescribed {
    case: &'static str,
    /// The files of the version `v1`'s bundle, or `None` for a row whose
    /// composition names a label no version exists under.
    files: Option<&'static [(&'static str, &'static [u8])]>,
    version_labels: &'static [&'static str],
}

/// A version label that describes no environment composes nothing at all: the
/// request is refused with the service's own sentence for that class, and the
/// application is left with no environment — not even for the labels that did
/// describe one.
#[tokio::test]
async fn a_version_label_that_describes_no_environment_composes_nothing() {
    let good = manifest("c-ok+", Some(SOLUTION_STACK.as_str()), None, &[], &[]);
    // One row per way a label can fail to describe an environment.
    let rows: [Undescribed; 5] = [
        Undescribed {
            case: "a label this application has no version for",
            files: None,
            version_labels: &["absent"],
        },
        Undescribed {
            case: "a version whose bundle carries no manifest",
            files: Some(&[("app.js", b"console.log('no manifest');\n")]),
            version_labels: &["v1"],
        },
        Undescribed {
            case: "a manifest that names no environment",
            files: Some(&[("env.yaml", b"AWSConfigurationTemplateVersion: 1.1.0.0\n")]),
            version_labels: &["v1"],
        },
        Undescribed {
            case: "a manifest that is not a YAML mapping",
            files: Some(&[("env.yaml", b"- EnvironmentName: c-list+\n")]),
            version_labels: &["v1"],
        },
        Undescribed {
            case: "a second label that describes nothing beside one that does",
            files: Some(&[("app.js", b"console.log('no manifest');\n")]),
            version_labels: &["good", "v1"],
        },
    ];
    for Undescribed {
        case,
        files,
        version_labels,
    } in rows
    {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        given_version(&emulator, case, "good", &[("env.yaml", &good)]).await;
        if let Some(files) = files {
            given_version(&emulator, case, "v1", files).await;
        }

        let refused = compose(&emulator, Some(GROUP_NAME), version_labels).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 from ComposeEnvironments, got {} and body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "MissingParameter",
            "case {case:?}: expected MissingParameter, got body {}",
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            "No valid application version found.",
            "case {case:?}: expected the service's sentence for a label that composes nothing, \
             got {:?}",
            refused.error_message()
        );
        let (described, answer) = described_environments(&emulator, case).await;
        assert!(
            described.is_empty(),
            "case {case:?}: a refused composition must leave no environment behind, got body {}",
            answer.body
        );
    }
}

/// One version a composition names that has NOT been successfully
/// pre-processed.
struct Unready {
    case: &'static str,
    /// The `env.yaml` the version's source bundle carries, or `None` for a
    /// version created with no source bundle at all — "if neither SourceBundle
    /// nor SourceBuildInformation are provided, Elastic Beanstalk uses a sample
    /// application", which is no archive this emulator holds.
    manifest: Option<Vec<u8>>,
    /// Whether the create asks for the pre-processing of that bundle.
    process: bool,
    /// The modelled `ApplicationVersionStatus` the version is in when the
    /// composition names it, taken from the service's own constants so the wait
    /// cannot drift from what the service reports.
    status: &'static str,
    /// The same status as the service puts an application version's status on
    /// the wire, which is the text the refusal must carry.
    reported: &'static str,
}

/// A version of the application that has not been successfully pre-processed
/// composes nothing: pre-processing is what reads and validates the `env.yaml`
/// manifest a composition is described by, so the request is refused with the
/// service's configuration refusal naming the label the request supplied and
/// the status the version is in — a transient state a client waits out, told
/// apart from a label that names no version of the application at all — and no
/// environment is created.
///
/// The rows span every status that is not the successfully processed one and
/// each way a version gets into it: a create that asked for no pre-processing at
/// all, a bundle past the bounds this emulator parses caller content within (so
/// nothing validated it, and the emulator keeps serving), a bundle that failed
/// validation, and pre-processing the clock still owes.
#[tokio::test]
async fn a_version_that_was_not_successfully_processed_composes_nothing() {
    let good = manifest("c-ready+", Some(SOLUTION_STACK.as_str()), None, &[], &[]);
    // One level past the nesting bound, and a kilobyte of alias amplification:
    // both bounds are taken BEFORE a tree is built from the text, so neither
    // file is ever loaded and nothing pre-processed the version.
    let deep = format!(
        "EnvironmentName: c-deep+\nOptionSettings: {}",
        "[".repeat(eb::state::MAX_VALIDATED_NESTING + 1)
    );
    let mut aliases = String::from("EnvironmentName: c-alias+\na0: &a0 [x, x]\n");
    for level in 1..24 {
        aliases.push_str(&format!(
            "a{level}: &a{level} [*a{}, *a{}]\n",
            level - 1,
            level - 1
        ));
    }
    // A manifest that is text but no YAML document: a flow sequence that is
    // never closed, so the pre-processing of it FAILS.
    let broken = b"EnvironmentName: c-broken+\nOptionSettings: [\n".to_vec();
    let rows = vec![
        Unready {
            case: "a version created with no source bundle",
            manifest: None,
            process: false,
            status: eb::state::VERSION_STATUS_UNPROCESSED,
            reported: "UNPROCESSED",
        },
        Unready {
            case: "a version whose create asked for no pre-processing",
            manifest: Some(good.clone()),
            process: false,
            status: eb::state::VERSION_STATUS_UNPROCESSED,
            reported: "UNPROCESSED",
        },
        Unready {
            case: "a version whose manifest nests past the bound this emulator parses",
            manifest: Some(deep.into_bytes()),
            process: true,
            status: eb::state::VERSION_STATUS_UNPROCESSED,
            reported: "UNPROCESSED",
        },
        Unready {
            case: "a version whose manifest would expand its aliases exponentially",
            manifest: Some(aliases.into_bytes()),
            process: true,
            status: eb::state::VERSION_STATUS_UNPROCESSED,
            reported: "UNPROCESSED",
        },
        Unready {
            case: "a version whose manifest failed validation",
            manifest: Some(broken),
            process: true,
            status: eb::state::VERSION_STATUS_FAILED,
            reported: "FAILED",
        },
        Unready {
            case: "a version whose pre-processing is still owed",
            manifest: Some(good.clone()),
            process: true,
            status: eb::state::VERSION_STATUS_PROCESSING,
            reported: "PROCESSING",
        },
    ];

    for Unready {
        case,
        manifest,
        process,
        status,
        reported,
    } in rows
    {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        let mut params: Vec<(String, String)> = vec![
            ("ApplicationName".to_string(), APPLICATION.to_string()),
            ("VersionLabel".to_string(), VERSION_LABEL.to_string()),
        ];
        if let Some(text) = &manifest {
            let bucket = bundle_bucket(VERSION_LABEL);
            emulator
                .put_object(
                    &bucket,
                    BUNDLE_KEY,
                    &zip_of(&[("env.yaml", text.as_slice())]),
                )
                .await;
            params.push(("SourceBundle.S3Bucket".to_string(), bucket));
            params.push(("SourceBundle.S3Key".to_string(), BUNDLE_KEY.to_string()));
        }
        if process {
            params.push(("Process".to_string(), "true".to_string()));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let created = emulator.call("CreateApplicationVersion", &borrowed).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
            created.status, created.body
        );
        if status == eb::state::VERSION_STATUS_PROCESSING {
            // The transient status is the one the create itself reports — the
            // version settles one status-transition span later — so the
            // composition that must observe it follows straight away, with
            // nothing between the two calls.
            assert_eq!(
                created.value("ApplicationVersion/Status"),
                Some(reported),
                "case {case:?}: expected the create to report {reported}, got body {}",
                created.body
            );
        } else {
            wait_for_version_status(&emulator, case, APPLICATION, VERSION_LABEL, status).await;
        }

        let refused = compose(&emulator, Some(GROUP_NAME), &[VERSION_LABEL]).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 from ComposeEnvironments, got {} and body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ConfigurationValidationException",
            "case {case:?}: expected the configuration refusal, got body {}",
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            format!(
                "ApplicationVersion {VERSION_LABEL} has not been successfully processed. Current \
                 status: {reported}"
            ),
            "case {case:?}: expected the refusal to name the label and the version's status, got \
             {:?}",
            refused.error_message()
        );
        // And the emulator is still serving: the next call is answered from the
        // same process, and the refused composition left nothing behind.
        let (described, answer) = described_environments(&emulator, case).await;
        assert!(
            described.is_empty(),
            "case {case:?}: a refused composition must leave no environment behind, got body {}",
            answer.body
        );
    }
}

/// What a composition answers for the labels one row of
/// [`version_labels_are_scoped_to_the_named_applications_versions`] names.
enum Composed {
    /// Refused with this error code and message, composing nothing.
    Refused {
        code: &'static str,
        message: String,
    },
    /// Composed exactly these environments, in this order.
    Environments(&'static [&'static str]),
}

/// One request's version labels, and what the composition must answer for them.
struct Scoped {
    case: &'static str,
    version_labels: Vec<&'static str>,
    expected: Composed,
}

/// "A list of version labels, specifying one or more application source bundles
/// that belong to the TARGET APPLICATION": the labels are scoped to the versions
/// of the application the request names, so a label that names no version of it
/// — one of another application, or one nothing carries — is not a version this
/// call composes from. A request left with no version by that scoping resolves
/// no application version at all and is refused with the service's fixed
/// sentence for that class; a request left with one composes it, and a request
/// left with one that has not been successfully pre-processed is refused with
/// that version's label and status instead.
///
/// The rows span where each label lives, how many a request carries, and which
/// of them is the one the scoping keeps.
#[tokio::test]
async fn version_labels_are_scoped_to_the_named_applications_versions() {
    let ready = manifest("c-api+", Some(SOLUTION_STACK.as_str()), None, &[], &[]);
    let no_valid_version = Composed::Refused {
        code: "MissingParameter",
        message: "No valid application version found.".to_string(),
    };
    let rows = vec![
        Scoped {
            case: "a label of another application's version alone",
            version_labels: vec![FOREIGN_LABEL],
            expected: no_valid_version,
        },
        Scoped {
            case: "a label no version carries alone",
            version_labels: vec!["v-never"],
            expected: Composed::Refused {
                code: "MissingParameter",
                message: "No valid application version found.".to_string(),
            },
        },
        Scoped {
            case: "a foreign label beside a processed version of the application",
            version_labels: vec![FOREIGN_LABEL, "v-ready"],
            expected: Composed::Environments(&["c-api-dev"]),
        },
        Scoped {
            case: "a label no version carries beside a processed version of the application",
            version_labels: vec!["v-never", "v-ready"],
            expected: Composed::Environments(&["c-api-dev"]),
        },
        Scoped {
            case: "a foreign label beside an unprocessed version of the application",
            version_labels: vec![FOREIGN_LABEL, "v-raw"],
            expected: Composed::Refused {
                code: "ConfigurationValidationException",
                message: "ApplicationVersion v-raw has not been successfully processed. Current \
                          status: UNPROCESSED"
                    .to_string(),
            },
        },
    ];

    for Scoped {
        case,
        version_labels,
        expected,
    } in rows
    {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        given_foreign_version(&emulator, case, FOREIGN_LABEL).await;
        given_version(&emulator, case, "v-ready", &[("env.yaml", &ready)]).await;
        given_sampled_version(&emulator, case, "v-raw").await;

        let answer = compose(&emulator, Some(GROUP_NAME), &version_labels).await;
        match expected {
            Composed::Refused { code, message } => {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected HTTP 400 from ComposeEnvironments, got {} and body {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code(),
                    code,
                    "case {case:?}: expected {code}, got body {}",
                    answer.body
                );
                assert_eq!(
                    answer.error_message(),
                    message,
                    "case {case:?}: expected the refusal {message:?}, got {:?}",
                    answer.error_message()
                );
                let (described, body) = described_environments(&emulator, case).await;
                assert!(
                    described.is_empty(),
                    "case {case:?}: a refused composition must leave no environment behind, got \
                     body {}",
                    body.body
                );
            }
            Composed::Environments(names) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected 200 from ComposeEnvironments, got {} and body {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.values_at("Environments/member/EnvironmentName"),
                    names.to_vec(),
                    "case {case:?}: expected the scoped version's environment, got body {}",
                    answer.body
                );
                let (described, body) = described_environments(&emulator, case).await;
                assert_eq!(
                    described
                        .iter()
                        .map(|environment| environment.value("EnvironmentName").unwrap_or_default())
                        .collect::<Vec<_>>(),
                    names.to_vec(),
                    "case {case:?}: the stored group must hold exactly that environment, got body \
                     {}",
                    body.body
                );
            }
        }
    }
}
