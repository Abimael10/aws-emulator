// SPDX-License-Identifier: CC0-1.0

//! The unit tests of this service's state: the readings of a name, a version, a page token
//! and a refusal sentence that every operation of the crate shares.

use super::*;

#[test]
fn names_differing_only_in_case_spaces_or_underscores_normalize_alike() {
    let cases: &[(&str, &str)] = &[
        ("Build-Infra", "build-infra"),
        ("build infra", "build-infra"),
        ("build_infra", "build-infra"),
        ("BUILD INFRA", "build-infra"),
        ("build-infra", "build-infra"),
        ("Prod_Linux Build-01", "prod-linux-build-01"),
    ];
    for (name, expected) in cases {
        let actual = normalized_name(name);
        assert_eq!(
            &actual, expected,
            "case {name:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn a_minted_request_id_is_uuid_shaped_and_fresh() {
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..16 {
        let id = new_request_id();
        let groups: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(
            groups,
            vec![8, 4, 4, 4, 12],
            "case new_request_id: expected 8-4-4-4-12 groups, got {id:?}"
        );
        assert!(
            id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "case new_request_id: expected lowercase hex, got {id:?}"
        );
        assert!(
            !seen.contains(&id),
            "case new_request_id: expected a fresh id, got the already-minted {id:?}"
        );
        seen.push(id);
    }
}

#[test]
fn a_semantic_version_is_read_only_inside_the_documented_bound() {
    // Each row is a version and the three nodes it assigns, or `None` for one the
    // documented upper limit (2^30-1) or the three-node syntax refuses.
    let cases: &[(&str, Option<[u64; ASSIGNED_VERSION_NODES]>)] = &[
        ("1.0.0", Some([1, 0, 0])),
        ("0.0.0", Some([0, 0, 0])),
        ("2021.1.1", Some([2021, 1, 1])),
        ("1073741823.1073741823.1073741823", Some([
            MAX_VERSION_NODE,
            MAX_VERSION_NODE,
            MAX_VERSION_NODE,
        ])),
        ("1073741824.0.0", None),
        ("1.0", None),
        ("1.0.0.0", None),
        ("1.x.x", None),
    ];
    for (version, expected) in cases {
        let actual = parse_semantic_version(version);
        assert_eq!(
            &actual, expected,
            "case {version:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn a_component_reference_reads_build_and_wildcard_version_arns() {
    let build = component_build_version_arn("us-west-2", "Demo Component", "1.2.3", 4);
    let reference = ComponentReference::parse(&build)
        .unwrap_or_else(|| panic!("case {build:?}: expected a component reference"));
    assert_eq!(
        reference,
        ComponentReference {
            region: "us-west-2".to_string(),
            name: "demo-component".to_string(),
            version: [
                VersionNode::Exact(1),
                VersionNode::Exact(2),
                VersionNode::Exact(3)
            ],
            build_version: Some(4),
        },
        "case {build:?}: expected the ARN's own fields, got {reference:?}"
    );

    // A wildcard is admitted in TRAILING positions only, and a build version ARN
    // names an exact version.
    let cases: &[(&str, bool)] = &[
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.2.3", true),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.2.x", true),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.x.x", true),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/x.x.x", true),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/x.2.3", false),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.x.3", false),
        ("arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.x.x/1", false),
        ("arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/demo/1.2.3", false),
        ("arn:aws:ec2:us-west-2:000000000000:component/demo/1.2.3", false),
        ("not-an-arn", false),
    ];
    for (reference, readable) in cases {
        let actual = ComponentReference::parse(reference).is_some();
        assert_eq!(
            actual, *readable,
            "case {reference:?}: expected readable={readable}, got {actual}"
        );
    }
}

#[test]
fn a_component_document_declares_its_type_and_parameters() {
    // One phase named `test` is the only document that is a TEST component; the rows
    // vary the phase count, the phase name and the order.
    let cases: &[(&str, &str)] = &[
        ("name: t\nphases:\n  - name: test\n    steps: []\n", COMPONENT_TYPE_TEST),
        ("name: b\nphases:\n  - name: build\n    steps: []\n", COMPONENT_TYPE_BUILD),
        (
            "name: bt\nphases:\n  - name: build\n    steps: []\n  - name: test\n    steps: []\n",
            COMPONENT_TYPE_BUILD,
        ),
        (
            "name: tb\nphases:\n  - name: test\n    steps: []\n  - name: build\n    steps: []\n",
            COMPONENT_TYPE_BUILD,
        ),
    ];
    for (data, expected) in cases {
        let document = parse_component_document(data)
            .unwrap_or_else(|why| panic!("case {data:?}: expected a document, got {why}"));
        assert_eq!(
            &document.component_type, expected,
            "case {data:?}: expected type {expected:?}, got {:?}",
            document.component_type
        );
    }

    let declared = parse_component_document(
        "name: p\nparameters:\n  - Size:\n      type: string\n      default: small\n      \
             description: how big\n  - Hosts:\n      type: stringList\n      default:\n        \
             - a\n        - b\n  - NoType:\n      description: declares nothing\nphases:\n  - \
             name: build\n    steps: []\n",
    )
    .unwrap_or_else(|why| panic!("case parameters: expected a document, got {why}"));
    let parameters = declared
        .parameters
        .unwrap_or_else(|| panic!("case parameters: expected the declared parameters"));
    let actual: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            format!(
                "{}:{}:{:?}:{:?}",
                parameter.name,
                parameter.r#type,
                parameter.default_value,
                parameter.description
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            "Size:string:Some([\"small\"]):Some(\"how big\")".to_string(),
            "Hosts:stringList:Some([\"a\", \"b\"]):None".to_string(),
        ],
        "case parameters: expected the two typed parameters in document order, got {actual:?}"
    );
}

