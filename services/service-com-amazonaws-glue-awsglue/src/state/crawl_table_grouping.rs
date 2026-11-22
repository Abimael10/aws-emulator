// SPDX-License-Identifier: CC0-1.0

//! How many TABLES a crawl writes for one include path, and what their columns are.
//!
//! # The rule this module implements
//!
//! A crawl of an Amazon S3 include path describes one candidate table per child prefix that
//! holds data, and then decides whether those candidates are ONE table of the include path or
//! one table each:
//!
//! * by DEFAULT, the candidates are folded into a single partitioned table of the include path
//!   only when the schemas at that folder level are SIMILAR — the data carries the same
//!   classification and the share of fields the schemas agree on (same name and same type) out
//!   of every field either carries is at least [`SCHEMA_SIMILARITY_THRESHOLD`], the partition
//!   threshold AWS applies when it decides whether a folder level is a partition of one table.
//!   Candidates that are not similar are a table each, named after their own child prefix,
//!   which is what a crawler is documented to leave behind when the schemas it detected at a
//!   folder level differ;
//! * a crawler whose `Configuration` carries
//!   `Grouping.TableGroupingPolicy = CombineCompatibleSchemas`
//!   ([`TABLE_GROUPING_POLICY_COMBINE_COMPATIBLE_SCHEMAS`]) folds candidates whose schemas are
//!   merely COMPATIBLE — the same classification, and every field they share carries types that
//!   widen to one ([`widened_type`]) — into that single table even where they are not similar.
//!
//! `CreateCrawler$Configuration` is "crawler configuration information. This versioned JSON
//! string allows users to specify aspects of a crawler's behavior", modelled as a document the
//! service does not constrain, so the member is READ here rather than validated: a
//! configuration that states no grouping policy, and one this cannot read as a JSON document at
//! all, leaves the default in force.
//!
//! A folded table's columns are the candidates' schemas MERGED ([`merged_columns`]): the first
//! candidate's columns in order, each widened to the type that covers every candidate carrying
//! that column, followed by the columns only the later candidates carry. So the grouped crawler
//! publishes one table whose schema covers the data of every child prefix it folded.
//!
//! # What bounds the work
//!
//! Both factors of it are written by callers — how many child prefixes a target holds, and how
//! many columns the data under each one carries — so both are capped before any of it runs
//! ([`MAX_CRAWLED_TABLE_CANDIDATES`], [`MAX_CRAWLED_SCHEMA_COLUMNS`], applied by the walk that
//! reads the schemas). Inside those caps every decision here is LINEAR in the columns carried:
//! each candidate's columns are resolved through a name-keyed index rather than by scanning the
//! other candidate's column list, and the first candidate's index is built ONCE per decision
//! rather than once per pair.

use super::*;

/// The `Grouping.TableGroupingPolicy` value that folds compatible schemas into one table. The
/// only policy value this service acts on; any other value leaves the default in force.
pub const TABLE_GROUPING_POLICY_COMBINE_COMPATIBLE_SCHEMAS: &str = "CombineCompatibleSchemas";

/// The `Configuration` member holding the crawler's table-grouping behaviour.
pub const CONFIGURATION_GROUPING_MEMBER: &str = "Grouping";

/// The member of [`CONFIGURATION_GROUPING_MEMBER`] naming the policy itself.
pub const CONFIGURATION_TABLE_GROUPING_POLICY_MEMBER: &str = "TableGroupingPolicy";

/// How much of their schemas two candidate tables at one folder level have to agree on for the
/// level to be partitions of ONE table rather than a table each: the partition threshold of
/// 0.7, the share of fields AWS's crawler requires the schemas to share before it treats the
/// folder level as similar.
pub const SCHEMA_SIMILARITY_THRESHOLD: f64 = 0.7;

/// How many CANDIDATE tables one Amazon S3 target yields — the child prefixes of one include
/// path this crawl describes.
///
/// CHOSEN, not an AWS quota: the prefixes under a target are written by callers, and the
/// grouping's work is the product of how many there are and how many columns they carry, so
/// this is one factor of that product. Well above what a partitioned dataset holds (a year of
/// daily folders is 365) and below the objects one crawl lists at all
/// ([`MAX_CRAWLED_OBJECTS`]), since a candidate needs an object of its own. A target holding
/// more is one this crawl did not describe.
pub const MAX_CRAWLED_TABLE_CANDIDATES: usize = 500;

