// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a request the framework refused before any handler ran.
//!
//! The framework hands the refusal over structurally — a list of
//! [`aws_smithy_schema_server::Violation`]s, each naming the member, what it failed and
//! what it held — and leaves the sentence to the service being emulated
//! (`smithy_server_runtime::Refusal`). CodeBuild does not state every one of them the
//! same way: a few constraint classes have a sentence of CodeBuild's OWN, worded per
//! RULE and built from the request, and everything else keeps the shared constraint
//! envelope that names the member and the constraint it failed.
//!
//! One module so that the choice is made in one place for every operation of the
//! service: each sentence below is keyed to the rule that was broken — the member path the
//! model gives it and the constraint trait the model puts on it — never to one recorded
//! request. One rule needs the OPERATION as well, because the member path alone does not
//! identify the member: a list of identifiers left empty is refused with a sentence naming
//! the kind of identifier the list holds, and two inputs carry a list of different kinds
//! under one member name. The framework hands this seam no request, so which operation is
//! being served is read from the scope the service opens around the walk
//! ([`super::served_operation`]), and the sentence is keyed to the MEMBER of that
//! operation's input — still the model, never one request.

use aws_smithy_schema::{Schema, ShapeType};
use aws_smithy_schema_server::constraints::{LengthTrait, PatternTrait, RangeTrait};
use aws_smithy_schema_server::{Violation, ViolationKind};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// The whole message for a request refused by the constraint walk, as CodeBuild words
/// it.
///
/// A violation of a rule CodeBuild states itself is the WHOLE answer whenever one is
/// among the violations found — the service refuses the rule that was broken rather than
/// reporting a constraint envelope beside it — and the first such violation in the
/// walk's own order decides the sentence. Everything else is the shared envelope, with
/// each clause rendered as this service renders it ([`stated_clause`]).
pub(super) fn message(violations: &[Violation]) -> String {
    if let Some(stated) = violations.iter().find_map(codebuild_sentence) {
        return stated;
    }
    let stated: Vec<Violation> = violations.iter().map(stated_clause).collect();
    aws_smithy_schema_server::envelope(&stated).unwrap_or_else(|| "Invalid input".to_string())
}

/// The sentence CodeBuild states for `violation`, when that violation is of a rule it
/// words itself rather than through the constraint envelope.
fn codebuild_sentence(violation: &Violation) -> Option<String> {
    let path = member_path_of(&violation.path);
    match &violation.kind {
        // A request that carries no project identifier at all on an operation whose input
        // addresses ONE project: CodeBuild states what that member takes — a project's name
        // or its ARN — rather than the constraint envelope's "must not be null", which says
        // nothing about the two forms the value may take.
        ViolationKind::MissingRequiredMember if path == PROJECT_IDENTIFIER_PATH => {
            Some(PROJECT_IDENTIFIER_REQUIRED_MESSAGE.to_string())
        }
        // A sort order outside its enum: CodeBuild's own sentence, so no caller is told
        // its list request failed generic request validation.
        ViolationKind::EnumValue(_) if is_sort_order(violation) => {
            Some(INVALID_SORT_ORDER_MESSAGE.to_string())
        }
        // A sort KEY outside its enum: CodeBuild names the member and quotes the value
        // back, which is the one sentence that serves every criterion a request may ask to
        // be sorted by.
        ViolationKind::EnumValue(_) if path == SORT_KEY_PATH => violation
            .value
            .as_deref()
            .map(|submitted| invalid_sort_key_message(&path, submitted)),
        // An input list of resource identifiers the request left EMPTY, or omitted
        // altogether: both are a request that names nothing to act on, which CodeBuild
        // states in one sentence naming the KIND of identifier the list holds. A project
        // NAME that is itself empty is the sibling rule: an identifier of no resource.
        ViolationKind::MissingRequiredMember | ViolationKind::MinLength(_) => {
            empty_identifier_list_sentence(violation, &path)
                .or_else(|| empty_project_name_sentence(violation, &path))
                .map(str::to_string)
        }
        // An input list of resource identifiers longer than the model lets a request
        // carry.
        ViolationKind::MaxLength(max) if IDENTIFIER_LIST_PATHS.contains(&path.as_str()) => {
            collection_size(violation).map(|size| {
                format!(
                    "The maximum length of input resource list is {max} but input list \
                     with size {size} is found."
                )
            })
        }
        // A build timeout outside the window the model pins on it.
        ViolationKind::MinValue(_) | ViolationKind::MaxValue(_) => {
            build_timeout_windows().get(&path).map(stated_window)
        }
        // A resource NAME carrying a character the model's `@pattern` for that kind of
        // name does not allow: CodeBuild states the character set itself. Keyed to the
        // PATTERN rather than to a member name, so a project's name is refused the same
        // way wherever the model constrains it with it — `CreateProject$name`, and the
        // `projectName` the webhook operations carry.
        ViolationKind::Pattern(pattern) => {
            unsupported_name_character_sentence(pattern, violation.value.as_deref())
        }
        _ => None,
    }
}