#[test]
fn a_document_that_is_not_one_yaml_mapping_or_uses_aliases_is_refused() {
    let deep_flow = "phases: ".to_string() + &"[".repeat(8_000);
    let quoted_closers = "phases: ".to_string() + &"[']'".repeat(4_000);
    let cases: Vec<String> = vec![
        // Two documents.
        "name: a\n---\nname: b\n".to_string(),
        // Not a mapping.
        "- name: a\n".to_string(),
        // An alias, which is refused rather than expanded.
        "anchor: &a [1, 2]\nphases: *a\n".to_string(),
        // Not YAML at all.
        "name: [a\n".to_string(),
        // Nesting built to drive the loader's recursion, refused before any reader
        // runs: open flow collections, and the same with every closer quoted so a
        // byte scan's running depth stays low while the loader's climbs.
        deep_flow,
        quoted_closers,
        // Block nesting, by indentation and by compact indicators on one line.
        " ".repeat(1_000) + "name: a\n",
        "phases:\n".to_string() + &"- ".repeat(1_000) + "x\n",
    ];
    for data in cases {
        let actual = parse_component_document(&data);
        assert!(
            actual.is_err(),
            "case {:?}…: expected a refusal, got {actual:?}",
            data.chars().take(32).collect::<String>()
        );
    }

    // A document that is big and FLAT is not deep, so none of those bounds touches
    // it: 150 steps at the document's own nesting, inside the modelled 16,000-byte
    // maximum for `data`, are read.
    let flat = "name: Flat\nphases:\n  - name: build\n    steps:\n".to_string()
        + &"      - {name: s, action: ExecuteBash, inputs: {commands: [echo hi]}}\n"
            .repeat(150);
    assert!(
        flat.len() <= 16_000,
        "case a big flat document: the row must stay inside the modelled data maximum, got {}",
        flat.len()
    );
    let actual = parse_component_document(&flat);
    assert!(
        actual.is_ok(),
        "case a big flat document: expected it to be read, got {actual:?}"
    );
}

/// RULE: a document that declares no phases describes no work, and is refused with the
/// clause this class carries whatever else the document holds.
#[test]
fn a_document_declaring_no_phases_is_refused_as_missing_phases() {
    // The rows differ along HOW the phases are missing: the key absent, present with an
    // empty list, present with no value at all, and present carrying something that is
    // not a list of phases.
    let cases: &[(&str, &str)] = &[
        ("no phases key at all", "name: x\nschemaVersion: 1.0\n"),
        ("nothing but the name", "name: x\n"),
        ("an empty phases list", "name: x\nphases: []\n"),
        ("a phases key with no value", "name: x\nphases:\n"),
        ("phases carrying a scalar", "name: x\nphases: build\n"),
        (
            "phases carrying a mapping rather than a list",
            "name: x\nphases:\n  build:\n    steps: []\n",
        ),
    ];
    for (case, data) in cases {
        let actual = parse_component_document(data);
        assert_eq!(
            actual.as_ref().err().map(String::as_str),
            Some(MISSING_PHASES_REQUIREMENT),
            "case {case:?}: expected the missing-phases clause, got {actual:?}"
        );
    }
}

