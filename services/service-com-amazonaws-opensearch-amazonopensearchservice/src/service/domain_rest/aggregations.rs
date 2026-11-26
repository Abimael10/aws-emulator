// SPDX-License-Identifier: CC0-1.0

//! The AGGREGATIONS section of a `_search` body: the summaries a request asks OVER the
//! documents its query selected, rather than the hits themselves.
//!
//! An aggregation reads a field's PER-DOCUMENT VALUES — the same values a sort key orders by
//! and a `term` clause compares against ([`Terms::held`]) — once per document the scan
//! selects, so what it reports is computed from the documents the caller's query really
//! matched and from nothing else. That is also what makes it answer the FIELD-LEVEL SECURITY
//! in force: the scan is handed each document as the caller may SEE it, so a field hidden
//! from that caller holds no value to aggregate and contributes to no bucket and no metric —
//! reading a field by aggregating it is reading it.
//!
//! Reading a field's per-document values is also what an aggregation CANNOT do on a field the
//! index holds none for — an analyzed `text` field with `fielddata` left off, whose values are
//! only the terms of its analysis. Such a field can bucket and measure nothing, so an
//! aggregation naming one is REFUSED ([`Aggregations::without_doc_values`]) exactly as a sort
//! key naming one is: an empty `buckets` array means "the selected documents hold no such
//! value", and answering it for a field that could not be read at all would report nothing
//! where the engine reports a failure. A MULTI-FIELD of that same field — the `keyword`
//! sub-field a mapping adds for exactly this purpose — does hold them, and reads the values of
//! the field above it, which is what makes the usual "full-text search plus exact aggregation
//! on one field" mapping work.
//!
//! The aggregations served are the ones this engine can compute over those values: the
//! `terms` buckets, and the single-value metrics `value_count`, `min`, `max`, `sum` and `avg`.
//! A request naming any other type is REFUSED, rather than answered with a section it did not
//! ask for or with one silently left out.
//!
//! A BUCKET aggregation may carry an `aggs` section of its own, and those sub-aggregations are
//! computed over the documents that fell in each bucket — a bucket is a subset of the
//! documents the query selected, so the sub-aggregations of one are the same reading of that
//! subset, reported inside the bucket beside its `doc_count`. A METRIC opens no bucket and so
//! has no subset to aggregate over, which is why one carrying sub-aggregations is refused.
//!
//! Every factor of the work one request causes is bounded, and so is their PRODUCT: how many
//! aggregations one request may name ([`MAX_AGGREGATIONS`]), how deep they may nest
//! ([`MAX_AGGREGATION_DEPTH`]), how many buckets ONE REQUEST may open over all of them together
//! ([`MAX_BUCKETS`]) and how many per-document VALUES one request may read into them, at every
//! level ([`MAX_VALUES_READ`]) — so neither the body a caller wrote, nor the documents the
//! domain holds, nor the way the body nests one inside another can make one request unbounded.

use std::collections::BTreeMap;

use ::serde_json::{json, Map, Value};
use aws_smithy_runtime_api::http::Response as RtResponse;

use crate::state::analysis::IndexAnalysis;

use super::{error, scalar_text, Terms, PARSING_EXCEPTION};

/// How many aggregations one request may name.
///
/// The emulator's OWN bound, like the one on a query's clauses: every aggregation reads every
/// document the scan selects, so this is what keeps that work the documents selected rather
/// than their product with a count the request chose.
const MAX_AGGREGATIONS: usize = 32;

/// How many buckets ONE REQUEST may open, over every aggregation it names and every level
/// they nest.
///
/// The engine's own `search.max_buckets` is the bound of this kind — the buckets of the whole
/// response, which is why it is counted across the request rather than per aggregation; this
/// is the value this cluster holds it at, because every bucket lives in this process's heap
/// until the response is rendered. Counting them together is also what keeps a nested section
/// bounded: without it, each level would multiply the level above it.
const MAX_BUCKETS: usize = 1_000;

/// How deep one aggregations section may nest.
///
/// The emulator's OWN bound, beside the two above: a sub-aggregation is read into every bucket
/// its parent opens, so the depth is what the section's shape multiplies, and a body past this
/// is refused while it is being read rather than walked.
const MAX_AGGREGATION_DEPTH: usize = 8;

