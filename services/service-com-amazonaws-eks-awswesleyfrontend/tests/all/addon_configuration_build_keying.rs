// SPDX-License-Identifier: CC0-1.0

//! What `DescribeAddonConfiguration` publishes for ONE BUILD of an add-on, read
//! over the wire through the same `MultiServiceRouter` the production binary
//! mounts.
//!
//! The RULE under test: the operation resolves one `(addonName, addonVersion)`
//! pair — both members are `@required` — so the configuration document and the
//! pod-identity list it answers with are the ones published for THAT BUILD. A
//! configuration key an add-on declares in a different place in different releases
//! of its own is declared where the requested build's own document puts it, and in
//! that one place only; and the answer changes from build to build exactly where
//! the add-on publishes a change, so two builds whose published surface is the same
//! answer with the same document and list.
//!
//! Three suites assert it, over the published build line of the add-on whose
//! documents changed across its releases and over add-ons whose documents did not:
//!
//! * the placement of the moved key, with every published build of the line a row,
//!   so the rows span both published bounds (the oldest build and the newest) and
//!   both sides of the release the key moved in;
//! * the answer as a function of the build: two builds answer alike exactly when
//!   their published surface is the same, which is asserted for the add-on that
//!   published a change and for add-ons that published none;
//! * the read changes nothing a later caller sees, each build read again after its
//!   neighbours.
//!
//! The harness is the crate's own (`wire.rs`), and the catalog readers are the ones
//! `addon_versions.rs` and `addon_catalog_publication.rs` own.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::{resolved, schema_root};
use crate::addon_versions::{
    addons, describe_addon_configuration, string_member, version_key, versions_of, whole_catalog,
};
use crate::wire::emulator;

/// One configuration key an add-on declares in a different place in different
/// releases of its own.
///
/// SOURCE: the document each build publishes, which `aws eks
/// describe-addon-configuration --addon-name <name> --addon-version <build>`
/// reports — the external authority this expectation is taken from, not the
/// catalog under test. The EFS CSI driver's earlier documents declare the
/// access-point switch at the document ROOT; from the driver's 2.0 line on, the
/// release that gathered the controller's values under `controller`, its documents
/// declare the same key inside that object.
struct MovedKey {
    /// The add-on whose documents declare the key.
    addon: &'static str,
    /// The key itself.
    property: &'static str,
    /// The JSON Schema type it takes wherever it is declared.
    json_type: &'static str,
    /// The object key the documents of the add-on's OLDER builds declare it under,
    /// or `None` for the document's root.
    declared_before: Option<&'static str>,
    /// The object key the documents of its builds from [`Self::moved_in`] on
    /// declare it under, or `None` for the document's root.
    declared_from: Option<&'static str>,
    /// The add-on's own release, as `(major, minor, patch)`, whose documents first
    /// declare it at [`Self::declared_from`].
    moved_in: (u32, u32, u32),
}

/// The moved key this suite walks a whole published build line for.
const MOVED: MovedKey = MovedKey {
    addon: "aws-efs-csi-driver",
    property: "deleteAccessPointRootDir",
    json_type: "boolean",
    declared_before: None,
    declared_from: Some("controller"),
    moved_in: (2, 0, 0),
};

/// The releases from which the EFS CSI driver's builds publish a pod-identity
/// recommendation for each of the service accounts it runs as.
///
/// SOURCE: the minimum add-on version the Amazon EKS User Guide publishes for each
/// component of that add-on that supports EKS Pod Identity — the external authority
/// this expectation is taken from, not the catalog under test.
/// `addon_pod_identity_catalog.rs` asserts WHICH accounts each build publishes;
/// here how many of them it publishes is one half of what makes two builds' answers
/// differ.
const POD_IDENTITY_FROM: &[(u32, u32, u32)] = &[(2, 0, 7), (2, 1, 0)];

/// The `(major, minor, patch)` release a `vX.Y.Z-eksbuild.N` add-on version names,
/// which is what orders one add-on's builds.
fn release_of(addon_version: &str) -> (u32, u32, u32) {
    let (major, minor, patch, _) = version_key(addon_version);
    (major, minor, patch)
}

