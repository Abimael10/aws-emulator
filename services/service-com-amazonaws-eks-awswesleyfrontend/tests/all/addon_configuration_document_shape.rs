// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SHAPE in which `DescribeAddonConfiguration` declares
//! an OBJECT-VALUED configuration key of one build of one add-on.
//!
//! The RULE under test: a published add-on configuration document declares the
//! object-valued configuration keys of the BUILD it is published for the way that
//! document carries them — in a document that declares the add-on's keys at its
//! ROOT there is no `definitions` section, so an object-valued key declares its
//! own keys INLINE under it, with no definition title, and the set of them is
//! CLOSED (`additionalProperties: false`) exactly as the document's root is, while
//! an object the document declares no key of stays open because the chart's own
//! values go inside it; a document of the `definitions` form keeps naming its
//! types, and each definition is titled after the type it describes.
//!
//! The suite runs over a TABLE: it walks EVERY add-on the catalog publishes and,
//! per add-on, its newest and its oldest published build — so the rows differ
//! along the add-on, the document form, the build (either side of a release that
//! moved a key between the root and an object) and whether an object declares keys
//! at all. That a document is what an INSTALL is held to, inside an object-valued
//! key too, is a row of the install table in `addon_version_records.rs`.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::addon_catalog_publication::configuration_schema;
use crate::addon_versions::{addons, string_member, versions_of, whole_catalog};
use crate::wire::emulator;

/// The JSON Schema keyword that closes a set of properties: a key the schema does
/// not declare does not satisfy it.
const CLOSED: &str = "additionalProperties";

/// One published document, as this suite reads it.
struct Document {
    case: String,
    body: serde_json::Value,
}

impl Document {
    /// Whether the document declares the add-on's keys at its own root, which is
    /// the form that carries no `definitions` section.
    fn declares_keys_at_root(&self) -> bool {
        self.body["definitions"].is_null()
    }

