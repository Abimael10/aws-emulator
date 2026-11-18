// SPDX-License-Identifier: CC0-1.0

//! `ListControlMappings`: how a FRAMEWORK mapping names a compliance framework, and how
//! much of a framework cross-walk a control reports.
//!
//! Two rules are pinned here, over the whole catalog rather than one control:
//!
//! 1. a FRAMEWORK mapping names the framework by the compact designator the catalog
//!    publishes it under (`NIST-CSF-v1.1`) — not by the framework's prose title —
//!    paired with that framework's own identifier for the requirement;
//! 2. the requirements a control satisfies follow from the control OBJECTIVE it
//!    implements, so every control implementing one objective reports that objective's
//!    whole cross-walk, and a framework that restates another framework's requirements
//!    reports them under the identifier it restates;
//! 3. a designator is the framework's abbreviation followed by the EDITION the catalog
//!    reads it at, where it names one — a version, a revision, a level, the part of a
//!    wider standard or a dated edition — the catalog cross-walks a requirement into
//!    every edition of a framework it publishes, and the item beside a designator is
//!    numbered as THAT edition numbers its requirements.
//!
//! Each test is a TABLE whose rows are the controls the catalog publishes and the
//! frameworks it cross-walks into, read back over the wire through the same
//! `MultiServiceRouter` front door `control_catalog.rs` drives.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_controlcatalog_controlcatalog::state::{
    ComAmazonawsControlcatalogControlcatalogState as Catalog, PUBLISHED_FRAMEWORKS,
};

use crate::control_catalog::{drain, Emulator};

/// The route `ListControlMappings` is bound to and the member carrying its items.
const MAPPINGS_PATH: &str = "/list-control-mappings";
const MAPPINGS_KEY: &str = "ControlMappings";

/// `FrameworkName`'s and `FrameworkItem`'s modeled `@length` bounds
/// (`smithy_query(query="shape", shape="FrameworkMappingDetails")`).
const FRAMEWORK_NAME_BOUNDS: (usize, usize) = (3, 250);

/// The framework designators that restate another framework's or another edition's
/// requirements, with the designator they restate: a FedRAMP baseline is composed of the
/// NIST SP 800-53 controls of the revision it is read at, under the identifier that
/// publication gives them, a `CMMC-Level2` practice is an `NIST-SP-800-171-r2`
/// requirement renamed after its family, the 2013 edition of ISO/IEC 27001 states its
/// Annex A controls under the numbering the 2022 edition replaced, and a dated edition of
/// SOC 2 restates the trust services criteria under the same criterion identifier.
const NIST_SP_800_53_R5: &str = "NIST-SP-800-53-r5";
const NIST_SP_800_171_R2: &str = "NIST-SP-800-171-r2";
const FEDRAMP_R4: &str = "FedRAMP-r4";
const FEDRAMP_R5: &str = "FedRAMP-r5";
const CMMC_LEVEL_2: &str = "CMMC-Level2";
const ISO_IEC_27001: &str = "ISO-IEC-27001";
const ISO_IEC_27001_2022_ANNEX_A: &str = "ISO-IEC-27001:2022-Annex-A";
const SSAE_18_SOC_2: &str = "SSAE-18-SOC-2";
const SSAE_18_SOC_2_OCT_2023: &str = "SSAE-18-SOC-2-Oct-2023";

/// The NIST SP 800-53 control identifiers revision 4 of the publication carries under
/// the same designation as revision 5, so a FedRAMP revision-4 baseline can be composed
/// of them. Written out here so the restriction the service applies is checked against
/// an independent list: revision 5 introduced families revision 4 has none of, so a
/// revision-4 baseline cannot name every revision-5 control.
const CONTROLS_IN_800_53_R4: [&str; 14] = [
    "AC-3", "AC-6", "AU-2", "AU-6", "AU-9", "AU-11", "AU-12", "CM-8", "IA-2", "IA-5",
    "SC-7", "SC-8", "SC-28", "SI-4",
];

/// The ISO/IEC 27001:2013 Annex A controls each 2022 Annex A clause the catalog
/// cross-walks into consolidated, in the correspondence published with the 2022
/// edition. Written out here so the derivation the service performs is checked against
/// an independent table, not against itself; a clause the 2022 edition introduced
/// corresponds to no 2013 control and so carries an empty list.
const ANNEX_A_2013_CORRESPONDENCE: [(&str, &[&str]); 9] = [
    ("5.9", &["A.8.1.1", "A.8.1.2"]),
    ("5.15", &["A.9.1.1", "A.9.4.1"]),
    ("5.17", &["A.9.2.4", "A.9.3.1", "A.9.4.3"]),
    ("5.33", &["A.18.1.3"]),
    ("8.2", &["A.9.2.3"]),
    ("8.15", &["A.12.4.1", "A.12.4.2", "A.12.4.3"]),
    ("8.16", &[]),
    ("8.20", &["A.13.1.1"]),
    ("8.24", &["A.10.1.1", "A.10.1.2"]),
];