/// The sentence CodeBuild states for a NAME the constraint walk refused against
/// `pattern`, when the value broke that pattern's CHARACTER clause.
///
/// `None` when the pattern is not one of the names CodeBuild words itself
/// ([`name_character_sentences`]), and `None` when the value broke another clause of the
/// same pattern — one the sentence below would not describe, so it keeps the shared
/// constraint envelope:
///
/// * every character it carries IS one the name's character set allows (so what it broke
///   is the LENGTH or the leading-character clause);
/// * it is LONGER than the maximum the model gives that name. Its characters are then not
///   even readable here: the walk hands a refusal the value already elided for rendering
///   (`Violation::value` is "ALREADY elided"), and an elision introduces characters of
///   its own — so a name written entirely with supported characters would otherwise be
///   refused for carrying unsupported ones. Within the modelled maximum (150 characters
///   for a project, 128 for a fleet) the value reaches this unabridged.
///
/// That is the same split the store's own identifier rules make
/// (`crate::state::project_identifier_violation`), so one name is refused with one
/// sentence however it reaches the service.
///
/// The MEMBER the value came in on is not part of the question: a member the model
/// constrains with a resource's name pattern carries a name of that resource whatever it
/// is called, which is what gives `CreateProject$name` and the webhook operations'
/// `projectName` the same sentence.
fn unsupported_name_character_sentence(pattern: &str, value: Option<&str>) -> Option<String> {
    let value = value?;
    let (longest, stated) = name_character_sentences().get(pattern)?;
    if value.chars().count() > *longest {
        return None;
    }
    if value
        .chars()
        .all(crate::state::is_supported_name_character)
    {
        return None;
    }
    Some((*stated).to_string())
}

/// What CodeBuild states for a name carrying a character its kind's `@pattern` does not
/// allow, with the longest that name may be, keyed by that PATTERN.
///
/// Built once from the schema of the member that NAMES each resource, so nothing here
/// spells a pattern or a bound: a key is whatever the MODEL puts on `CreateProject$name`
/// or `CreateFleet$name`, which covers every member the model constrains with the same
/// pattern — the name an update carries, and any input that grows one — without another
/// edit here, and the length beside it is that member's own `@length` maximum. These are
/// the two resources this service addresses by name, and the two whose sentence is
/// recorded (`crate::state::INVALID_PROJECT_NAME_MESSAGE`,
/// `crate::state::INVALID_FLEET_NAME_MESSAGE`); a pattern that is not a key — one that
/// does not state [`SUPPORTED_NAME_CHARACTER_CLASS`], or whose member the model gives no
/// maximum length — keeps the shared constraint envelope.
fn name_character_sentences() -> &'static BTreeMap<&'static str, (usize, &'static str)> {
    static SENTENCES: LazyLock<BTreeMap<&'static str, (usize, &'static str)>> =
        LazyLock::new(|| {
            /// The resources whose name has a sentence of CodeBuild's own, as the input
            /// that CREATES each one — the schema the pattern and the bound are read from.
            const STATED: [(&::aws_smithy_schema::Schema, &str); 2] = [
                (
                    crate::ops::types::CREATE_PROJECT_INPUT_SCHEMA,
                    crate::state::INVALID_PROJECT_NAME_MESSAGE,
                ),
                (
                    crate::ops::types::CREATE_FLEET_INPUT_SCHEMA,
                    crate::state::INVALID_FLEET_NAME_MESSAGE,
                ),
            ];
            let sentences: BTreeMap<&'static str, (usize, &'static str)> = STATED
                .into_iter()
                .filter_map(|(input, stated)| {
                    let member = name_member(input)?;
                    let pattern = pattern_of(member)?;
                    // Only a pattern that states the character class the shared predicate
                    // implements: the sentence is chosen by the pattern but the clause
                    // that was broken is decided by that predicate, so a name kind whose
                    // pattern admits some OTHER set is left to the shared constraint
                    // envelope rather than classified against the wrong set.
                    if !pattern.contains(SUPPORTED_NAME_CHARACTER_CLASS) {
                        return None;
                    }
                    // The longest that name may be, which is what bounds the value this
                    // sentence may be chosen from — see
                    // [`unsupported_name_character_sentence`].
                    let longest = usize::try_from(max_length_of(member)?).ok()?;
                    Some((pattern, (longest, stated)))
                })
                .collect();
            // Two resources, two patterns: a model in which they collided would silently
            // word one kind's refusal with the other kind's resource name.
            debug_assert_eq!(
                sentences.len(),
                STATED.len(),
                "each name kind CodeBuild words itself needs its own @pattern stating the \
                 supported character class, and a maximum length"
            );
            sentences
        });
    &SENTENCES
}