    /// The keys the document declares at its root, each with the node declaring
    /// it.
    fn root_properties(&self) -> Vec<(String, serde_json::Value)> {
        self.body["properties"]
            .as_object()
            .map(|declared| {
                declared
                    .iter()
                    .map(|(key, node)| (key.clone(), node.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Every add-on the catalog publishes, each with its newest and its oldest
/// published build: the table this suite is rows of.
async fn published_builds(router: &MultiServiceRouter) -> Vec<(String, Vec<String>)> {
    let catalog = addons(&whole_catalog(router).await);
    assert!(
        !catalog.is_empty(),
        "setup DescribeAddonVersions: the catalog must publish at least one add-on"
    );
    catalog
        .iter()
        .map(|addon| {
            let addon_name = string_member(addon, "addonName");
            let builds = versions_of(addon);
            assert!(
                !builds.is_empty(),
                "setup {addon_name}: a published add-on must publish a build"
            );
            // The newest and the oldest build, which are the two sides of any
            // release that moved a key in the add-on's published documents.
            let newest = string_member(&builds[0], "addonVersion");
            let oldest = string_member(
                builds
                    .last()
                    .unwrap_or_else(|| panic!("setup {addon_name}: expected a published build")),
                "addonVersion",
            );
            let mut wanted = vec![newest];
            if !wanted.contains(&oldest) {
                wanted.push(oldest);
            }
            (addon_name, wanted)
        })
        .collect()
}

async fn document(router: &MultiServiceRouter, addon_name: &str, addon_version: &str) -> Document {
    Document {
        case: format!("{addon_name}/{addon_version}"),
        body: configuration_schema(router, addon_name, addon_version).await,
    }
}

// ---------------------------------------------------------------------------
// RULE: a document that declares its keys at its root carries no definition and
// no definition title; each object-valued key declares its own keys inline and
// closes them where it declares any. A document of the `definitions` form names
// its types and titles each definition after the type it describes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_object_valued_key_is_declared_the_way_its_document_carries_it() {
    let router = emulator();

    // What the walk has to have SEEN for the table to span the rule: both
    // document forms, an inline object whose keys the document declares (so it
    // is closed) and one it declares none of (so it stays open).
    let mut root_declared: BTreeSet<String> = BTreeSet::new();
    let mut definition_form: BTreeSet<String> = BTreeSet::new();
    let mut closed_objects: BTreeSet<String> = BTreeSet::new();
    let mut open_objects: BTreeSet<String> = BTreeSet::new();

    for (addon_name, builds) in published_builds(&router).await {
        for addon_version in builds {
            let document = document(&router, &addon_name, &addon_version).await;
            let case = &document.case;
            if !document.declares_keys_at_root() {
                definition_form.insert(addon_name.clone());
                assert!(
                    document.body["$ref"]
                        .as_str()
                        .is_some_and(|pointer| pointer.starts_with("#/definitions/")),
                    "case {case}: a document with a definitions section names its own values \
                     type at its root, got {:?}",
                    document.body["$ref"]
                );
                let definitions = document.body["definitions"]
                    .as_object()
                    .unwrap_or_else(|| panic!("case {case}: expected a definitions section"));
                for (title, definition) in definitions {
                    assert_eq!(
                        definition["title"].as_str(),
                        Some(title.as_str()),
                        "case {case}/{title}: a definition is titled after the type it \
                         describes, got {definition:?}"
                    );
                }
                continue;
            }

            root_declared.insert(addon_name.clone());
            assert!(
                document.body["$ref"].is_null(),
                "case {case}: a document that declares its keys at its root references no \
                 definition, got {:?}",
                document.body["$ref"]
            );
            assert_eq!(
                document.body[CLOSED],
                serde_json::json!(false),
                "case {case}: the document's root is closed around the keys the add-on takes, \
                 got {:?}",
                document.body[CLOSED]
            );
            let declared = document.root_properties();
            assert!(
                !declared.is_empty(),
                "case {case}: expected the add-on's keys at the document root, got {:?}",
                document.body
            );
            for (key, node) in declared {
                assert!(
                    node["title"].is_null(),
                    "case {case}/{key}: a document with no definitions section titles no node, \
                     got {node:?}"
                );
                if node["type"].as_str() != Some("object") {
                    continue;
                }
                let own_keys = node["properties"]
                    .as_object()
                    .map(|keys| keys.len())
                    .unwrap_or_default();
                if own_keys == 0 {
                    open_objects.insert(format!("{addon_name}.{key}"));
                    assert!(
                        node[CLOSED].is_null(),
                        "case {case}/{key}: an object the document declares no key of takes the \
                         chart's own values, so it is not closed, got {node:?}"
                    );
                    continue;
                }
                closed_objects.insert(format!("{addon_name}.{key}"));
                assert_eq!(
                    node[CLOSED],
                    serde_json::json!(false),
                    "case {case}/{key}: an object the document declares the keys of is closed \
                     around them, got {node:?}"
                );
                for (nested, declared) in node["properties"]
                    .as_object()
                    .unwrap_or(&serde_json::Map::new())
                {
                    assert!(
                        declared["type"].is_string(),
                        "case {case}/{key}.{nested}: a declared key names the JSON type it \
                         takes, got {declared:?}"
                    );
                    assert!(
                        declared["title"].is_null(),
                        "case {case}/{key}.{nested}: a document with no definitions section \
                         titles no node, got {declared:?}"
                    );
                }
            }
        }
    }

    assert!(
        !root_declared.is_empty() && !definition_form.is_empty(),
        "the walk must span both published document forms, got root-declared {root_declared:?} \
         and definition-form {definition_form:?}"
    );
    assert!(
        !closed_objects.is_empty(),
        "the walk must reach an object whose own keys a root-declared document declares, got \
         {closed_objects:?}"
    );
    assert!(
        !open_objects.is_empty(),
        "the walk must reach an object a root-declared document declares no key of, got \
         {open_objects:?}"
    );
}