/// Every build the catalog publishes of `addon_name`, newest first — the order
/// `DescribeAddonVersions` publishes them in.
async fn published_builds(router: &MultiServiceRouter, addon_name: &str) -> Vec<String> {
    let catalog = addons(&whole_catalog(router).await);
    let addon = catalog
        .iter()
        .find(|addon| string_member(addon, "addonName") == addon_name)
        .unwrap_or_else(|| panic!("the catalog must publish {addon_name}"));
    let builds: Vec<String> = versions_of(addon)
        .iter()
        .map(|build| string_member(build, "addonVersion"))
        .collect();
    assert!(
        builds.len() > 1,
        "setup: the catalog must publish several builds of {addon_name}, got {builds:?}"
    );
    builds
}

/// The `configurationSchema` and `podIdentityConfiguration` one published pair is
/// answered with, as the document and the list a client reads.
async fn published_configuration(
    router: &MultiServiceRouter,
    addon_name: &str,
    addon_version: &str,
) -> (serde_json::Value, serde_json::Value) {
    let case = format!("{addon_name}/{addon_version}");
    let answer = describe_addon_configuration(router, addon_name, addon_version).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200, got {answer:?}"
    );
    // The answer is about the pair that was asked for, which is what makes the rest
    // of these assertions about the requested build at all.
    assert_eq!(
        string_member(&answer.body, "addonName"),
        addon_name,
        "case {case}: expected the add-on the request named, got {:?}",
        answer.body["addonName"]
    );
    assert_eq!(
        string_member(&answer.body, "addonVersion"),
        addon_version,
        "case {case}: expected the build the request named, got {:?}",
        answer.body["addonVersion"]
    );
    let document: serde_json::Value =
        serde_json::from_str(&string_member(&answer.body, "configurationSchema")).unwrap_or_else(
            |error| panic!("case {case}: configurationSchema must be JSON: {error}"),
        );
    let pod_identity = answer.body["podIdentityConfiguration"].clone();
    assert!(
        pod_identity.is_array(),
        "case {case}: expected a podIdentityConfiguration list, got {pod_identity:?}"
    );
    (document, pod_identity)
}

/// The node the published document declares for `property` inside `parent` — or at
/// the document's root when `parent` is `None` — or `None` when the document
/// declares it nowhere there.
///
/// A key whose value is an object reads either as the object's own declaration or
/// as a reference into the document's `definitions`, depending on the dialect the
/// add-on publishes in, so the parent is resolved before its keys are read.
fn declared_in<'a>(
    document: &'a serde_json::Value,
    parent: Option<&str>,
    property: &str,
) -> Option<&'a serde_json::Value> {
    let root = schema_root(document);
    let object = match parent {
        None => root,
        Some(parent) => {
            let declared = &root["properties"][parent];
            if declared.is_null() {
                return None;
            }
            resolved(document, declared)
        }
    };
    let declared = &object["properties"][property];
    (!declared.is_null()).then_some(declared)
}

#[tokio::test]
async fn a_build_declares_a_moved_key_where_its_own_document_puts_it() {
    let router = emulator();
    let builds = published_builds(&router, MOVED.addon).await;

    let mut before = 0;
    let mut after = 0;
    for build in &builds {
        let case = format!("{}/{build}", MOVED.addon);
        let (document, _) = published_configuration(&router, MOVED.addon, build).await;
        let (expected, unexpected) = if release_of(build) >= MOVED.moved_in {
            after += 1;
            (MOVED.declared_from, MOVED.declared_before)
        } else {
            before += 1;
            (MOVED.declared_before, MOVED.declared_from)
        };

        let declared = declared_in(&document, expected, MOVED.property);
        assert_eq!(
            declared.map(|declared| declared["type"].clone()),
            Some(serde_json::json!(MOVED.json_type)),
            "case {case}: expected {} declared under {expected:?} as a {}, got {declared:?}",
            MOVED.property,
            MOVED.json_type
        );
        // The key the build's document moved is declared in ONE place: a client
        // that walks the document finds it where that build puts it and nowhere
        // else, so it cannot set it in a place the install then refuses.
        let elsewhere = declared_in(&document, unexpected, MOVED.property);
        assert!(
            elsewhere.is_none(),
            "case {case}: {} is declared under {unexpected:?} as well, got {elsewhere:?}",
            MOVED.property
        );
    }

    assert!(
        before > 0 && after > 0,
        "setup: the published line of {} must cross the release {:?} the key moved in, got \
         {before} builds before it and {after} from it on",
        MOVED.addon,
        MOVED.moved_in
    );
}