/// How the model's name patterns write the characters a name may carry — the class
/// `crate::state::is_supported_name_character` implements, as `ProjectName` and
/// `FleetName` spell it inside their own `@pattern`.
const SUPPORTED_NAME_CHARACTER_CLASS: &str = r"[A-Za-z0-9\-_]";

/// The member of `input` that carries the resource's own name, if it has one.
fn name_member(input: &'static Schema) -> Option<&'static Schema> {
    input
        .members()
        .iter()
        .find(|member| member.member_name() == Some(NAME_MEMBER))
        .copied()
}

/// The `@pattern` the model attaches to `member`, if it puts one on it.
fn pattern_of(member: &Schema) -> Option<&'static str> {
    let carried = member.traits()?.get(&PatternTrait::TRAIT_ID)?;
    carried
        .as_any()
        .downcast_ref::<PatternTrait>()
        .map(|pattern| pattern.regex)
}

/// The `@length` the model attaches to `member`, if it bounds its length at all.
fn length_of(member: &Schema) -> Option<LengthTrait> {
    let carried = member.traits()?.get(&LengthTrait::TRAIT_ID)?;
    carried.as_any().downcast_ref::<LengthTrait>().copied()
}

/// The maximum the model's `@length` allows `member` to be, if it bounds it above.
fn max_length_of(member: &Schema) -> Option<i64> {
    length_of(member)?.max
}

/// The name the model gives the member that carries a resource's own name on the
/// operation that CREATES it — the member the pattern and the length bound above are read
/// from.
const NAME_MEMBER: &str = "name";

/// What CodeBuild states as the reason a request addressing ONE project carries no project
/// identifier, carried by the modelled `InvalidInputException`.
///
/// Fixed text, and it names the two FORMS the member takes rather than the member itself:
/// those members are typed as plain strings precisely because either a project's name or
/// its ARN is a legal value there (`crate::state::ProjectReference` reads both). Recorded
/// behaviour — the model states no message for the error.
const PROJECT_IDENTIFIER_REQUIRED_MESSAGE: &str = "Project name or ARN is required";

/// The validation path of the input member that carries ONE project's identifier.
///
/// Every operation of this service that addresses a single project by a plain-string
/// identifier names that member `projectName` — the per-project listings, the
/// build-starting operations, the cache invalidation and the webhook operations — so a
/// missing-member violation at this path is a request that named no project. The members
/// called `name` are NOT this path: `name` carries the project's own name on the operations
/// that create or change one, and the same spelling carries a FLEET's and a report group's.
const PROJECT_IDENTIFIER_PATH: &str = "projectName";

/// What CodeBuild states as the reason a list request asked for a sort order it does
/// not define, carried by the modelled `InvalidInputException`.
///
/// The sentence is FIXED: it quotes neither the rejected value, the member path nor the
/// permitted set, so it is the same on every request and every operation that takes a
/// sort order. Recorded behaviour — the model states no message for the error.
const INVALID_SORT_ORDER_MESSAGE: &str = "Invalid sort order specified";