/// How many COLUMNS the candidates of one target carry between them.
///
/// CHOSEN, for the reason [`MAX_CRAWLED_TABLE_CANDIDATES`] gives: it is the other factor of the
/// grouping's work, and a column comes off a header row a caller uploaded, so the two are capped
/// together rather than each on its own. Far more than a dataset's schema needs — a catalog
/// table's columns are counted in tens — and reached only by data no crawl could describe as a
/// schema anyway.
pub const MAX_CRAWLED_SCHEMA_COLUMNS: usize = 10_000;

/// The column types a number is read as, from the narrowest to the widest: a column one
/// candidate carries as a whole number and another as a fraction widens to the type that holds
/// both. Order matters — the index IS the width.
const NUMERIC_TYPE_WIDTHS: [&str; 3] = [JSON_INT_TYPE, BIGINT_TYPE, DOUBLE_TYPE];

/// How a crawl decides whether the candidate tables behind one include path are one table or
/// one each, read off the crawler's `Configuration`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TableGrouping {
    /// The behaviour of a crawler that states no grouping policy: the candidates are one table
    /// only where their schemas are SIMILAR.
    #[default]
    SimilarSchemas,
    /// `TableGroupingPolicy = CombineCompatibleSchemas`: candidates whose schemas are
    /// COMPATIBLE are folded into one table even where they are not similar.
    CombineCompatibleSchemas,
}

impl TableGrouping {
    /// What the crawler's stored `Configuration` asks for. A configuration that states no
    /// policy, states one this service does not act on, or is not a JSON document at all leaves
    /// the default behaviour in force.
    #[must_use]
    pub fn of_configuration(configuration: Option<&str>) -> Self {
        let policy = configuration
            .and_then(|configuration| {
                ::serde_json::from_str::<::serde_json::Value>(configuration).ok()
            })
            .as_ref()
            .and_then(|document| {
                document
                    .get(CONFIGURATION_GROUPING_MEMBER)?
                    .get(CONFIGURATION_TABLE_GROUPING_POLICY_MEMBER)?
                    .as_str()
                    .map(str::to_string)
            });
        match policy {
            Some(policy)
                if policy
                    .trim()
                    .eq_ignore_ascii_case(TABLE_GROUPING_POLICY_COMBINE_COMPATIBLE_SCHEMAS) =>
            {
                Self::CombineCompatibleSchemas
            }
            _ => Self::SimilarSchemas,
        }
    }

    /// Whether these candidates are ONE table of the include path.
    ///
    /// A single candidate is always that one table: there is no second schema at the folder
    /// level to tell it apart from. Every further candidate is decided against the FIRST one —
    /// the schemas at a folder level are either the same dataset or they are not — under the
    /// relation this grouping names: similarity by default, compatibility under
    /// [`Self::CombineCompatibleSchemas`].
    ///
    /// The first candidate's columns are indexed ONCE here and every other candidate is decided
    /// against that index, so the whole decision costs one pass over the columns the candidates
    /// carry.
    #[must_use]
    pub fn folds(self, candidates: &[CandidateSchema<'_>]) -> bool {
        let Some((first, rest)) = candidates.split_first() else {
            return true;
        };
        let held = declared_types(first);
        rest.iter().all(|other| match self {
            Self::SimilarSchemas => is_similar(first.classification, &held, other),
            Self::CombineCompatibleSchemas => is_compatible(first.classification, &held, other),
        })
    }
}

/// What a crawl read off ONE candidate table's data, as the grouping decision sees it: what the
/// data was classified as, and the columns it carries.
#[derive(Debug, Clone, Copy)]
pub struct CandidateSchema<'a> {
    /// The dataset's classification ([`CLASSIFICATION_PARAMETER`]): data of two formats is
    /// never one table, whatever its columns say.
    pub classification: &'a str,
    /// The columns the candidate's data carries, in the order the data carries them.
    pub columns: &'a [Column],
}

/// Whether two candidate schemas at one folder level are SIMILAR: the same classification, and
/// at least [`SCHEMA_SIMILARITY_THRESHOLD`] of the fields either carries are fields both carry
/// under the same name AND the same type.
#[must_use]
pub fn schemas_are_similar(first: &CandidateSchema<'_>, other: &CandidateSchema<'_>) -> bool {
    is_similar(first.classification, &declared_types(first), other)
}

/// Whether two candidate schemas are COMPATIBLE: the same classification, and every column
/// BOTH carry holds types that widen to one ([`widened_type`]). A column only one of them
/// carries is no incompatibility — the merged schema simply carries it.
#[must_use]
pub fn schemas_are_compatible(first: &CandidateSchema<'_>, other: &CandidateSchema<'_>) -> bool {
    is_compatible(first.classification, &declared_types(first), other)
}