/// The family abbreviation of each `NIST-SP-800-171-r2` section, in the order the
/// publication lists its requirement families. Written out here so the derivation the
/// service performs is checked against an independent table, not against itself.
const NIST_800_171_FAMILIES: [(&str, &str); 14] = [
    ("3.1", "AC"),
    ("3.2", "AT"),
    ("3.3", "AU"),
    ("3.4", "CM"),
    ("3.5", "IA"),
    ("3.6", "IR"),
    ("3.7", "MA"),
    ("3.8", "MP"),
    ("3.9", "PS"),
    ("3.10", "PE"),
    ("3.11", "RA"),
    ("3.12", "CA"),
    ("3.13", "SC"),
    ("3.14", "SI"),
];

/// The `CMMC-Level2` practice identifier that restates an 800-171 r2 requirement.
fn cmmc_practice(requirement: &str) -> String {
    let mut parts = requirement.split('.');
    let section = format!(
        "{}.{}",
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default()
    );
    let family = NIST_800_171_FAMILIES
        .iter()
        .find(|(candidate, _)| *candidate == section)
        .map(|(_, family)| *family)
        .unwrap_or_else(|| {
            panic!("case requirement {requirement}: no 800-171 family owns section {section}")
        });
    format!("{family}.L2-{requirement}")
}