/// How many per-document VALUES one request may read into its aggregations, over every
/// aggregation and every level they nest.
///
/// The emulator's OWN bound, and the one that bounds the PRODUCT the three above leave open: a
/// bucket's sub-aggregations read each document of that bucket again, so without a budget over
/// the whole reading a body nesting one aggregation inside another would multiply the values
/// each level walks by the values of the level above it — a number the caller's body and the
/// documents the domain holds choose together. Charged per value read at EVERY level and
/// checked before those values are walked ([`Requested::consider`]), so nothing past it is read
/// whatever shape the section has; a request that reaches it is refused rather than answered
/// from a reading that stopped halfway.
///
/// Sized well above what a plain section reads — a scan of every document one domain may hold
/// ([`super::MAX_DOCUMENTS_VISITED`]) with a handful of values at each — and far below what the
/// nesting could otherwise reach, because each value read costs this process a value clone and
/// a rendering of its text.
const MAX_VALUES_READ: usize = 10_000_000;

/// How many buckets a `terms` aggregation REPORTS when the request names no `size`, which is
/// that aggregation's own documented default.
const DEFAULT_TERMS_SIZE: usize = 10;

/// The two members a search names its aggregations under, which are the two spellings the
/// engine takes.
const AGGS: [&str; 2] = ["aggs", "aggregations"];

/// The member every aggregation below names the field it reads under.
const FIELD: &str = "field";

/// The member a `terms` aggregation names how many buckets it wants under.
const SIZE: &str = "size";

/// What one aggregation computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `terms`: "a multi-bucket aggregation where each bucket is a unique value for the
    /// field it is run on", reporting the `size` most frequent of them.
    Terms { size: usize },
    /// `value_count`: how many values the selected documents hold for the field.
    Count,
    /// The single-value metrics over those values.
    Min,
    Max,
    Sum,
    Avg,
}

/// The names those kinds are asked for under.
const TERMS: &str = "terms";
const VALUE_COUNT: &str = "value_count";
const MIN: &str = "min";
const MAX: &str = "max";
const SUM: &str = "sum";
const AVG: &str = "avg";

/// What one aggregation has observed of the documents the scan has reached so far.
#[derive(Debug, Clone, Default)]
struct Observed {
    /// The buckets a `terms` aggregation has opened, keyed by the TEXT of the value they
    /// bucket — the form every value of a field is compared by.
    buckets: BTreeMap<String, Bucket>,
    /// How many values the field held over the documents selected.
    values: usize,
    /// The arithmetic the metrics report, over the values that ARE numbers: a value a field
    /// holds no number for is read by no metric.
    numbers: usize,
    sum: f64,
    least: Option<f64>,
    greatest: Option<f64>,
}

/// One bucket a `terms` aggregation has opened: the value it buckets as it was read, how many
/// documents fell in it, and what this bucket's own sub-aggregations have observed of THOSE
/// documents.
#[derive(Debug, Clone)]
struct Bucket {
    key: Value,
    count: usize,
    /// The sub-aggregations over the documents of this bucket — a fresh copy of the ones the
    /// request named, since each bucket aggregates its own subset.
    subs: Vec<Requested>,
}

/// One aggregation a request asked for: the name it is reported under, the field it reads,
/// what it computes from it, and the aggregations it asks OVER each bucket it opens.
#[derive(Debug, Clone)]
struct Requested {
    name: String,
    field: String,
    kind: Kind,
    /// The section this aggregation carries of its own, computed per bucket. Empty for an
    /// aggregation that carries none, and never non-empty for a metric, which opens no
    /// bucket to compute them over.
    subs: Vec<Requested>,
    observed: Observed,
}

/// The aggregations a search asked for — empty for a request that named none.
#[derive(Debug, Clone, Default)]
pub(super) struct Aggregations {
    requested: Vec<Requested>,
    /// How many buckets this request has opened so far, over every aggregation and every
    /// level: what [`MAX_BUCKETS`] bounds.
    opened: usize,
    /// How many per-document values this request has read so far, over every aggregation and
    /// every level: what [`MAX_VALUES_READ`] bounds.
    read: usize,
}

impl Aggregations {
    /// The aggregations `body` names, or the engine's refusal of a section it does not read.
    pub(super) fn of(body: &Value) -> Result<Self, Box<RtResponse>> {
        let Some(named) = AGGS.iter().find_map(|member| body.get(*member)) else {
            return Ok(Self::default());
        };
        let mut counted = 0_usize;
        Ok(Self {
            requested: section(named, 0, &mut counted)?,
            opened: 0,
            read: 0,
        })
    }