/// What CodeBuild states as the reason a batch request names no build to act on,
/// carried by the modelled `InvalidInputException`.
///
/// Fixed text that names the KIND of list the request left empty rather than the member
/// path or the size it carried, so the sentence is the same for every such request.
/// Recorded behaviour — the model states no message for the error.
const EMPTY_BUILD_ID_LIST_MESSAGE: &str = "The input build ID list cannot be empty.";

/// What CodeBuild states as the reason a batch request names no project to act on, worded
/// the same way and for the same reason as [`EMPTY_BUILD_ID_LIST_MESSAGE`]: fixed text
/// naming the KIND of list, with nothing of the request interpolated. Recorded behaviour.
const EMPTY_PROJECT_NAME_LIST_MESSAGE: &str = "The input project name list cannot be empty.";

/// The sentence CodeBuild states for an input list of resource identifiers a request left
/// empty or omitted, keyed by the INPUT SHAPE the list belongs to and the member that
/// carries it — the two halves of that member's own shape id
/// (`com.amazonaws.codebuild#BatchGetProjectsInput$names`).
///
/// Keyed to the member rather than to the member's NAME, because the name alone does not
/// say what kind of identifier the list holds: `names` is a list of project names on one
/// operation and of fleet names on another, and a member schema carries its target's kind
/// but not its target's name (`super::served_operation` says how the input in hand is
/// known). Each entry is a list the model bounds below at one
/// (`smithy_query(query="shape",shape="BuildIds,ProjectNames")`, both `@length(min: 1)`),
/// which `every_stated_empty_list_is_a_list_the_model_bounds_below` pins — so an entry
/// cannot claim a sentence for a list the model lets a request leave empty.
///
/// The lists whose sentence is NOT recorded — the fleet names, the report and report-group
/// ARNs, the command-execution ids — keep the shared constraint envelope naming the member
/// and the constraint it failed: CodeBuild words those refusals somewhere, but not in words
/// this emulator has seen, and a sentence guessed for one list is a sentence no client
/// matches.
const EMPTY_IDENTIFIER_LIST_SENTENCES: [(&str, &str, &str); 3] = [
    (
        "BatchDeleteBuildsInput",
        BUILD_ID_LIST_MEMBER,
        EMPTY_BUILD_ID_LIST_MESSAGE,
    ),
    (
        "BatchGetBuildsInput",
        BUILD_ID_LIST_MEMBER,
        EMPTY_BUILD_ID_LIST_MESSAGE,
    ),
    (
        "BatchGetProjectsInput",
        "names",
        EMPTY_PROJECT_NAME_LIST_MESSAGE,
    ),
];

/// The name the model gives the member that carries a list of BUILD ids, on both operations
/// that take one (`BatchGetBuilds` and `BatchDeleteBuilds`, each targeting `BuildIds`).
const BUILD_ID_LIST_MEMBER: &str = "ids";

/// What CodeBuild states as the reason a value handed to it where a project NAME belongs is
/// empty, carried by the modelled `InvalidInputException`.
///
/// Fixed text that names the KIND of value that was empty rather than the member path or the
/// bound it broke, so the sentence is the same wherever a request carries an empty project
/// name. Recorded behaviour — the model states no message for the error.
const EMPTY_PROJECT_NAME_MESSAGE: &str = "Project name cannot be empty";

/// The members of this service's inputs that carry a project NAME inside a LIST, keyed as
/// [`EMPTY_IDENTIFIER_LIST_SENTENCES`] is: the input shape and the validation path of the
/// member, which for a list's element is the list member followed by the collection step
/// (`names.member`).
///
/// One entry, because `ProjectNames` is the one list of this service's inputs that holds
/// project names; the sibling `FleetNames` holds fleet names, and the element sentence for
/// those is not recorded, so an empty fleet name keeps the shared constraint envelope. The
/// single-project identifier member needs no entry: every operation that addresses ONE
/// project by a plain-string identifier names it [`PROJECT_IDENTIFIER_PATH`], which names a
/// project name on every input that declares it.
const PROJECT_NAME_LIST_MEMBERS: [(&str, &str); 1] = [("BatchGetProjectsInput", "names.member")];