/// Every `(framework, item)` pair a control reports, read over the wire.
async fn framework_items(emulator: &Emulator, control_arn: &str) -> BTreeSet<(String, String)> {
    let body = format!(r#"{{"Filter":{{"ControlArns":["{control_arn}"]}}}}"#);
    drain(emulator, MAPPINGS_PATH, MAPPINGS_KEY, None, &body)
        .await
        .iter()
        .filter(|mapping| mapping["MappingType"] == "FRAMEWORK")
        .map(|mapping| {
            let framework = &mapping["Mapping"]["Framework"];
            (
                framework["Name"].as_str().unwrap_or_default().to_string(),
                framework["Item"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Every FRAMEWORK mapping names a published framework by its designator, and every
/// control is cross-walked into several of them.
#[tokio::test]
async fn a_framework_mapping_names_a_published_framework_by_its_designator() {
    let emulator = Emulator::new();
    let catalog = Catalog::default();

    // One row per published control: whatever the control's surface or behavior, the
    // mapping set it reports has to name frameworks the catalog publishes, in the
    // designator form, and has to span more than a framework or two — a compliance
    // cross-walk that stopped at one framework would report no coverage anywhere else.
    let mut names_seen: BTreeSet<String> = BTreeSet::new();
    for control in catalog.controls.values() {
        let items = framework_items(&emulator, &control.arn).await;
        let frameworks: BTreeSet<&str> = items
            .iter()
            .map(|(framework, _)| framework.as_str())
            .collect();
        assert!(
            frameworks.len() >= 4,
            "case control {}: expected items in at least four published frameworks, got \
             {frameworks:?}",
            control.arn
        );
        for (framework, item) in &items {
            assert!(
                PUBLISHED_FRAMEWORKS.contains(&framework.as_str()),
                "case control {} framework {framework}: not a framework the catalog publishes \
                 ({PUBLISHED_FRAMEWORKS:?})",
                control.arn
            );
            // The designator is one token: an abbreviation and the version or revision
            // it is read at. A prose title (which carries spaces) is a different
            // surface's wording of the same framework and cannot be joined on.
            assert!(
                !framework.contains(char::is_whitespace),
                "case control {} framework {framework}: a designator carries no whitespace",
                control.arn
            );
            assert!(
                framework
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || ['-', '.', '_', '+', ':'].contains(&character)),
                "case control {} framework {framework}: a designator is alphanumeric with \
                 separators",
                control.arn
            );
            let (min, max) = FRAMEWORK_NAME_BOUNDS;
            assert!(
                (min..=max).contains(&framework.chars().count()),
                "case control {} framework {framework}: outside FrameworkName's modeled \
                 length {min}..{max}",
                control.arn
            );
            assert!(
                (min..=max).contains(&item.chars().count()),
                "case control {} item {item}: outside FrameworkItem's modeled length \
                 {min}..{max}",
                control.arn
            );
            names_seen.insert(framework.clone());
        }
    }

    // Every designator the catalog publishes is reachable: a framework nothing maps to
    // would be a designator a client can never join on.
    for framework in PUBLISHED_FRAMEWORKS {
        assert!(
            names_seen.contains(*framework),
            "case framework {framework}: published but named by no control mapping, saw \
             {names_seen:?}"
        );
    }
}

/// A framework edition that restates another framework's or another edition's
/// requirements reports exactly the restatements of the rows it is derived from.
#[tokio::test]
async fn a_restating_framework_reports_the_requirements_it_restates() {
    let emulator = Emulator::new();
    let catalog = Catalog::default();

    // One row per published control, so the derivation is checked on every control's
    // own requirement set rather than on one hand-picked control.
    for control in catalog.controls.values() {
        let items = framework_items(&emulator, &control.arn).await;
        let of = |wanted: &str| -> BTreeSet<String> {
            items
                .iter()
                .filter(|(framework, _)| framework == wanted)
                .map(|(_, item)| item.clone())
                .collect()
        };

        let nist_800_53 = of(NIST_SP_800_53_R5);
        assert!(
            !nist_800_53.is_empty(),
            "case control {}: expected {NIST_SP_800_53_R5} requirements, got {items:?}",
            control.arn
        );
        // Both FedRAMP baselines the catalog publishes: a baseline is composed of the
        // 800-53 controls of the revision it is read at, and names a control by the
        // identifier that publication gives it — so the revision-5 baseline reports
        // every 800-53 r5 control the control satisfies, and the revision-4 baseline
        // reports those revision 4 publishes too.
        let fedramp_r5 = of(FEDRAMP_R5);
        assert_eq!(
            fedramp_r5, nist_800_53,
            "case control {}: expected {FEDRAMP_R5} to report the {NIST_SP_800_53_R5} \
             controls its baseline is composed of, got {fedramp_r5:?}",
            control.arn
        );
        let expected_r4: BTreeSet<String> = nist_800_53
            .iter()
            .filter(|item| CONTROLS_IN_800_53_R4.contains(&item.as_str()))
            .cloned()
            .collect();
        let fedramp_r4 = of(FEDRAMP_R4);
        assert_eq!(
            fedramp_r4, expected_r4,
            "case control {}: expected {FEDRAMP_R4} to report the controls revision 4 \
             publishes out of {nist_800_53:?}, got {fedramp_r4:?}",
            control.arn
        );

        let nist_800_171 = of(NIST_SP_800_171_R2);
        let expected_cmmc: BTreeSet<String> =
            nist_800_171.iter().map(|item| cmmc_practice(item)).collect();
        assert_eq!(
            of(CMMC_LEVEL_2),
            expected_cmmc,
            "case control {}: expected {CMMC_LEVEL_2} to restate the {NIST_SP_800_171_R2} \
             requirements {nist_800_171:?}, got {:?}",
            control.arn,
            of(CMMC_LEVEL_2)
        );

        // The 2013 edition of ISO/IEC 27001 states, under its own Annex A numbering,
        // exactly the controls the 2022 clauses this control satisfies consolidated.
        let annex_a_2022 = of(ISO_IEC_27001_2022_ANNEX_A);
        assert!(
            !annex_a_2022.is_empty(),
            "case control {}: expected {ISO_IEC_27001_2022_ANNEX_A} clauses, got {items:?}",
            control.arn
        );
        let expected_2013: BTreeSet<String> = annex_a_2022
            .iter()
            .flat_map(|clause| {
                ANNEX_A_2013_CORRESPONDENCE
                    .iter()
                    .find(|(candidate, _)| candidate == clause)
                    .map(|(_, controls)| *controls)
                    .unwrap_or_else(|| {
                        panic!(
                            "case control {} clause {clause}: no recorded 2013 \
                             correspondence",
                            control.arn
                        )
                    })
                    .iter()
                    .map(|annex_a| (*annex_a).to_string())
            })
            .collect();
        assert_eq!(
            of(ISO_IEC_27001),
            expected_2013,
            "case control {}: expected {ISO_IEC_27001} to state the 2013 controls the \
             clauses {annex_a_2022:?} consolidated, got {:?}",
            control.arn,
            of(ISO_IEC_27001)
        );

        // The trust services criteria are numbered the same in each edition of SOC 2,
        // so the dated edition reports the same criteria.
        let criteria = of(SSAE_18_SOC_2);
        assert!(
            !criteria.is_empty(),
            "case control {}: expected {SSAE_18_SOC_2} criteria, got {items:?}",
            control.arn
        );
        assert_eq!(
            of(SSAE_18_SOC_2_OCT_2023),
            criteria,
            "case control {}: expected {SSAE_18_SOC_2_OCT_2023} to report the criteria \
             {criteria:?}, got {:?}",
            control.arn,
            of(SSAE_18_SOC_2_OCT_2023)
        );
    }
}

/// Controls that implement one objective report that objective's cross-walk, whichever
/// surface implements them.
#[tokio::test]
async fn controls_implementing_one_objective_share_its_cross_walk() {
    let emulator = Emulator::new();
    let catalog = Catalog::default();

    // Group the published controls by the common control each implements — the
    // objective-level statement of what the control does — reading the grouping off
    // the catalog's own COMMON_CONTROL mappings.
    let mut by_objective: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for control in catalog.controls.values() {
        let body = format!(
            r#"{{"Filter":{{"ControlArns":["{}"],"MappingTypes":["COMMON_CONTROL"]}}}}"#,
            control.arn
        );
        for mapping in drain(&emulator, MAPPINGS_PATH, MAPPINGS_KEY, None, &body).await {
            if let Some(objective) = mapping["Mapping"]["CommonControl"]["CommonControlArn"].as_str()
            {
                by_objective
                    .entry(objective.to_string())
                    .or_default()
                    .push(control.arn.clone());
            }
        }
    }
    assert!(
        by_objective.values().any(|controls| controls.len() > 1),
        "case catalog: expected an objective implemented by more than one control, got \
         {by_objective:?}"
    );

    // One row per objective with more than one control: the requirements they agree on
    // are the objective's own cross-walk, which has to be the bulk of what each
    // reports — a preventive control and the detective control for the same objective
    // cannot disagree about which framework requirements the objective meets.
    for (objective, controls) in &by_objective {
        if controls.len() < 2 {
            continue;
        }
        let mut shared: Option<BTreeSet<(String, String)>> = None;
        for control_arn in controls {
            let items = framework_items(&emulator, control_arn).await;
            shared = Some(match shared {
                None => items,
                Some(shared) => shared.intersection(&items).cloned().collect(),
            });
        }
        let shared = shared.unwrap_or_default();
        let frameworks: BTreeSet<&str> = shared
            .iter()
            .map(|(framework, _)| framework.as_str())
            .collect();
        assert!(
            shared.len() >= 8 && frameworks.len() >= 5,
            "case objective {objective} ({} controls): expected the controls to share the \
             objective's cross-walk, got {} item(s) across {frameworks:?}",
            controls.len(),
            shared.len()
        );
    }
}

/// A spot check on one published control, so a cross-walk that silently emptied is
/// caught even if every assertion above were satisfied vacuously.
///
/// The control is picked by the catalog designator it is published under, and the
/// requirements asserted are the asset-inventory ones: `CM-8` ("system component
/// inventory") in 800-53 r5 and in each FedRAMP baseline that restates it, safeguard
/// `1.1` in CIS v8.0, clause `5.9` of ISO/IEC 27001:2022 Annex A and the 2013 Annex A
/// control `A.8.1.1` ("inventory of assets") it consolidated.
#[tokio::test]
async fn the_asset_inventory_control_reports_its_whole_cross_walk() {
    let emulator = Emulator::new();
    let catalog = Catalog::default();

    let designator = "CONFIG.ACCOUNT.DT.1";
    let control = catalog
        .controls
        .values()
        .find(|control| control.aliases.iter().any(|alias| alias == designator))
        .unwrap_or_else(|| panic!("case {designator}: the catalog publishes this control"));

    let items = framework_items(&emulator, &control.arn).await;
    let expected = [
        (NIST_SP_800_53_R5, "CM-8"),
        (FEDRAMP_R4, "CM-8"),
        (FEDRAMP_R5, "CM-8"),
        ("NIST-CSF-v1.1", "ID.AM-1"),
        ("CIS-v8.0", "1.1"),
        (ISO_IEC_27001_2022_ANNEX_A, "5.9"),
        (ISO_IEC_27001, "A.8.1.1"),
        (NIST_SP_800_171_R2, "3.4.1"),
        (CMMC_LEVEL_2, "CM.L2-3.4.1"),
    ];
    for (framework, item) in expected {
        assert!(
            items.contains(&(framework.to_string(), item.to_string())),
            "case {designator} {framework} {item}: expected the requirement in the control's \
             cross-walk, got {items:?}"
        );
    }
}

// --- the FORM of a designator and of the item beside it ---------------------
//
// Rule 1 above says WHICH name a FRAMEWORK mapping carries; the tests below say what
// that name and the item beside it are MADE of: the framework's abbreviation followed
// by the edition the catalog reads it at, where it names one, and an item in the
// numbering THAT edition uses. Each is a TABLE over EVERY designator the catalog
// publishes, whose rows differ in the form of the edition (`v1.1`, `r5`, `Level2`,
// `Oct-2023`, `2022-Annex-A`, none at all) and in the numbering of their items (control
// identifiers, dotted clause numbers, 2013 Annex A controls, trust services criteria,
// CMMC practices, best-practice identifiers).

/// How one published framework designates its edition and numbers its requirements.
struct FrameworkForm {
    /// The designator the catalog publishes the framework under.
    designator: &'static str,
    /// The framework's abbreviation, which the designator opens with.
    abbreviation: &'static str,
    /// The edition the designator closes with: the version, revision, level, part of a
    /// wider standard or dated edition the catalog reads the framework at. Empty for a
    /// framework the catalog publishes at the framework's own designation, with no
    /// edition token of its own.
    edition: &'static str,
    /// Does `item` carry the numbering this framework uses for a requirement?
    item_is_well_formed: fn(&str) -> bool,
    /// What that numbering is, for the failure message.
    item_form: &'static str,
}

/// A requirement identified by a control family and a number within it (`CM-8`,
/// `AC-12`): the numbering NIST SP 800-53 and the FedRAMP baselines composed of it use.
fn control_identifier(item: &str) -> bool {
    let Some((family, number)) = item.split_once('-') else {
        return false;
    };
    family.len() == 2
        && family.chars().all(|character| character.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|character| character.is_ascii_digit())
}

/// A requirement identified by a dotted number (`8.22`, `11.5.1`, `3.14.6`): the
/// numbering a clause- or section-numbered publication uses.
fn dotted_number(item: &str) -> bool {
    let sections: Vec<&str> = item.split('.').collect();
    sections.len() >= 2
        && sections.iter().all(|section| {
            !section.is_empty() && section.chars().all(|character| character.is_ascii_digit())
        })
}

/// A requirement identified as an ISO/IEC 27001:2013 Annex A control (`A.9.4.1`): the
/// annex letter and the clause number, which is how the 2013 edition numbers them.
fn annex_a_2013_control(item: &str) -> bool {
    item.strip_prefix("A.").is_some_and(dotted_number)
}

/// A requirement identified by a function and category code and a number within it
/// (`PR.AC-4`, `DE.CM-01.02`): the numbering the NIST Cybersecurity Framework and the
/// CRI Profile use.
fn category_identifier(item: &str) -> bool {
    let Some((category, number)) = item.split_once('-') else {
        return false;
    };
    let Some((function, area)) = category.split_once('.') else {
        return false;
    };
    !function.is_empty()
        && !area.is_empty()
        && function
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && area.chars().all(|character| character.is_ascii_uppercase())
        && !number.is_empty()
        && number
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
}

/// A CMMC practice: the 800-171 requirement it restates, renamed after that
/// requirement's family and the level it is required at (`CM.L2-3.4.2`).
fn is_cmmc_practice(item: &str) -> bool {
    let Some((family, requirement)) = item.split_once("L2-") else {
        return false;
    };
    family.len() == 3
        && family.ends_with('.')
        && family[..2]
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && dotted_number(requirement)
}

/// The trust services criteria categories SOC 2 defines: the common criteria (`CC`) and
/// one category per trust services category other than security — availability (`A1`),
/// confidentiality (`C1`), processing integrity (`PI1`) and privacy (`P1`..`P8`).
const TRUST_SERVICES_CATEGORIES: [&str; 12] = [
    "CC", "A1", "C1", "PI1", "P1", "P2", "P3", "P4", "P5", "P6", "P7", "P8",
];

/// A trust services criterion: one of the categories SOC 2 defines and a number within
/// it (`CC6.1`, `A1.2`, `C1.1`, `PI1.4`, `P2.1`).
fn trust_services_criterion(item: &str) -> bool {
    TRUST_SERVICES_CATEGORIES.iter().any(|category| {
        item.strip_prefix(category).is_some_and(|rest| {
            if *category == "CC" {
                // The common criteria are numbered by criteria series and number
                // within it (CC6.1); every other category carries its number in the
                // category token itself, so only the criterion number follows.
                dotted_number(rest)
            } else {
                rest.strip_prefix('.').is_some_and(|number| {
                    !number.is_empty()
                        && number.chars().all(|character| character.is_ascii_digit())
                })
            }
        })
    })
}

/// A Well-Architected best practice: the pillar question it answers and the practice
/// within that question (`SEC08-BP04`).
fn best_practice_identifier(item: &str) -> bool {
    let Some((question, practice)) = item.split_once("-BP") else {
        return false;
    };
    question.len() == 5
        && question[..3]
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && question[3..]
            .chars()
            .all(|character| character.is_ascii_digit())
        && !practice.is_empty()
        && practice.chars().all(|character| character.is_ascii_digit())
}

/// Every framework the catalog cross-walks its controls into, with the form its
/// designator and its items carry. Source: each framework's own publication, named in
/// the comment beside its row; the set itself is the catalog's published list
/// (`state::PUBLISHED_FRAMEWORKS`), which the first test below compares against.
const FRAMEWORK_FORMS: &[FrameworkForm] = &[
    // Security and Privacy Controls for Information Systems and Organizations,
    // revision 5: a requirement is a control in a two-letter family.
    FrameworkForm {
        designator: "NIST-SP-800-53-r5",
        abbreviation: "NIST-SP-800-53",
        edition: "r5",
        item_is_well_formed: control_identifier,
        item_form: "a control identifier such as CM-8",
    },
    // Protecting Controlled Unclassified Information, revision 2: a requirement is
    // numbered 3.<section>.<n>.
    FrameworkForm {
        designator: "NIST-SP-800-171-r2",
        abbreviation: "NIST-SP-800-171",
        edition: "r2",
        item_is_well_formed: dotted_number,
        item_form: "a requirement number such as 3.4.1",
    },
    // The Cybersecurity Framework, version 1.1: a subcategory is numbered
    // <function>.<category>-<n>.
    FrameworkForm {
        designator: "NIST-CSF-v1.1",
        abbreviation: "NIST-CSF",
        edition: "v1.1",
        item_is_well_formed: category_identifier,
        item_form: "a subcategory such as PR.AC-4",
    },
    // The Payment Card Industry Data Security Standard, version 4.0: a requirement is
    // numbered <requirement>.<sub>.<n>.
    FrameworkForm {
        designator: "PCI-DSS-v4.0",
        abbreviation: "PCI-DSS",
        edition: "v4.0",
        item_is_well_formed: dotted_number,
        item_form: "a requirement number such as 8.2.1",
    },
    // The CIS Amazon Web Services Foundations Benchmark, version 3.0: a recommendation
    // is numbered <section>.<n>.
    FrameworkForm {
        designator: "CIS-AWS-Benchmark-v3.0",
        abbreviation: "CIS-AWS-Benchmark",
        edition: "v3.0",
        item_is_well_formed: dotted_number,
        item_form: "a recommendation number such as 3.1",
    },
    // The CIS Critical Security Controls, version 8.0: a safeguard is numbered
    // <control>.<n>.
    FrameworkForm {
        designator: "CIS-v8.0",
        abbreviation: "CIS",
        edition: "v8.0",
        item_is_well_formed: dotted_number,
        item_form: "a safeguard number such as 3.3",
    },
    // The CIS Google Cloud Platform Foundation Benchmark, version 2.0.
    FrameworkForm {
        designator: "CIS-GCP-Benchmark-v2.0",
        abbreviation: "CIS-GCP-Benchmark",
        edition: "v2.0",
        item_is_well_formed: dotted_number,
        item_form: "a recommendation number such as 5.2",
    },
    // A FedRAMP baseline, revision 4: it is composed of NIST SP 800-53 controls and
    // names each by the control identifier the publication gives it.
    FrameworkForm {
        designator: "FedRAMP-r4",
        abbreviation: "FedRAMP",
        edition: "r4",
        item_is_well_formed: control_identifier,
        item_form: "a control identifier such as AC-12",
    },
    // A FedRAMP baseline, revision 5: the catalog publishes both revisions, and a
    // control identifier is the same in each.
    FrameworkForm {
        designator: "FedRAMP-r5",
        abbreviation: "FedRAMP",
        edition: "r5",
        item_is_well_formed: control_identifier,
        item_form: "a control identifier such as SC-7",
    },
    // ISO/IEC 27001:2013 read at Annex A: that edition numbers its Annex A controls
    // with the annex letter and a clause number (A.9.4.1).
    FrameworkForm {
        designator: "ISO-IEC-27001",
        abbreviation: "ISO-IEC-27001",
        edition: "",
        item_is_well_formed: annex_a_2013_control,
        item_form: "a 2013 Annex A control such as A.9.4.1",
    },
    // ISO/IEC 27001:2022 read at Annex A: the designator names the edition and the
    // annex, and that edition renumbered its Annex A clauses (8.22), so an item carries
    // no annex letter.
    FrameworkForm {
        designator: "ISO-IEC-27001:2022-Annex-A",
        abbreviation: "ISO-IEC-27001",
        edition: "2022-Annex-A",
        item_is_well_formed: dotted_number,
        item_form: "a 2022 Annex A clause number such as 8.22, carrying no annex letter",
    },
    // The Cyber Risk Institute Profile, version 2.2: a diagnostic statement is numbered
    // <function>.<category>-<n>.<n>.
    FrameworkForm {
        designator: "CRI-Profile-v2.2",
        abbreviation: "CRI-Profile",
        edition: "v2.2",
        item_is_well_formed: category_identifier,
        item_form: "a diagnostic statement such as PR.IR-01.03",
    },
    // The Cybersecurity Maturity Model Certification at level 2: a practice is an
    // 800-171 requirement renamed after its family and that level.
    FrameworkForm {
        designator: "CMMC-Level2",
        abbreviation: "CMMC",
        edition: "Level2",
        item_is_well_formed: is_cmmc_practice,
        item_form: "a practice such as CM.L2-3.4.2",
    },
    // The SOC 2 trust services criteria as attested under SSAE 18: the criteria are
    // numbered by category and number within it, in every edition.
    FrameworkForm {
        designator: "SSAE-18-SOC-2",
        abbreviation: "SSAE-18-SOC-2",
        edition: "",
        item_is_well_formed: trust_services_criterion,
        item_form: "a trust services criterion such as CC6.1",
    },
    // The same criteria in the edition of October 2023: this framework issues dated
    // editions rather than numbered ones, so the designator carries the date.
    FrameworkForm {
        designator: "SSAE-18-SOC-2-Oct-2023",
        abbreviation: "SSAE-18-SOC-2",
        edition: "Oct-2023",
        item_is_well_formed: trust_services_criterion,
        item_form: "a trust services criterion such as CC6.1",
    },
    // The Amazon Web Services Well-Architected Framework, version 10: a best practice
    // is numbered <pillar><question>-BP<n>.
    FrameworkForm {
        designator: "AWS-WAF-v10",
        abbreviation: "AWS-WAF",
        edition: "v10",
        item_is_well_formed: best_practice_identifier,
        item_form: "a best practice such as SEC08-BP04",
    },
];

/// Every `(framework, item)` pair the catalog serves, read over the wire through the
/// FRAMEWORK filter arm, grouped by framework.
async fn served_framework_items(emulator: &Emulator) -> BTreeMap<String, BTreeSet<String>> {
    let body = r#"{"Filter":{"MappingTypes":["FRAMEWORK"]}}"#;
    let mut served: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for mapping in drain(emulator, MAPPINGS_PATH, MAPPINGS_KEY, None, body).await {
        let framework = &mapping["Mapping"]["Framework"];
        let name = framework["Name"].as_str().unwrap_or_default().to_string();
        let item = framework["Item"].as_str().unwrap_or_default().to_string();
        served.entry(name).or_default().insert(item);
    }
    served
}

/// The designator the catalog publishes a framework under opens with the framework's
/// abbreviation and closes with the edition it is read at, where it names one.
#[test]
fn a_published_designator_names_the_framework_and_the_edition_it_is_read_at() {
    // One row per published framework, so the form is checked on every designator the
    // catalog publishes rather than only on the ones a client happened to ask for.
    for form in FRAMEWORK_FORMS {
        let FrameworkForm {
            designator,
            abbreviation,
            edition,
            ..
        } = form;
        assert!(
            designator.starts_with(abbreviation),
            "case framework {designator}: expected a designator opening with the \
             abbreviation {abbreviation}"
        );
        if edition.is_empty() {
            // A framework the catalog publishes at the framework's own designation:
            // the designator is that designation, with nothing appended.
            assert_eq!(
                designator, abbreviation,
                "case framework {designator}: expected the framework's own designation \
                 {abbreviation} and no edition token"
            );
        } else {
            assert!(
                designator.ends_with(edition),
                "case framework {designator}: expected a designator closing with the \
                 edition {edition}"
            );
            assert!(
                designator.len() > abbreviation.len() + edition.len(),
                "case framework {designator}: expected the abbreviation {abbreviation} \
                 and the edition {edition} to be joined by a separator"
            );
            let separators = designator.len() - abbreviation.len() - edition.len();
            assert_eq!(
                separators, 1,
                "case framework {designator}: expected the abbreviation and the edition \
                 {edition} to be joined by one separator, got {separators}"
            );
            assert!(
                edition.chars().any(|character| character.is_ascii_digit()),
                "case framework {designator}: expected the edition {edition} to be \
                 identified by a number or a date"
            );
        }
        // A designator is one token a client can join on; a prose title carries
        // whitespace, so it cannot be one.
        assert!(
            !designator.contains(char::is_whitespace),
            "case framework {designator}: a designator carries no whitespace"
        );
    }

    // The table and the catalog's published list are the same set: a framework the
    // catalog publishes but this table does not describe would go unchecked, and one
    // this table describes but the catalog does not publish is named by no mapping.
    let described: BTreeSet<&str> = FRAMEWORK_FORMS.iter().map(|form| form.designator).collect();
    let published: BTreeSet<&str> = PUBLISHED_FRAMEWORKS.iter().copied().collect();
    assert_eq!(
        described.len(),
        FRAMEWORK_FORMS.len(),
        "case catalog: expected one row per designator, so no two rows describe one \
         designator"
    );
    assert_eq!(
        described, published,
        "case catalog: expected the published frameworks to be exactly the ones this \
         table describes"
    );
}

/// Every FRAMEWORK mapping the catalog serves names a published designator and carries
/// an item in that framework's own numbering.
#[tokio::test]
async fn a_framework_mapping_carries_an_item_in_that_framework_s_numbering() {
    let emulator = Emulator::new();
    let served = served_framework_items(&emulator).await;
    assert!(
        !served.is_empty(),
        "case catalog: expected the catalog to serve FRAMEWORK mappings"
    );

    // One row per published framework: every designator is reachable, and every item
    // served under it is numbered the way that framework numbers its requirements.
    for form in FRAMEWORK_FORMS {
        let items = served.get(form.designator).cloned().unwrap_or_default();
        assert!(
            !items.is_empty(),
            "case framework {}: published but named by no control mapping, saw {:?}",
            form.designator,
            served.keys().collect::<Vec<_>>()
        );
        for item in &items {
            assert!(
                (form.item_is_well_formed)(item),
                "case framework {} item {item}: expected {}, got {item:?}",
                form.designator,
                form.item_form
            );
        }
    }

    // Nothing is served under a designator the catalog does not publish.
    for framework in served.keys() {
        assert!(
            PUBLISHED_FRAMEWORKS.contains(&framework.as_str()),
            "case framework {framework}: not a framework the catalog publishes \
             ({PUBLISHED_FRAMEWORKS:?})"
        );
    }
}

/// A framework the catalog publishes at more than one edition reports each edition
/// under its own designator, with items numbered as THAT edition numbers them.
#[tokio::test]
async fn each_published_edition_of_a_framework_carries_its_own_numbering() {
    let emulator = Emulator::new();
    let served = served_framework_items(&emulator).await;

    // Group the published designators by the framework they name: a framework with more
    // than one row is one the catalog cross-walks at more than one edition.
    let mut by_framework: BTreeMap<&str, Vec<&FrameworkForm>> = BTreeMap::new();
    for form in FRAMEWORK_FORMS {
        by_framework
            .entry(form.abbreviation)
            .or_default()
            .push(form);
    }
    let multi: Vec<(&&str, &Vec<&FrameworkForm>)> = by_framework
        .iter()
        .filter(|(_, forms)| forms.len() > 1)
        .collect();
    assert!(
        !multi.is_empty(),
        "case catalog: expected a framework published at more than one edition, got \
         {:?}",
        by_framework.keys().collect::<Vec<_>>()
    );

    // One row per edition of each such framework: every edition is served, and every
    // item under it is numbered the way that edition numbers its requirements.
    for (framework, forms) in multi {
        for form in forms {
            let items = served.get(form.designator).cloned().unwrap_or_default();
            assert!(
                !items.is_empty(),
                "case framework {framework} edition {}: published but named by no \
                 control mapping, saw {:?}",
                form.designator,
                served.keys().collect::<Vec<_>>()
            );
            for item in &items {
                assert!(
                    (form.item_is_well_formed)(item),
                    "case framework {framework} edition {} item {item}: expected {}",
                    form.designator,
                    form.item_form
                );
            }
        }
    }

    // The editions whose numberings DIFFER cannot share an item: ISO/IEC 27001 numbers
    // its Annex A controls A.9.4.1 in the 2013 edition and renumbered its Annex A
    // clauses 8.22 in the 2022 edition, so an item of one is never an item of the
    // other. Where two editions number a requirement identically (the SOC 2 trust
    // services criteria, a FedRAMP control identifier) they SHARE the item, which is
    // what the restatement test above pins.
    let renumbered: [(&str, &str); 1] = [("ISO-IEC-27001", "ISO-IEC-27001:2022-Annex-A")];
    for (earlier, later) in renumbered {
        let earlier_items = served.get(earlier).cloned().unwrap_or_default();
        let later_items = served.get(later).cloned().unwrap_or_default();
        assert!(
            !earlier_items.is_empty() && !later_items.is_empty(),
            "case editions {earlier} / {later}: expected both editions to be served, got \
             {} and {} item(s)",
            earlier_items.len(),
            later_items.len()
        );
        let shared: BTreeSet<&String> = earlier_items.intersection(&later_items).collect();
        assert!(
            shared.is_empty(),
            "case editions {earlier} / {later}: the later edition renumbered its \
             requirements, so no item belongs to both, got {shared:?}"
        );
    }
}
