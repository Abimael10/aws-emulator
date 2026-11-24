// SPDX-License-Identifier: CC0-1.0

//! The ENVIRONMENT LINKS a composition creates between the members of the group
//! it composes.
//!
//! The rule under test: `ComposeEnvironments` "creates or updates a group of
//! environments that each run a separate component of a single application" from
//! version labels whose source bundles each carry an `env.yaml`, and such a
//! bundle "optionally can specify environment links to create". A link is one of
//! "a list of links to other environments in the same group" and "provides
//! connection information in system properties that can be used to connect to
//! another environment in the same group", so its target is named the way the
//! manifests name environments: a target ending in the `+` that "specify a group
//! name only if the environment name defined in each target environment's
//! manifest ends with a + (plus) character" describes is completed by the group
//! the request carries, exactly as the environment's own name is, and a target
//! written plainly is the name as written. So one call composes one environment
//! per version label whose manifest names one, each carrying the links ITS
//! manifest declared with every target resolved to the environment of the group
//! it names — whether that environment is one this same call composes or one the
//! group was already running — and a manifest that declares none composes an
//! environment with none.
//!
//! Every name and every link target this suite expects is written out as LITERAL
//! table data, so the completion the service performs is pinned by this file
//! rather than recomputed from the service.
//!
//! The table spans what the rule generalises over: how many labels one call
//! names, which of them declares the links, how many links one manifest
//! declares, whether a link's target is a group template or a plain name,
//! whether the target is composed by the same call or by an earlier one, and
//! which tier each manifest declares (or none). Each row asserts the links the
//! composition ANSWERS and the links the environment then STORES, read back
//! through `DescribeEnvironments`, so a link that is only reported by the call
//! that created it fails the row.
//!
//! The manifest writer, the version upload and the composition call are the ones
//! the `composed_environments` suite owns, and the harness (the router, the form
//! encoder, the XML reader and the status poll) is `lifecycle`'s.

use crate::composed_environments::{
    compose, described_environments, given_application, given_version, manifest, WORKER_TIER_TYPE,
};
use crate::harness::{wait_for_status, Emulator, SOLUTION_STACK};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The group every composition below names, and the suffix the completed names
/// in the table carry because of it: a manifest name ending in `+` is completed
/// by dropping the placeholder and appending `-<group name>`.
const GROUP_NAME: &str = "dev";