    /// Whether this names no aggregation at all.
    pub(super) fn is_empty(&self) -> bool {
        self.requested.is_empty()
    }

    /// The first field these aggregations name that the index whose mappings are `analysis`
    /// holds no PER-DOCUMENT values for, where they name one: the condition an aggregation
    /// cannot be computed under, since every one of them reads those values.
    ///
    /// The same question a sort key is asked ([`super::search::Sorting::without_doc_values`]),
    /// because it is the same reading: a field's per-document values are what orders a hit and
    /// what buckets or measures one.
    pub(super) fn without_doc_values(&self, analysis: &IndexAnalysis) -> Option<&str> {
        self.requested
            .iter()
            .find_map(|requested| requested.without_doc_values(analysis))
    }

    /// Read one SELECTED document into every aggregation.
    ///
    /// `source` is the document as the caller may see it, which is what makes a field hidden
    /// from that caller hold no value here.
    pub(super) fn consider(
        &mut self,
        terms: &Terms<'_>,
        source: &Value,
    ) -> Result<(), Box<RtResponse>> {
        let mut opened = self.opened;
        let mut read = self.read;
        for requested in &mut self.requested {
            requested.consider(terms, source, &mut opened, &mut read)?;
        }
        self.opened = opened;
        self.read = read;
        Ok(())
    }

    /// What the aggregations report over the documents the scan selected.
    pub(super) fn reported(&self) -> Value {
        let mut reported = Map::new();
        for requested in &self.requested {
            reported.insert(requested.name.clone(), requested.reported());
        }
        Value::Object(reported)
    }
}

impl Requested {
    /// The first field this aggregation or one of its sub-aggregations names that the index
    /// whose mappings are `analysis` holds no per-document values for.
    fn without_doc_values(&self, analysis: &IndexAnalysis) -> Option<&str> {
        if analysis.needs_fielddata(&self.field) {
            return Some(self.field.as_str());
        }
        self.subs
            .iter()
            .find_map(|sub| sub.without_doc_values(analysis))
    }

    /// Read one document's values for this aggregation's field into it.
    ///
    /// `opened` carries how many buckets the whole request has opened and `read` how many
    /// values it has read, over every aggregation and every level — the two budgets this
    /// reading is bounded by ([`MAX_BUCKETS`], [`MAX_VALUES_READ`]).
    ///
    /// A document falls in a bucket ONCE however many of its values map to that bucket's key:
    /// a `doc_count` counts documents, and a bucket's own sub-aggregations read the document
    /// once, the way they would read it if the bucket's documents were the whole selection.
    /// That is also what keeps this reading bounded by the budgets above rather than by the
    /// product of the per-level value counts: the values are read once per level, and what
    /// recurses is the DISTINCT buckets the document opened.
    fn consider(
        &mut self,
        terms: &Terms<'_>,
        source: &Value,
        opened: &mut usize,
        read: &mut usize,
    ) -> Result<(), Box<RtResponse>> {
        let held_values = terms.held(source, &self.field);
        // Charged BEFORE the values are walked, and at every level, so no reading past the
        // budget is performed whatever the section's shape: a level that recurses charges the
        // values of each level below it too.
        *read = read.saturating_add(held_values.len().max(1));
        if *read > MAX_VALUES_READ {
            return Err(too_many_values());
        }
        // The buckets THIS document opens, keyed by the TEXT every value of a field is
        // compared by, holding the value as it was read — so a value the document repeats
        // opens one bucket and is counted once.
        let mut keyed: BTreeMap<String, Value> = BTreeMap::new();
        for held in held_values {
            if held.is_null() {
                continue;
            }
            self.observed.values = self.observed.values.saturating_add(1);
            if let Some(number) = held.as_f64() {
                self.observed.numbers = self.observed.numbers.saturating_add(1);
                self.observed.sum += number;
                self.observed.least = Some(
                    self.observed
                        .least
                        .map_or(number, |least| if number < least { number } else { least }),
                );
                self.observed.greatest = Some(
                    self.observed
                        .greatest
                        .map_or(number, |most| if number > most { number } else { most }),
                );
            }
            if !matches!(self.kind, Kind::Terms { .. }) {
                continue;
            }
            keyed.entry(scalar_text(&held)).or_insert(held);
        }
        for (key, value) in keyed {
            if !self.observed.buckets.contains_key(&key) {
                if *opened >= MAX_BUCKETS {
                    return Err(too_many_buckets(*opened));
                }
                *opened = opened.saturating_add(1);
            }
            let subs = self.subs.clone();
            let bucket = self.observed.buckets.entry(key).or_insert_with(|| Bucket {
                key: value,
                count: 0,
                subs,
            });
            bucket.count = bucket.count.saturating_add(1);
            // The document fell in this bucket, so this bucket's own sub-aggregations read it
            // — a bucket is a subset of the selected documents, and its sub-aggregations are
            // that subset's summaries.
            for sub in &mut bucket.subs {
                sub.consider(terms, source, opened, read)?;
            }
        }
        Ok(())
    }