/// The sentence CodeBuild states for a project NAME the request carried as the empty string.
///
/// `None` for every other violation, which keeps the shared constraint envelope: a value that
/// is short rather than empty, a COLLECTION (whose rendered value is its element count, which
/// is the sibling rule above), a member that carries no project name, and the project's own
/// name on the operations that create or change one — `CreateProject$name` carries the model's
/// own `@pattern` and `@length(min: 2)`, whose refusals are stated by the rules keyed to those
/// traits, and nothing records this sentence for them.
fn empty_project_name_sentence(violation: &Violation, path: &str) -> Option<&'static str> {
    if !matches!(violation.kind, ViolationKind::MinLength(_))
        || violation.value.as_deref() != Some("")
    {
        return None;
    }
    if path == PROJECT_IDENTIFIER_PATH {
        return Some(EMPTY_PROJECT_NAME_MESSAGE);
    }
    let input = super::served_operation::served_input()?;
    let shape = input.shape_id().shape_name();
    PROJECT_NAME_LIST_MEMBERS
        .iter()
        .any(|(input_shape, member)| *input_shape == shape && *member == path)
        .then_some(EMPTY_PROJECT_NAME_MESSAGE)
}

/// The sentence CodeBuild states for the identifier list at `path` of the input being
/// served, when that list is one it refuses empty in its own words.
///
/// `None` for every other violation, which keeps the shared constraint envelope: a
/// violation of another kind, a list of identifiers whose sentence is not recorded, a
/// member of another input, and — outside a served request — any violation at all, since
/// which input the path names is then unknown.
fn empty_identifier_list_sentence(violation: &Violation, path: &str) -> Option<&'static str> {
    match &violation.kind {
        // Omitted, or sent as JSON `null`: the deserializer reports the member absent.
        ViolationKind::MissingRequiredMember => {}
        // Sent as a list with nothing in it. The element count is what tells a COLLECTION
        // apart from a string the model bounds below ([`collection_size`]), and zero is what
        // makes the request one that names nothing — a list shorter than a minimum above one
        // is not an empty list and keeps the envelope.
        ViolationKind::MinLength(_) if collection_size(violation) == Some(0) => {}
        _ => return None,
    }
    let input = super::served_operation::served_input()?;
    let shape = input.shape_id().shape_name();
    EMPTY_IDENTIFIER_LIST_SENTENCES
        .iter()
        .find(|(input_shape, member, _)| *input_shape == shape && *member == path)
        .map(|(_, _, stated)| *stated)
}

/// What CodeBuild states as the reason a listing was asked to sort by a criterion it does
/// not define, carried by the modelled `InvalidInputException`.
///
/// The prefix is fixed and names the MEMBER the value arrived on; the trailing token is that
/// value, taken from the request exactly as sent — so a wrong-case spelling of a modelled
/// value is quoted back as the caller wrote it. Recorded behaviour — the model states no
/// message for the error.
fn invalid_sort_key_message(member: &str, submitted: &str) -> String {
    format!("Invalid {member} type specified: {submitted}")
}

/// The validation path of the input member that names the criterion a listing is sorted BY.
///
/// Every operation of this service that takes one names it `sortBy` — the project, fleet,
/// report-group and shared-resource listings, and the code-coverage report read — each
/// targeting an enumerated type of its own. The sentence above names the member rather than
/// the type or the permitted set, so one rule covers all of them; the sibling `sortOrder`,
/// which names the DIRECTION rather than the criterion, has a fixed sentence of its own
/// ([`INVALID_SORT_ORDER_MESSAGE`]).
const SORT_KEY_PATH: &str = "sortBy";