/// One manifest a row composes from: the version label whose source bundle
/// carries it, its `EnvironmentName`, the tier it declares (none at all where it
/// is `None`), the links it declares as the manifest writes them, the
/// environment name it must be composed under, and the links that environment
/// must report — the last two written out, not derived.
type ManifestRow = (
    &'static str,
    &'static str,
    Option<(&'static str, &'static str)>,
    &'static [(&'static str, &'static str)],
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// One row of the table: the case, the manifests composed by an EARLIER call (so
/// a link can name an environment the group was already running), and the
/// manifests the composition under test composes.
type CompositionRow = (
    &'static str,
    &'static [ManifestRow],
    &'static [ManifestRow],
);

/// The web tier, as a manifest's `EnvironmentTier` declares it.
const WEB_SERVER_TIER: Option<(&str, &str)> = Some((
    eb::state::TIER_NAME_WEB_SERVER,
    eb::state::TIER_TYPE_STANDARD,
));
/// The worker tier, beside [`WEB_SERVER_TIER`].
const WORKER_TIER: Option<(&str, &str)> = Some((eb::state::TIER_NAME_WORKER, WORKER_TIER_TYPE));

/// The link name a web manifest gives the worker queue it connects to, as
/// Elastic Beanstalk's environment-manifest reference writes one, and the names
/// a row declaring more than one link uses beside it.
const LINK_NAME: &str = "WORKERQUEUE";
/// See [`LINK_NAME`].
const SECOND_LINK_NAME: &str = "CACHE";

/// The `EnvironmentStatus` a composed launch reaches, from the service's own
/// constant.
const READY: &str = eb::state::STATUS_READY;

/// The links one environment carries, as `(LinkName, EnvironmentName)` pairs
/// read out of an `EnvironmentDescription`.
fn links_of(environment: &crate::harness::Answer) -> Vec<(String, String)> {
    environment
        .elements_at("EnvironmentLinks/member")
        .iter()
        .map(|link| {
            (
                link.value("LinkName").unwrap_or_default().to_string(),
                link.value("EnvironmentName").unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The links the table states an environment must carry, as the pairs
/// [`links_of`] reads.
fn stated_links(links: &[(&str, &str)]) -> Vec<(String, String)> {
    links
        .iter()
        .map(|(link_name, target)| ((*link_name).to_string(), (*target).to_string()))
        .collect()
}

/// Compose `manifests` and assert the call answered one environment per
/// manifest, under the name the row states, carrying the links the row states.
async fn composed_with_its_links(emulator: &Emulator, case: &str, manifests: &[ManifestRow]) {
    let labels = manifests
        .iter()
        .map(|(version_label, _, _, _, _, _)| *version_label)
        .collect::<Vec<_>>();
    let composed = compose(emulator, Some(GROUP_NAME), &labels).await;
    assert_eq!(
        composed.status, 200,
        "case {case:?}: expected 200 from ComposeEnvironments, got {} and body {}",
        composed.status, composed.body
    );
    let answered = composed.elements_at("Environments/member");
    let answered_names = answered
        .iter()
        .map(|environment| environment.value("EnvironmentName").unwrap_or_default())
        .collect::<Vec<_>>();
    let expected_names = manifests
        .iter()
        .map(|(_, _, _, _, environment_name, _)| *environment_name)
        .collect::<Vec<_>>();
    assert_eq!(
        answered_names, expected_names,
        "case {case:?}: expected one environment per version label, named \
         {expected_names:?}, got {answered_names:?} and body {}",
        composed.body
    );
    for (environment, (_, _, _, _, environment_name, expected_links)) in
        answered.iter().zip(manifests)
    {
        let answered_links = links_of(environment);
        assert_eq!(
            answered_links,
            stated_links(expected_links),
            "case {case:?}: expected {environment_name:?} to report the links its manifest \
             declared, got {answered_links:?} and body {}",
            composed.body
        );
    }
}

/// Every way a manifest set can wire the links of the group it composes.
#[tokio::test]
async fn a_composition_creates_the_links_its_manifests_declare() {
    let rows: [CompositionRow; 4] = [
        (
            "a two-tier group whose web manifest links the worker the same call composes",
            &[],
            &[
                (
                    "v-front",
                    "l-front+",
                    WEB_SERVER_TIER,
                    &[(LINK_NAME, "l-worker+")],
                    "l-front-dev",
                    &[(LINK_NAME, "l-worker-dev")],
                ),
                (
                    "v-worker",
                    "l-worker+",
                    WORKER_TIER,
                    &[],
                    "l-worker-dev",
                    &[],
                ),
            ],
        ),
        (
            "the same group with the link declared by the WORKER manifest instead",
            &[],
            &[
                (
                    "v-front",
                    "l-front+",
                    WEB_SERVER_TIER,
                    &[],
                    "l-front-dev",
                    &[],
                ),
                (
                    "v-worker",
                    "l-worker+",
                    WORKER_TIER,
                    &[("FRONTEND", "l-front+")],
                    "l-worker-dev",
                    &[("FRONTEND", "l-front-dev")],
                ),
            ],
        ),
        (
            "one manifest declaring two links, to the two other members of the group",
            &[],
            &[
                (
                    "v-hub",
                    "l-hub+",
                    WEB_SERVER_TIER,
                    &[(LINK_NAME, "l-worker+"), (SECOND_LINK_NAME, "l-cache+")],
                    "l-hub-dev",
                    &[
                        (LINK_NAME, "l-worker-dev"),
                        (SECOND_LINK_NAME, "l-cache-dev"),
                    ],
                ),
                (
                    "v-worker",
                    "l-worker+",
                    WORKER_TIER,
                    &[],
                    "l-worker-dev",
                    &[],
                ),
                ("v-cache", "l-cache+", None, &[], "l-cache-dev", &[]),
            ],
        ),
        (
            "a link whose PLAIN target names the environment an earlier composition created",
            &[("v-store", "l-store", None, &[], "l-store", &[])],
            &[(
                "v-front",
                "l-front+",
                WEB_SERVER_TIER,
                &[(LINK_NAME, "l-store")],
                "l-front-dev",
                &[(LINK_NAME, "l-store")],
            )],
        ),
    ];

    for (case, given, manifests) in rows {
        let emulator = Emulator::with_object_storage();
        given_application(&emulator, case).await;
        for (version_label, environment_name, tier, links, _, _) in given.iter().chain(manifests) {
            given_version(
                &emulator,
                case,
                version_label,
                &[(
                    "env.yaml",
                    &manifest(
                        environment_name,
                        Some(SOLUTION_STACK.as_str()),
                        *tier,
                        &[],
                        links,
                    ),
                )],
            )
            .await;
        }
        // What the row says the group was already running when the composition
        // under test arrived: composed by its own call and settled, so a link
        // naming it names an environment the group runs.
        if !given.is_empty() {
            composed_with_its_links(&emulator, case, given).await;
            for (_, _, _, _, environment_name, _) in given {
                wait_for_status(&emulator, case, environment_name, READY).await;
            }
        }

        composed_with_its_links(&emulator, case, manifests).await;
        for (_, _, _, _, environment_name, _) in manifests {
            wait_for_status(&emulator, case, environment_name, READY).await;
        }

        // Every link is on the STORED environment, not only in the answer of
        // the call that created it, and the group holds exactly the
        // environments the compositions composed.
        let (described, answer) = described_environments(&emulator, case).await;
        let mut stored = described
            .iter()
            .map(|environment| environment.value("EnvironmentName").unwrap_or_default())
            .collect::<Vec<_>>();
        stored.sort_unstable();
        let mut expected = given
            .iter()
            .chain(manifests)
            .map(|(_, _, _, _, environment_name, _)| *environment_name)
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            stored, expected,
            "case {case:?}: expected the application to hold {expected:?}, got {stored:?} and \
             body {}",
            answer.body
        );
        for (_, _, _, _, environment_name, expected_links) in given.iter().chain(manifests) {
            let stored_links = described
                .iter()
                .find(|environment| environment.value("EnvironmentName") == Some(environment_name))
                .map(links_of)
                .unwrap_or_default();
            assert_eq!(
                stored_links,
                stated_links(expected_links),
                "case {case:?}: expected the stored {environment_name:?} to carry the links its \
                 manifest declared, got {stored_links:?} and body {}",
                answer.body
            );
        }
    }
}