    /// What this one aggregation reports.
    ///
    /// A `terms` aggregation reports its buckets most frequent first, and a tie between two
    /// equally frequent values is broken by the value itself — so one set of documents
    /// reports the same buckets in the same order on every read. The documents whose value
    /// fell outside the reported buckets are counted in `sum_other_doc_count`, which is what
    /// tells a caller there were more. Each bucket carries its own sub-aggregations' reports
    /// beside its `doc_count`, computed over the documents that fell in it.
    fn reported(&self) -> Value {
        match self.kind {
            Kind::Terms { size } => {
                let mut ranked: Vec<(&String, &Bucket)> = self.observed.buckets.iter().collect();
                ranked.sort_by(|(left_key, left), (right_key, right)| {
                    right
                        .count
                        .cmp(&left.count)
                        .then_with(|| left_key.cmp(right_key))
                });
                let other: usize = ranked.iter().skip(size).map(|(_, bucket)| bucket.count).sum();
                let buckets: Vec<Value> = ranked
                    .iter()
                    .take(size)
                    .map(|(_, bucket)| {
                        let mut reported = Map::new();
                        reported.insert("key".to_string(), bucket.key.clone());
                        reported.insert("doc_count".to_string(), json!(bucket.count));
                        for sub in &bucket.subs {
                            reported.insert(sub.name.clone(), sub.reported());
                        }
                        Value::Object(reported)
                    })
                    .collect();
                json!({
                    // Every document the scan selected was read, so no bucket's count is an
                    // estimate and none was dropped from a shard's own report.
                    "doc_count_error_upper_bound": 0,
                    "sum_other_doc_count": other,
                    "buckets": buckets,
                })
            }
            Kind::Count => json!({ "value": self.observed.values }),
            Kind::Min => json!({ "value": number(self.observed.least) }),
            Kind::Max => json!({ "value": number(self.observed.greatest) }),
            // "sum: a sum of the values": over no value at all it is zero, while an average,
            // a minimum and a maximum of no value is no value.
            Kind::Sum => json!({ "value": number(Some(self.observed.sum)) }),
            Kind::Avg => {
                #[allow(clippy::cast_precision_loss)]
                let average = (self.observed.numbers > 0)
                    .then(|| self.observed.sum / self.observed.numbers as f64);
                json!({ "value": number(average) })
            }
        }
    }
}

/// The aggregations ONE `aggs` section names — the request's own, or the one a bucket
/// aggregation carries for the documents of each bucket it opens.
///
/// `counted` carries how many aggregations the whole section has named so far, so the bound is
/// on the request rather than on one level of it, and `depth` is how deep this level sits.
fn section(
    named: &Value,
    depth: usize,
    counted: &mut usize,
) -> Result<Vec<Requested>, Box<RtResponse>> {
    let Some(named) = named.as_object() else {
        return Err(unreadable("[aggs] must be an object naming each aggregation"));
    };
    if depth > MAX_AGGREGATION_DEPTH {
        return Err(unreadable(&format!(
            "an aggregation may nest at most {MAX_AGGREGATION_DEPTH} levels of \
             sub-aggregations"
        )));
    }
    let mut requested = Vec::new();
    for (name, definition) in named {
        *counted = counted.saturating_add(1);
        if *counted > MAX_AGGREGATIONS {
            return Err(unreadable(&format!(
                "a request may name at most {MAX_AGGREGATIONS} aggregations, and this one \
                 names {counted}"
            )));
        }
        requested.push(one(name, definition, depth, counted)?);
    }
    Ok(requested)
}