/// The validation paths of the input members that carry a list of RESOURCE IDENTIFIERS —
/// the lists CodeBuild's over-long-list sentence calls an "input resource list".
///
/// Every batch operation of this service takes exactly one such member, and these are
/// their names: `ids` (`BuildIds` on the build read and delete, `BuildBatchIds`,
/// `SandboxIds`), `names` (`ProjectNames`, `FleetNames`), `reportGroupArns`,
/// `reportArns` and `commandExecutionIds`. Keyed to them rather than applied to every
/// collection the model bounds, because this service bounds lists that are NOT identifier
/// lists — a project's `tags` (`TagList`, max 50), its `secondarySources`
/// (`ProjectSources`, max 12) and their siblings — and a request refused for one of those
/// is not a request whose resource list was too long, so it keeps the shared constraint
/// envelope naming the member and the constraint it failed.
const IDENTIFIER_LIST_PATHS: [&str; 5] = [
    "ids",
    "names",
    "reportGroupArns",
    "reportArns",
    "commandExecutionIds",
];

/// `true` when `violation` is a value outside the model's `SortOrderType` value set.
///
/// A violation carries the value set from the schema of the member that failed, so the
/// comparison is against the MODEL rather than against a member name: every operation
/// whose input models a sort order is covered, including the ones whose handlers are
/// still scaffolded, and no other enum of this service declares the same values.
fn is_sort_order(violation: &Violation) -> bool {
    let declared = crate::ops::types::SORT_ORDER_TYPE_VALUES;
    match &violation.kind {
        // Set equality, so the order codegen happens to list an enum's values in is
        // not part of the test.
        ViolationKind::EnumValue(allowed) => {
            allowed.len() == declared.len()
                && allowed
                    .iter()
                    .all(|value| declared.contains(&value.as_str()))
        }
        _ => false,
    }
}

/// The element count a COLLECTION-length violation carries, or `None` for a violation of
/// any other kind of length.
///
/// The constraint walk renders a collection's value as its element count followed by
/// [`ITEMS_SUFFIX`] (`101 items`), where a string's is the string itself — so this is
/// what tells a list that is too long apart from a string that is, and it is what the
/// size CodeBuild states in its own sentence is read from.
fn collection_size(violation: &Violation) -> Option<usize> {
    violation
        .value
        .as_deref()?
        .strip_suffix(ITEMS_SUFFIX)?
        .parse()
        .ok()
}

/// How the constraint walk spells the element count of a collection it refused.
const ITEMS_SUFFIX: &str = " items";

/// The window a build timeout member is held to, as the sentence CodeBuild states for a
/// value outside it.
///
/// Both bounds come from the `@range` the MODEL puts on the member, and the maximum is
/// stated in hours exactly when it is a whole number of them — which is how AWS words
/// the window of `BuildTimeOut` (`@range(min: 5, max: 2160)`): "between 5 minutes and 36
/// hours". Recorded behaviour — the model states no message for the error.
fn stated_window(window: &RangeTrait) -> String {
    format!(
        "Invalid timeout. Should be between {} and {}",
        window.min.map_or_else(|| "0 minutes".to_string(), minutes),
        window.max.map_or_else(|| "0 minutes".to_string(), minutes),
    )
}

/// `minutes` worth of wait, in the unit AWS states it in: whole hours where it is a
/// whole number of them, minutes otherwise.
fn minutes(minutes: f64) -> String {
    let whole = minutes.max(0.0).round() as i64;
    /// Minutes in an hour, for the unit the window above is stated in.
    const MINUTES_PER_HOUR: i64 = 60;
    if whole >= MINUTES_PER_HOUR && whole % MINUTES_PER_HOUR == 0 {
        return format!("{} hours", whole / MINUTES_PER_HOUR);
    }
    format!("{whole} minutes")
}

/// The `@range` window of every input member that states a BUILD's timeout, by
/// validation path.
///
/// Built once from the schemas themselves, so nothing here names a bound: a path is in
/// this map exactly when the MODEL puts a `@range` on a member whose name is one of
/// [`BUILD_TIMEOUT_MEMBERS`], and the window stated is that member's own. The queued
/// timeout is a different wait with a window of its own (`TimeOut`, 5..480) and is not
/// among them.
fn build_timeout_windows() -> &'static BTreeMap<String, RangeTrait> {
    static WINDOWS: LazyLock<BTreeMap<String, RangeTrait>> = LazyLock::new(|| {
        let mut windows = BTreeMap::new();
        for input in super::INPUT_SCHEMAS {
            for member in input.members() {
                let Some(name) = member.member_name() else {
                    continue;
                };
                if !BUILD_TIMEOUT_MEMBERS.contains(&name) {
                    continue;
                }
                if let Some(window) = range_of(member) {
                    windows.insert(
                        aws_smithy_schema_server::constraints::path::member(name),
                        window,
                    );
                }
            }
        }
        windows
    });
    &WINDOWS
}

