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
//! launch like any other: it starts `Launching` and reaches `Ready`. A label
//! that describes no environment composes none.
//!
//! The table spans what that rule generalises over: how many labels one call
//! names, whether a manifest's name is completed by a group name, which tier it
//! declares and whether it declares one at all, which published solution stack
//! it names, the configuration and the links it carries, a second composition of
//! the same group (the UPDATE half of the rule), and each way a label can
//! describe no environment at all.
//!
//! The harness (the router, the form encoder, the XML reader, the object-storage
//! upload and the poll for a settled status) is the one `lifecycle.rs` owns in
//! this same test binary; the archive writer is the one
//! `application_version_processing.rs` owns.

use crate::application_version_processing::zip_of;
use crate::harness::{wait_for_status, Answer, Emulator, SOLUTION_STACK};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every group below is composed under.
const APPLICATION: &str = "composed-app";

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
    if !settings.is_empty() {
        text.push_str("OptionSettings:\n");
        for (namespace, option_name, value) in settings {
            text.push_str(&format!(
                "  {namespace}:\n    {option_name}: '{value}'\n"
            ));
        }
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

/// Upload a source bundle carrying `files` and create the application version
/// `version_label` from it, the way a client that is about to compose does.
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

/// A manifest past the bounds this emulator parses caller content within
/// describes no environment either: both bounds are taken BEFORE a tree is
/// built from the text, so the request is refused with the same sentence and
/// the emulator keeps serving. The rows differ along which bound the manifest
/// is past — its nesting, and the alias expansion a loader would perform.
#[tokio::test]
async fn a_manifest_past_the_parse_bounds_composes_nothing() {
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
    for (case, manifest) in [
        ("a manifest nesting past the bound", deep),
        ("a manifest whose aliases would expand exponentially", aliases),
    ] {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        given_version(&emulator, case, "v1", &[("env.yaml", manifest.as_bytes())]).await;

        let refused = compose(&emulator, Some(GROUP_NAME), &["v1"]).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 from ComposeEnvironments, got {} and body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_message(),
            "No valid application version found.",
            "case {case:?}: expected the service's sentence for a label that composes nothing, \
             got {:?}",
            refused.error_message()
        );
        // And the emulator is still serving: the next call is answered from the
        // same process.
        let (described, answer) = described_environments(&emulator, case).await;
        assert!(
            described.is_empty(),
            "case {case:?}: nothing may be composed from a manifest nothing parsed, got body {}",
            answer.body
        );
    }
}

/// A version created with NO source bundle carries no manifest either — "if
/// neither SourceBundle nor SourceBuildInformation are provided, Elastic
/// Beanstalk uses a sample application", which is no archive this emulator holds
/// — so it describes no environment and composes none.
#[tokio::test]
async fn a_version_with_no_source_bundle_composes_nothing() {
    let case = "a version created without a source bundle";
    let emulator = Emulator::with_object_storage();
    given_application(&emulator, case).await;
    let created = emulator
        .call(
            "CreateApplicationVersion",
            &[
                ("ApplicationName", APPLICATION),
                ("VersionLabel", "sampled"),
            ],
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200 from CreateApplicationVersion, got {} and body {}",
        created.status, created.body
    );

    let refused = compose(&emulator, Some(GROUP_NAME), &["sampled"]).await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400 from ComposeEnvironments, got {} and body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_message(),
        "No valid application version found.",
        "case {case:?}: expected the service's sentence for a label that composes nothing, got \
         {:?}",
        refused.error_message()
    );
}