/// RULE: an already-exists refusal names the label AWS gives the kind the ARN identifies,
/// which is the kind its own resource field spells except where a recorded refusal shows
/// otherwise.
#[test]
fn an_already_exists_refusal_labels_the_kind_its_arn_names() {
    // (case, the existing resource's ARN, the label the message carries)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a component build version",
            "arn:aws:imagebuilder:us-west-2:000000000000:component/demo/1.0.0/1",
            "Component",
        ),
        (
            "an image recipe",
            "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/demo/1.0.0",
            "ImageRecipe",
        ),
        (
            "a distribution configuration",
            "arn:aws:imagebuilder:us-west-2:000000000000:distribution-configuration/demo",
            "DistributionConfiguration",
        ),
        (
            "an infrastructure configuration",
            "arn:aws:imagebuilder:us-west-2:000000000000:infrastructure-configuration/demo",
            "InfrastructureConfiguration",
        ),
        (
            "a container recipe",
            "arn:aws:imagebuilder:us-west-2:000000000000:container-recipe/demo/1.0.0",
            "ContainerRecipe",
        ),
        (
            "an image pipeline, which AWS labels by the shorter word",
            "arn:aws:imagebuilder:us-west-2:000000000000:image-pipeline/demo",
            "Pipeline",
        ),
    ];
    for (case, arn, label) in rows {
        let actual = resource_already_exists_message(arn);
        let expected = format!("The following resource '{label}' already exists: '{arn}'.");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn a_page_token_is_read_back_only_for_the_listing_that_issued_it() {
    let key = PageTokenKey::new();
    let cursor = PageCursor {
        operation: LIST_IMAGE_RECIPES_OPERATION.to_string(),
        region: "us-west-2".to_string(),
        query: image_recipe_query_fingerprint(Some(OWNERSHIP_SELF), None),
        after: "arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/demo/1.0.0"
            .to_string(),
    };
    let token = encode_page_token(&key, &cursor);
    let actual = decode_page_token(&key, &token, &cursor);
    assert_eq!(
        actual.as_deref(),
        Some(cursor.after.as_str()),
        "case issued token: expected {:?}, got {actual:?}",
        cursor.after
    );

    // The token is opaque: none of the LISTING it continues is readable out of it,
    // however it is decoded — only the position, which is the key the page it follows
    // ended at and the caller was just handed.
    let decoded = ::aws_smithy_types::base64::decode(&token)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    for secret in [LIST_IMAGE_RECIPES_OPERATION, cursor.query.as_str()] {
        assert!(
            !token.contains(secret) && !decoded.contains(secret),
            "case opacity: expected {secret:?} to be absent from {token:?}"
        );
    }

    // A token read against another listing, Region or query is not this token, and
    // neither is anything the service did not issue.
    let other_query = PageCursor {
        query: image_recipe_query_fingerprint(Some("Amazon"), None),
        ..cursor.clone()
    };
    let other_region = PageCursor {
        region: "eu-west-1".to_string(),
        ..cursor.clone()
    };
    let other_operation = PageCursor {
        operation: LIST_COMPONENTS_OPERATION.to_string(),
        ..cursor.clone()
    };
    let cases: &[(&str, String)] = &[
        ("another query", token.clone()),
        ("another region", token.clone()),
        ("another operation", token.clone()),
        ("fabricated", "fabricated-token".to_string()),
        ("another store's key", encode_page_token(&PageTokenKey::new(), &cursor)),
        (
            "edited authenticator",
            ::aws_smithy_types::base64::encode(
                [
                    vec![0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES],
                    page_token_payload(&cursor).into_bytes(),
                ]
                .concat(),
            ),
        ),
    ];
    for (case, candidate) in cases {
        let cursor_of = match *case {
            "another query" => &other_query,
            "another region" => &other_region,
            "another operation" => &other_operation,
            _ => &cursor,
        };
        let actual = decode_page_token(&key, candidate, cursor_of);
        assert!(
            actual.is_none(),
            "case {case}: expected a refusal, got {actual:?}"
        );
    }
}

/// A component version is billable only when it is one the Marketplace publishes: the
/// `billable` member is read off the stored record's owner, so a component this account
/// created reports false.
#[test]
fn billable_follows_the_component_s_owner() {
    let record = ComponentRecord {
        arn: component_build_version_arn("us-west-2", "demo", "1.0.0", FIRST_BUILD_VERSION),
        region: "us-west-2".to_string(),
        name: "demo".to_string(),
        version: "1.0.0".to_string(),
        build_version: FIRST_BUILD_VERSION,
        description: None,
        change_description: None,
        component_type: COMPONENT_TYPE_BUILD.to_string(),
        platform: PLATFORM_LINUX.to_string(),
        supported_os_versions: None,
        owner: ::engine::EMULATOR_ACCOUNT_ID.to_string(),
        data: None,
        uri: None,
        kms_key_id: None,
        parameters: None,
        date_created: "2026-01-01T00:00:00Z".to_string(),
        tags: None,
    };
    // (case, the owner the record reports, whether the version is billable)
    let rows: &[(&str, String, bool)] = &[
        (
            "this account's own",
            ::engine::EMULATOR_ACCOUNT_ID.to_string(),
            false,
        ),
        (
            "another account's, shared with this one",
            "210987654321".to_string(),
            false,
        ),
        ("Amazon's own", "Amazon".to_string(), false),
        ("a third party's", "ThirdParty".to_string(), false),
        (
            "the Marketplace's",
            OWNERSHIP_AWS_MARKETPLACE.to_string(),
            true,
        ),
    ];
    for (case, owner, expected) in rows {
        let record = ComponentRecord {
            owner: owner.clone(),
            ..record.clone()
        };
        let actual = record.billable();
        assert_eq!(
            actual, *expected,
            "case {case:?}: expected billable {expected}, got {actual}"
        );
        let entry = record.version_entry(false);
        assert_eq!(
            entry.billable,
            Some(*expected),
            "case {case:?}: expected the listing entry to report billable \
                 {expected:?}, got {:?}",
            entry.billable
        );
    }
}

/// RULE: the status a build version reports is read from the clock — a build is outstanding
/// until it is due, a build something asked to STOP is the running build until its
/// cancellation is due and cancelled from that instant on (so it never reaches available), and
/// a build something ENDED reports the status it was ended in at every instant.
#[test]
fn the_status_of_a_build_version_follows_the_clock_of_its_build_and_its_cancellation() {
    let queued = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let due = queued + IMAGE_BUILD_SPAN;
    // (case, the instant the build is due, the instant a cancellation of it is due, the status
    // something ended it in, the instants to read it at with the status each one reports)
    type Row<'row> = (
        &'row str,
        Option<SystemTime>,
        Option<SystemTime>,
        Option<&'row str>,
        &'row [(Duration, &'row str)],
    );
    let rows: &[Row<'_>] = &[
        (
            "a build nothing stopped",
            Some(due),
            None,
            None,
            &[
                (Duration::ZERO, IMAGE_STATUS_PENDING),
                (IMAGE_BUILD_SPAN, IMAGE_STATUS_AVAILABLE),
                (
                    IMAGE_BUILD_SPAN + Duration::from_secs(60),
                    IMAGE_STATUS_AVAILABLE,
                ),
            ],
        ),
        (
            "a build cancelled before it was due",
            Some(due),
            Some(queued + Duration::from_secs(1) + IMAGE_CANCELLATION_SPAN),
            None,
            &[
                (Duration::ZERO, IMAGE_STATUS_PENDING),
                // Still the running build while its cancellation is outstanding, even past the
                // instant its own build would have been due.
                (IMAGE_BUILD_SPAN, IMAGE_STATUS_PENDING),
                (
                    Duration::from_secs(1) + IMAGE_CANCELLATION_SPAN,
                    IMAGE_STATUS_CANCELLED,
                ),
                (Duration::from_secs(600), IMAGE_STATUS_CANCELLED),
            ],
        ),
        (
            "a build something ended",
            Some(due),
            Some(queued + IMAGE_CANCELLATION_SPAN),
            Some(IMAGE_STATUS_FAILED),
            &[
                (Duration::ZERO, IMAGE_STATUS_FAILED),
                (IMAGE_CANCELLATION_SPAN, IMAGE_STATUS_FAILED),
                (Duration::from_secs(600), IMAGE_STATUS_FAILED),
            ],
        ),
        (
            "a build that was already available",
            None,
            None,
            None,
            &[
                (Duration::ZERO, IMAGE_STATUS_AVAILABLE),
                (Duration::from_secs(600), IMAGE_STATUS_AVAILABLE),
            ],
        ),
    ];
    for (case, available_at, cancelled_at, terminal_status, reads) in rows {
        let record = ImageBuildVersionRecord {
            available_at: *available_at,
            cancelled_at: *cancelled_at,
            terminal_status: terminal_status.map(ToString::to_string),
            ..ImageBuildVersionRecord::default()
        };
        for (after, expected) in *reads {
            let at = queued + *after;
            let actual = record.status_at(at);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} {after:?} after the build was queued, \
                 got {actual:?}"
            );
        }
    }
}