/// What the catalog publishes DIFFERENTLY across the builds of ONE add-on: where
/// that add-on's moved key sits in the build's document, and how many of its
/// service accounts the build publishes a pod-identity recommendation for. It
/// LABELS the builds of one add-on into the classes that have to be answered
/// alike, so two builds that agree on it publish the same document and the same
/// list; it says nothing across add-ons.
type PublishedSurface = (Option<&'static str>, usize);

/// One build's answer, as the suite below compares the builds of one line.
struct Answered {
    /// The build the answer is about.
    build: String,
    /// The surface the catalog publishes for it.
    surface: PublishedSurface,
    /// The document and the pod-identity list it was answered with.
    answer: serde_json::Value,
}

/// The surface the catalog publishes for one build. See [`PublishedSurface`].
fn published_surface(addon_name: &str, addon_version: &str) -> PublishedSurface {
    if addon_name != MOVED.addon {
        // An add-on the catalog publishes no per-release change for has ONE class:
        // every build of it publishes the same surface.
        return (None, 0);
    }
    let release = release_of(addon_version);
    let accounts = POD_IDENTITY_FROM
        .iter()
        .filter(|first_build| release >= **first_build)
        .count();
    (MOVED.declared_in(release), accounts)
}

impl MovedKey {
    /// Where the documents of the builds of release `release` declare the key.
    fn declared_in(&self, release: (u32, u32, u32)) -> Option<&'static str> {
        if release >= self.moved_in {
            self.declared_from
        } else {
            self.declared_before
        }
    }
}

/// The add-ons this suite walks, each with whether the catalog publishes a change
/// across its build line.
///
/// SOURCE: the documents and pod-identity configurations the add-ons publish per
/// build (see [`MOVED`] and [`POD_IDENTITY_FROM`]). The rows differ along what the
/// rule generalises over: an add-on whose published surface changes across its
/// line, one whose builds all publish the same pod-identity entry, one that
/// publishes none at all, and one whose document carries objects of its own.
const WALKED: &[(&str, bool)] = &[
    ("aws-efs-csi-driver", true),
    ("vpc-cni", false),
    ("coredns", false),
    ("aws-ebs-csi-driver", false),
];

#[tokio::test]
async fn two_builds_answer_alike_exactly_where_the_catalog_publishes_no_change() {
    let router = emulator();

    for (addon_name, changes) in WALKED {
        let builds = published_builds(&router, addon_name).await;
        let mut answered: Vec<Answered> = Vec::new();
        for build in &builds {
            let (document, pod_identity) =
                published_configuration(&router, addon_name, build).await;
            answered.push(Answered {
                build: build.clone(),
                surface: published_surface(addon_name, build),
                answer: serde_json::json!([document, pod_identity]),
            });
        }

        for one in &answered {
            for other in &answered {
                let case = format!(
                    "{addon_name}/{} against {addon_name}/{}",
                    one.build, other.build
                );
                if one.surface == other.surface {
                    assert_eq!(
                        one.answer, other.answer,
                        "case {case}: two builds that publish the same surface {:?} must \
                         be answered alike, got {:?} against {:?}",
                        one.surface, one.answer, other.answer
                    );
                } else {
                    assert_ne!(
                        one.answer, other.answer,
                        "case {case}: a build published with {:?} must not be answered \
                         with what one published with {:?} gets, got {:?}",
                        one.surface, other.surface, one.answer
                    );
                }
            }
        }

        // The rows prove the rule only where the line is what it claims: an add-on
        // the catalog publishes a change for must answer with more than one surface
        // across its builds, and one it publishes no change for with exactly one.
        let surfaces: BTreeSet<String> = answered
            .iter()
            .map(|answered| format!("{:?}", answered.surface))
            .collect();
        assert_eq!(
            surfaces.len() > 1,
            *changes,
            "setup {addon_name}: expected the published line to carry {} published surface(s), \
             got {surfaces:?}",
            if *changes { "several" } else { "one" }
        );
    }
}

#[tokio::test]
async fn reading_one_builds_configuration_changes_no_other_answer() {
    let router = emulator();
    let builds = published_builds(&router, MOVED.addon).await;

    // Read the whole line, then read it again in reverse: a reporting operation
    // writes nothing, so the second pass answers exactly what the first did.
    let mut first: Vec<(serde_json::Value, serde_json::Value)> = Vec::new();
    for build in &builds {
        first.push(published_configuration(&router, MOVED.addon, build).await);
    }
    for (build, answered) in builds.iter().zip(&first).rev() {
        let case = format!("{}/{build}", MOVED.addon);
        let again = published_configuration(&router, MOVED.addon, build).await;
        assert_eq!(
            &again, answered,
            "case {case}: reading a build's configuration again must answer the same document \
             and list, got {again:?}"
        );
    }
}