/// One aggregation of the section: the single type it names, the field that type reads, and
/// the sub-aggregations it asks over each bucket it opens.
fn one(
    name: &str,
    definition: &Value,
    depth: usize,
    counted: &mut usize,
) -> Result<Requested, Box<RtResponse>> {
    let Some(members) = definition.as_object() else {
        return Err(unreadable(&format!(
            "[{name}] must be an object naming the aggregation it asks for"
        )));
    };
    // One member names the TYPE and one may name the section computed per bucket, in either
    // order, so the two are told apart by name rather than by position.
    let mut typed: Option<(&String, &Value)> = None;
    let mut nested: Option<&Value> = None;
    for (member, value) in members {
        if AGGS.contains(&member.as_str()) {
            nested = Some(value);
            continue;
        }
        if typed.replace((member, value)).is_some() {
            return Err(unreadable(&format!(
                "[{name}] aggregation must name exactly one type"
            )));
        }
    }
    let Some((named, options)) = typed else {
        return Err(unreadable(&format!(
            "[{name}] aggregation must name exactly one type"
        )));
    };
    let empty = Map::new();
    let options = options.as_object().unwrap_or(&empty);
    let kind = match named.as_str() {
        TERMS => Kind::Terms {
            size: options
                .get(SIZE)
                .and_then(Value::as_u64)
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(DEFAULT_TERMS_SIZE)
                .min(MAX_BUCKETS),
        },
        VALUE_COUNT => Kind::Count,
        MIN => Kind::Min,
        MAX => Kind::Max,
        SUM => Kind::Sum,
        AVG => Kind::Avg,
        other => return Err(unreadable(&format!("Unknown aggregation type [{other}]"))),
    };
    let Some(field) = options
        .get(FIELD)
        .and_then(Value::as_str)
        .filter(|field| !field.is_empty())
    else {
        return Err(unreadable(&format!(
            "[{named}] aggregation [{name}] must name the [field] it reads"
        )));
    };
    Ok(Requested {
        name: name.to_string(),
        field: field.to_string(),
        kind,
        subs: match nested {
            // A metric opens no bucket, so there is no subset of the documents for a section
            // under it to be computed over: it is refused rather than answered with the
            // section left out.
            Some(_) if !matches!(kind, Kind::Terms { .. }) => {
                return Err(no_sub_aggregations(name, named))
            }
            Some(nested) => section(nested, depth.saturating_add(1), counted)?,
            None => Vec::new(),
        },
        observed: Observed::default(),
    })
}

/// A metric's value as the engine answers it: the number it computed, and `null` for a metric
/// that read no value to compute one from.
fn number(value: Option<f64>) -> Value {
    value
        .and_then(::serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// The engine's refusal of an aggregations section it cannot read.
fn unreadable(reason: &str) -> Box<RtResponse> {
    Box::new(error(400, PARSING_EXCEPTION, reason))
}

/// The engine's refusal of a request whose buckets would pass the cluster's own ceiling,
/// which names that ceiling and the setting that carries it.
fn too_many_buckets(opened: usize) -> Box<RtResponse> {
    Box::new(error(
        400,
        TOO_MANY_BUCKETS_EXCEPTION,
        &format!(
            "Trying to create too many buckets. Must be less than or equal to: \
             [{MAX_BUCKETS}] but was [{}]. This limit can be set by changing the \
             [search.max_buckets] cluster level setting.",
            opened.saturating_add(1)
        ),
    ))
}

/// The engine's error type for a request past that ceiling.
const TOO_MANY_BUCKETS_EXCEPTION: &str = "too_many_buckets_exception";

/// The engine's refusal of sub-aggregations under an aggregation that opens no bucket, which
/// names the aggregation and the type that cannot carry them.
fn no_sub_aggregations(name: &str, kind: &str) -> Box<RtResponse> {
    Box::new(error(
        400,
        AGGREGATION_INITIALIZATION_EXCEPTION,
        &format!("Aggregator [{name}] of type [{kind}] cannot accept sub-aggregations"),
    ))
}

/// The engine's error type for an aggregations section it cannot build the aggregators of.
const AGGREGATION_INITIALIZATION_EXCEPTION: &str = "aggregation_initialization_exception";

/// The refusal of a request whose aggregations would read more per-document values than one
/// request may read — the emulator's own bound, stated as the argument failure it is.
fn too_many_values() -> Box<RtResponse> {
    Box::new(error(
        400,
        crate::state::ingest::ILLEGAL_ARGUMENT_EXCEPTION,
        &format!(
            "a request may read at most {MAX_VALUES_READ} values into its aggregations"
        ),
    ))
}