/// The names the model gives the members that state how long a BUILD may run before
/// CodeBuild times it out — the member of a project's configuration, and the override a
/// start takes for one build only. Both target `BuildTimeOut`, whose window AWS words as
/// "from 5 to 2160 (36 hours)".
const BUILD_TIMEOUT_MEMBERS: [&str; 2] = ["timeoutInMinutes", "timeoutInMinutesOverride"];

/// The `@range` trait codegen attached to `member`, if the model puts one on it.
fn range_of(member: &Schema) -> Option<RangeTrait> {
    let carried = member.traits()?.get(&RangeTrait::TRAIT_ID)?;
    carried.as_any().downcast_ref::<RangeTrait>().copied()
}

/// `violation` with the offending value rendered in the wire form of the member's
/// own modelled type, and an enumerated value set stated in the order CodeBuild
/// states it.
///
/// AWS states a constraint violation as `Value '<value>' at '<member path>' failed
/// to satisfy constraint: …`, and the value it echoes is written the way that
/// member's type is written on the wire: a floating-point member always carries a
/// fractional part (`150` sent for a `double` is stated as `'150.0'`), an integral
/// member never does. The framework renders every numeric leaf with Rust's
/// `to_string`, which prints an integral `f64` as `150`, so this restores the point
/// for exactly the members [`floating_point_member_paths`] reports as floating
/// point. Every other violation — and any value that is not a number — keeps the value
/// the framework worded it with.
fn stated_clause(violation: &Violation) -> Violation {
    let stated = sorted_value_set(violation);
    let Some(shown) = stated.value.as_deref() else {
        return stated;
    };
    if !floating_point_member_paths().contains(&member_path_of(&violation.path)) {
        return stated;
    }
    let Ok(value) = shown.parse::<f64>() else {
        return stated;
    };
    Violation {
        // `{value:?}` is the shortest text that round-trips an `f64` AND always
        // carries a point or an exponent; `to_string` would print `150` again.
        value: Some(format!("{value:?}")),
        ..stated
    }
}

/// `violation` with an enumerated value set stated in ascending order.
///
/// CodeBuild lists the values of an enumerated type in that order rather than in the
/// order its model declares them (recorded behaviour: `ProjectVisibilityType` is
/// declared `PUBLIC_READ, PRIVATE` and stated as `[PRIVATE, PUBLIC_READ]`), and the
/// order is part of the sentence a client matches on. A violation of any other kind is
/// left exactly as the framework worded it.
fn sorted_value_set(violation: &Violation) -> Violation {
    let ViolationKind::EnumValue(allowed) = &violation.kind else {
        return violation.clone();
    };
    let mut stated: Vec<String> = allowed.clone();
    stated.sort();
    Violation {
        kind: ViolationKind::EnumValue(stated),
        ..violation.clone()
    }
}

/// The validation paths, across every input in [`super::INPUT_SCHEMAS`], of the members
/// the model types as `float` or `double`.
///
/// Built once from the schemas themselves, so nothing here names a member: a path is in
/// this set exactly when the MODEL says that member is floating point.
/// Codegen emits a member schema without its target's own members
/// (`Schema::new_member` takes a shape type and a name), so the walk sees each
/// input's top-level members — which is where this service's floating-point
/// members are (`DescribeCodeCoverages$minLineCoveragePercentage` and
/// `$maxLineCoveragePercentage`, both targeting `Percentage`).
fn floating_point_member_paths() -> &'static BTreeSet<String> {
    static PATHS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
        let mut paths = BTreeSet::new();
        for input in super::INPUT_SCHEMAS {
            for member in input.members() {
                let Some(name) = member.member_name() else {
                    continue;
                };
                if matches!(member.shape_type(), ShapeType::Float | ShapeType::Double) {
                    // The casing of a validation path segment belongs to the path,
                    // not to the model, so it is spelled by the framework module
                    // that owns that spelling.
                    paths.insert(aws_smithy_schema_server::constraints::path::member(name));
                }
            }
        }
        paths
    });
    &PATHS
}