/// [`schemas_are_similar`] against a candidate whose columns are already indexed.
fn is_similar(
    classification: &str,
    held: &HashMap<&str, &str>,
    other: &CandidateSchema<'_>,
) -> bool {
    if classification != other.classification {
        return false;
    }
    let theirs = declared_types(other);
    if held.is_empty() && theirs.is_empty() {
        return true;
    }
    // Every field either schema carries, counted once: the ones the first carries, plus the
    // ones only the other does.
    let mut fields = held.len();
    let mut agreed = 0usize;
    for (name, declared) in &theirs {
        match held.get(name) {
            Some(ours) if ours == declared => agreed = agreed.saturating_add(1),
            Some(_) => {}
            None => fields = fields.saturating_add(1),
        }
    }
    // Both counts are bounded by the columns one target's candidates may carry
    // ([`MAX_CRAWLED_SCHEMA_COLUMNS`]), which is far inside the integers this ratio is exact
    // for.
    #[allow(clippy::cast_precision_loss)]
    let agreement = agreed as f64 / fields as f64;
    agreement >= SCHEMA_SIMILARITY_THRESHOLD
}

/// [`schemas_are_compatible`] against a candidate whose columns are already indexed.
fn is_compatible(
    classification: &str,
    held: &HashMap<&str, &str>,
    other: &CandidateSchema<'_>,
) -> bool {
    if classification != other.classification {
        return false;
    }
    other.columns.iter().all(|column| {
        match held.get(column.name.as_str()) {
            Some(ours) => {
                widened_type(ours, column.r#type.as_deref().unwrap_or_default()).is_some()
            }
            None => true,
        }
    })
}

/// One candidate's columns indexed by name, as the type each declares — the index every
/// decision here resolves a column through, so nothing scans a column list.
///
/// A column carrying no type at all is indexed under the empty reading, which agrees with
/// another untyped column of the same name and widens with nothing else.
fn declared_types<'a>(candidate: &CandidateSchema<'a>) -> HashMap<&'a str, &'a str> {
    candidate
        .columns
        .iter()
        .map(|column| {
            (
                column.name.as_str(),
                column.r#type.as_deref().unwrap_or_default(),
            )
        })
        .collect()
}

/// The columns of the ONE table a fold of these candidates writes: the first candidate's
/// columns in order, each widened to the type covering every candidate that carries that
/// column, then the columns only the later candidates carry, in candidate order.
///
/// A pair of types that widens to neither keeps the FIRST candidate's reading of the column:
/// the candidate that described the dataset is the one whose schema the table is written from.
///
/// The merged columns are indexed by name as they are collected, so each candidate's columns
/// cost one lookup each rather than a scan of what is merged so far.
#[must_use]
pub fn merged_columns(candidates: &[CandidateSchema<'_>]) -> Vec<Column> {
    let mut merged: Vec<Column> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for candidate in candidates {
        for column in candidate.columns {
            match at.get(column.name.as_str()) {
                Some(index) => {
                    if let Some(held) = merged.get_mut(*index) {
                        if let (Some(one), Some(another)) =
                            (held.r#type.clone(), column.r#type.as_deref())
                        {
                            if let Some(widened) = widened_type(&one, another) {
                                held.r#type = Some(widened);
                            }
                        }
                    }
                }
                None => {
                    at.insert(column.name.clone(), merged.len());
                    merged.push(column.clone());
                }
            }
        }
    }
    merged
}

/// The ONE type that holds the values of both readings of a column, or `None` for two readings
/// nothing covers.
///
/// The same type twice is itself; a whole number read as a wider whole number, or as a
/// fraction, is the wider of the two ([`NUMERIC_TYPE_WIDTHS`]). Anything else — text beside a
/// number, a nested value beside a scalar — is two different columns rather than one widened
/// one.
#[must_use]
pub fn widened_type(one: &str, another: &str) -> Option<String> {
    if one == another {
        return Some(one.to_string());
    }
    let (narrow, wide) = (width_of(one)?, width_of(another)?);
    NUMERIC_TYPE_WIDTHS
        .get(narrow.max(wide))
        .map(|widened| (*widened).to_string())
}

/// How wide a numeric column type is, as its index in [`NUMERIC_TYPE_WIDTHS`], or `None` for a
/// type that holds no number.
fn width_of(declared: &str) -> Option<usize> {
    NUMERIC_TYPE_WIDTHS
        .iter()
        .position(|candidate| *candidate == declared)
}