/// RULE: the phase a build STOPPED in is read from the record only once that build has
/// stopped, which the clock decides: a build still running has stopped in no phase however
/// long it has run and whatever a pending cancellation recorded, one that ran to completion has
/// stopped in none either, and a stopped build this store recorded no phase for stopped in the
/// phase every build of this emulator runs. Only the two phases a retry resumes from make that
/// build resumable.
#[test]
fn the_phase_a_build_stopped_in_is_read_once_the_clock_has_stopped_it() {
    let queued = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let due = queued + IMAGE_BUILD_SPAN;
    let cancelled = queued + IMAGE_CANCELLATION_SPAN;
    // (case, the instant the build is due, the instant a cancellation of it is due, the status
    // something ended it in, the phase this store recorded for it, the instants to read it at
    // with the phase it reports at each one)
    type Row<'row> = (
        &'row str,
        Option<SystemTime>,
        Option<SystemTime>,
        Option<&'row str>,
        Option<&'row str>,
        &'row [(Duration, Option<&'row str>)],
    );
    let rows: &[Row<'_>] = &[
        (
            "a build nothing stopped",
            Some(due),
            None,
            None,
            None,
            &[
                (Duration::ZERO, None),
                (IMAGE_BUILD_SPAN, None),
                (Duration::from_secs(600), None),
            ],
        ),
        (
            "a build whose cancellation is not due yet",
            Some(due),
            Some(cancelled),
            None,
            Some(IMAGE_PHASE_BUILDING),
            &[
                (Duration::ZERO, None),
                // Stopped, and so carrying the phase it stopped in, only from the instant its
                // cancellation is due.
                (IMAGE_CANCELLATION_SPAN, Some(IMAGE_PHASE_BUILDING)),
            ],
        ),
        (
            "a build something stopped in a phase a retry resumes from",
            Some(due),
            None,
            Some(IMAGE_STATUS_FAILED),
            Some(IMAGE_PHASE_DISTRIBUTING),
            &[
                (Duration::ZERO, Some(IMAGE_PHASE_DISTRIBUTING)),
                (Duration::from_secs(600), Some(IMAGE_PHASE_DISTRIBUTING)),
            ],
        ),
        (
            "a stopped build this store recorded no phase for",
            Some(due),
            None,
            Some(IMAGE_STATUS_CANCELLED),
            None,
            &[(Duration::ZERO, Some(IMAGE_PHASE_BUILDING))],
        ),
        (
            "a build that ran to completion",
            None,
            None,
            None,
            Some(IMAGE_PHASE_TESTING),
            &[(Duration::ZERO, None), (Duration::from_secs(600), None)],
        ),
    ];
    for (case, available_at, cancelled_at, terminal_status, stopped_in_phase, reads) in rows {
        let record = ImageBuildVersionRecord {
            available_at: *available_at,
            cancelled_at: *cancelled_at,
            terminal_status: terminal_status.map(ToString::to_string),
            stopped_in_phase: stopped_in_phase.map(ToString::to_string),
            ..ImageBuildVersionRecord::default()
        };
        for (after, expected) in *reads {
            let at = queued + *after;
            let actual = record.stopped_in_phase_at(at);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the stopped phase {expected:?} {after:?} after the \
                 build was queued, got {actual:?}"
            );
            let resumable = record.resumable_at(at);
            let expected_resumable =
                expected.is_some_and(|phase| RESUMABLE_IMAGE_PHASES.contains(&phase));
            assert_eq!(
                resumable, expected_resumable,
                "case {case:?}: expected resumable {expected_resumable} {after:?} after the \
                 build was queued, got {resumable}"
            );
        }
    }
}

/// RULE: a listing's STATED filter names are the SET that listing filters on, in the order it
/// states them — so the two orders the image listings state cannot drift into two sets, which
/// would admit a filter one listing refuses or refuse one it admits.
#[test]
fn the_filter_names_a_listing_states_are_the_ones_it_filters_on() {
    // (case, the set the listing filters on, the names its refusal states)
    let rows: &[(&str, &[&str], &[&str])] = &[(
        "the image listing",
        IMAGE_FILTER_NAMES,
        LIST_IMAGES_STATED_FILTER_NAMES,
    )];
    for (case, set, stated) in rows {
        let mut sorted_set: Vec<&str> = set.to_vec();
        sorted_set.sort_unstable();
        let mut sorted_stated: Vec<&str> = stated.to_vec();
        sorted_stated.sort_unstable();
        assert_eq!(
            sorted_stated, sorted_set,
            "case {case:?}: expected the stated names to be the set it filters on \
             ({sorted_set:?}), got {sorted_stated:?}"
        );
    }
}