/// `path` with the position or key of each collection step dropped, so a member
/// inside a list or a map answers to the one path its member schema has.
///
/// A violation path spells a collection step as `<position-or-key>.member`
/// (`ids.1.member`); the model knows that member only as `ids.member`.
fn member_path_of(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('.') {
        // The caller-chosen key of a map entry could itself be the literal
        // `member`; that only ever costs this one path a match, since the key is
        // dropped either way.
        if segment == COLLECTION_STEP {
            segments.pop();
        }
        segments.push(segment);
    }
    segments.join(".")
}

/// The segment a validation path puts between a collection and its element, as
/// `aws_smithy_schema_server::constraints::path` spells it.
const COLLECTION_STEP: &str = "member";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every list this service refuses empty in CodeBuild's own words is a list the
    /// MODEL declares and bounds below at one — so an entry cannot claim that sentence for a
    /// member that is not a list, or for one a request may legally leave empty.
    #[test]
    fn every_stated_empty_list_is_a_list_the_model_bounds_below() {
        for (shape, member_name, stated) in EMPTY_IDENTIFIER_LIST_SENTENCES {
            let input = super::super::INPUT_SCHEMAS
                .iter()
                .copied()
                .find(|input| input.shape_id().shape_name() == shape)
                .unwrap_or_else(|| panic!("case {shape}: this service validates no such input"));
            let member = input
                .members()
                .iter()
                .copied()
                .find(|member| member.member_name() == Some(member_name))
                .unwrap_or_else(|| {
                    panic!("case {shape}${member_name}: the input declares no such member")
                });
            assert_eq!(
                member.shape_type(),
                ShapeType::List,
                "case {shape}${member_name}: expected a list, got {:?}",
                member.shape_type()
            );
            assert_eq!(
                length_of(member).and_then(|length| length.min),
                Some(1),
                "case {shape}${member_name}: expected a list the model bounds below at one, \
                 got {:?}",
                length_of(member).and_then(|length| length.min)
            );
            assert!(
                stated.ends_with("list cannot be empty."),
                "case {shape}${member_name}: expected the empty-list sentence, got {stated:?}"
            );
        }
    }

    /// RULE: every list whose ELEMENT carries a project name is a list the model declares on
    /// that input — so an entry cannot claim the empty-name sentence for a member that is not
    /// a list of this service's inputs.
    #[test]
    fn every_project_name_list_member_is_a_list_of_that_input() {
        for (shape, path) in PROJECT_NAME_LIST_MEMBERS {
            let element_step = format!(".{COLLECTION_STEP}");
            let list = path.strip_suffix(&element_step).unwrap_or_else(|| {
                panic!("case {shape}/{path}: expected the path of a list's element")
            });
            let input = super::super::INPUT_SCHEMAS
                .iter()
                .copied()
                .find(|input| input.shape_id().shape_name() == shape)
                .unwrap_or_else(|| panic!("case {shape}: this service validates no such input"));
            let member = input
                .members()
                .iter()
                .copied()
                .find(|member| member.member_name() == Some(list))
                .unwrap_or_else(|| {
                    panic!("case {shape}${list}: the input declares no such member")
                });
            assert_eq!(
                member.shape_type(),
                ShapeType::List,
                "case {shape}${list}: expected a list, got {:?}",
                member.shape_type()
            );
        }
    }

    /// RULE: the sort-key sentence names the member the value arrived on and quotes the value
    /// exactly as the request wrote it.
    ///
    /// The rows span the forms an unmodelled value takes: a modelled value in the wrong case,
    /// a token of the right shape, the empty string and one carrying a space.
    #[test]
    fn the_sort_key_sentence_names_the_member_and_quotes_the_value() {
        for submitted in ["name", "BOGUS", "", "NAME "] {
            let stated = invalid_sort_key_message(SORT_KEY_PATH, submitted);
            assert_eq!(
                stated,
                format!("Invalid sortBy type specified: {submitted}"),
                "case {submitted:?}: expected CodeBuild's sort-key sentence, got {stated:?}"
            );
        }
    }
}
