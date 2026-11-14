// SPDX-License-Identifier: CC0-1.0

//! @op Rerank
//!
//! ## What this handler does
//!
//! `Rerank` is a stateless data-plane computation: it mints no resource, and no other
//! operation of this service reports a rerank, so it persists nothing (the only store
//! it reads is the one the AUTHORIZATION decision is made from — the caller's STS
//! identity and the IAM policies in force on it, through
//! [`crate::state::authorize`], exactly as every other operation here decides it).
//!
//! It answers with ONE permutation of the supplied `sources`, ordered by the relevance
//! of each source document to the single query, highest first:
//!
//! * every source contributes exactly ONE result, carrying that source's POSITION in
//!   the request's `sources` list as `index` — the list is permuted once for the query,
//!   not once per document;
//! * `relevanceScore` is a number in `(0, 1]` computed from the query text and that
//!   source document's own text, and from nothing else: the same (query, document) pair
//!   scores the same in every call, whatever the rest of the candidate list holds;
//! * `numberOfResults` keeps the leading `n` of that ordering.
//!
//! The `modelArn` the configuration names is RESOLVED before any of that: a reranking
//! model is addressed by the ARN of a Bedrock foundation model, and a value that is not
//! one is refused ([`INVALID_MODEL_ARN`]) rather than answered with a ranking produced
//! by no named model.
//!
//! ## How the relevance score is computed
//!
//! NO neural reranker model runs inside this emulator, and nothing here invents a score
//! and presents it as one a model produced (the same position
//! `service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice`'s `state` module
//! takes on inference). What a reranker ANSWERS, though, is an ordering by how well a
//! document answers the query, with a score that falls away sharply down that ordering —
//! so the two things computed here are exactly those, from the real text of the pair:
//!
//! * the RELEVANCE of the pair, in `[0, 1]`: how much of what the query ASKS ABOUT the
//!   document carries (its share of the query's content-bearing terms, the measure that
//!   decides the ordering), and how concentrated the document is on those terms (the
//!   share of the document's own terms they are, which separates a document about the
//!   query from a long one that merely mentions it). The query's FUNCTION words are not
//!   what it asks about — they are carried by every document alike — so they are left
//!   out, through the published list the `stop-words` crate carries, and both sides are
//!   folded to their Snowball stems so a query about one inflection is answered by a
//!   document spelling another. The same tokenisation, stop list and stemmer as
//!   `service-com-amazonaws-kendraranking-awskendrarerankingfrontendservice`'s
//!   `Rescore`, which ranks documents for a query for the same reason;
//! * the SCORE, which is that relevance on a LOGARITHMIC scale
//!   ([`relevance_score`]): a drop in relevance divides the score rather than subtracting
//!   from it, so a materially better match scores orders of magnitude above a weak one and
//!   the ranking a caller takes the top-k of is separated instead of being a band of
//!   near-equal numbers. `1.0` for a document that carries the whole query and nothing
//!   else; a positive FLOOR ([`relevance_score`] at zero relevance) for one that shares
//!   no content term, never `0.0`, because the score is a strength of match and no
//!   document is ruled out by having been reranked.
//!
//! Both are a pure function of the ONE pair — a cross-encoder reranker scores each
//! (query, document) pair independently, so a corpus-relative weighting (BM25's inverse
//! document frequency, say) would be the wrong shape: a document's score would move when
//! an unrelated candidate joined the list.
//!
//! A `JSON` source document is scored over the text its JSON carries ([`json_text`]),
//! since that serialisation is what a reranker model is shown.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `Rerank::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `Rerank`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json")
//! ```
//!
//! Reranks the relevance of sources based on queries. For more information, see Improve the
//! relevance of query responses with a reranker model.
//!
//! ## Input
//!
//! * `nextToken` (NextToken, len 1..2048, pattern ^\S*$) — If the total number of results was
//!   greater than could fit in a response, a token is returned in the nextToken field. You
//!   can enter that token in this field to return the next batch of results.
//! * `queries` (RerankQueriesList, len 1..1) **required** — An array of objects, each of
//!   which contains information about a query to submit to the reranker model.
//! * `rerankingConfiguration` (RerankingConfiguration) **required** — Contains configurations
//!   for reranking.
//! * `sources` (RerankSourcesList, len 1..1000) **required** — An array of objects, each of
//!   which contains information about the sources to rerank.
//!
//! ## Output
//!
//! * `nextToken` (NextToken, len 1..2048, pattern ^\S*$) — If the total number of results is
//!   greater than can fit in the response, use this token in the nextToken field when making
//!   another request to return the next batch of results.
//! * `results` (RerankResultsList) **required** — An array of objects, each of which contains
//!   information about the results of reranking.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — The request is denied because of missing access
//!   permissions. Check your permissions and retry your request.
//! * `BadGatewayException` (HTTP 502) — There was an issue with a dependency due to a server
//!   issue. Retry your request.
//! * `ConflictException` (HTTP 409) — There was a conflict performing an operation. Resolve
//!   the conflict and retry your request.
//! * `DependencyFailedException` (HTTP 424) — There was an issue with a dependency. Check the
//!   resource configurations and retry the request.
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry your
//!   request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource Amazon Resource Name
//!   (ARN) was not found. Check the Amazon Resource Name (ARN) and try your request again.
//! * `ServiceQuotaExceededException` (HTTP 400) — The number of requests exceeds the service
//!   quota. Resubmit your request later.
//! * `ThrottlingException` (HTTP 429) — The number of requests exceeds the limit. Resubmit
//!   your request later.
//! * `ValidationException` (HTTP 400) — Input validation failed. Check your request
//!   parameters and retry the request.

use std::collections::BTreeMap;

use ::aws_smithy_types::Document as JsonValue;
use ::aws_smithy_types::Number as JsonNumber;

#[allow(unused_imports)]
use super::types::*;

/// The `RerankingConfigurationType` this operation serves, and the only one the model
/// declares (`RERANKING_CONFIGURATION_TYPE_VALUES`).
const CONFIGURATION_TYPE_BEDROCK: &str = "BEDROCK_RERANKING_MODEL";

/// The `RerankQueryContentType` whose text lives in `textQuery`, and the only one the
/// model declares (`RERANK_QUERY_CONTENT_TYPE_VALUES`).
const QUERY_TYPE_TEXT: &str = "TEXT";

/// The `RerankSourceType` whose document travels in the request, and the only one the
/// model declares (`RERANK_SOURCE_TYPE_VALUES`).
const SOURCE_TYPE_INLINE: &str = "INLINE";

/// The two `RerankDocumentType` values (`RERANK_DOCUMENT_TYPE_VALUES`): `TEXT` carries
/// the document in `textDocument`, `JSON` in `jsonDocument`.
const DOCUMENT_TYPE_TEXT: &str = "TEXT";

/// See [`DOCUMENT_TYPE_TEXT`].
const DOCUMENT_TYPE_JSON: &str = "JSON";

/// How deep a caller-supplied `jsonDocument` is walked before the walk stops, keeping
/// the recursion bound LOCAL to this crate rather than inherited from the
/// deserializer's own limit (the same bound, for the same reason, as
/// `service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice`'s
/// `MAX_DOCUMENT_DEPTH`).
const MAX_DOCUMENT_DEPTH: usize = 64;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `RerankError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `RerankError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentruntime#Rerank")]
#[derive(Debug)]
pub enum RerankErrorModeled {
    AccessDeniedException(AccessDeniedException),
    BadGatewayException(BadGatewayException),
    ConflictException(ConflictException),
    DependencyFailedException(DependencyFailedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::rerank::RerankError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type RerankError = crate::scaffolded::Scaffolded<RerankErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<RerankErrorModeled> for RerankError {
    fn from(modeled: RerankErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `Rerank` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct Rerank<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> Rerank<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for Rerank<Eng>
{
    type Input = RerankInput;
    type Output = RerankOutput;
    type Error = RerankError;

    const NAME: &'static str = "Rerank";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::RERANK_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::RERANK_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The request is read in full BEFORE anything is decided or scored: a request
        // this service cannot interpret is refused whatever the caller may do, and the
        // authorization decision below needs the model the call names.
        let configuration = &input.reranking_configuration;
        if configuration.r#type != CONFIGURATION_TYPE_BEDROCK {
            return Err(refuse(format!(
                "rerankingConfiguration.type: {:?} is not a reranking configuration type \
                 this service offers. Supported types: {}",
                configuration.r#type,
                RERANKING_CONFIGURATION_TYPE_VALUES.join(", ")
            )));
        }

        // The reranking model is RESOLVED before anything is ranked: the ranking this
        // call answers is the one the named model produces, so a `modelArn` that names
        // no Bedrock foundation model is refused rather than served.
        let model_arn = &configuration
            .bedrock_reranking_configuration
            .model_configuration
            .model_arn;
        if !names_foundation_model(model_arn) {
            return Err(refuse(INVALID_MODEL_ARN.to_string()));
        }

        // `nextToken` names a page of a response this service never splits: a request
        // carries at most 1000 sources (`RerankSourcesList`'s own length bound), so the
        // complete ranking always fits one response and NO call issues a token. A token
        // reaching here therefore cannot have come from this service.
        if input.next_token.is_some() {
            return Err(refuse(
                "nextToken is not a pagination token this service issued: a Rerank response \
                 carries the complete ranking of the sources it was given"
                    .to_string(),
            ));
        }

        // `RerankQueriesList` is modelled `length 1..1`, so the boundary validator has
        // already refused any other count and the one query is the whole query.
        let Some(query) = input.queries.first() else {
            return Err(refuse(
                "queries must carry exactly one query to rerank the sources against"
                    .to_string(),
            ));
        };
        if query.r#type != QUERY_TYPE_TEXT {
            return Err(refuse(format!(
                "queries.0.type: {:?} is not a query content type this service offers. \
                 Supported types: {}",
                query.r#type,
                RERANK_QUERY_CONTENT_TYPE_VALUES.join(", ")
            )));
        }
        let Some(query_text) = query.text_query.text.as_deref() else {
            return Err(refuse(format!(
                "queries.0.textQuery.text is required for a {QUERY_TYPE_TEXT} query"
            )));
        };

        // One text per source, in the order the sources were given, so a source that
        // cannot be read is refused before any of them is scored.
        let mut documents: Vec<String> = Vec::with_capacity(input.sources.len());
        for (position, source) in input.sources.iter().enumerate() {
            if source.r#type != SOURCE_TYPE_INLINE {
                return Err(refuse(format!(
                    "sources.{position}.type: {:?} is not a source type this service offers. \
                     Supported types: {}",
                    source.r#type,
                    RERANK_SOURCE_TYPE_VALUES.join(", ")
                )));
            }
            documents.push(
                document_text(
                    &source.inline_document_source,
                    &format!("sources.{position}.inlineDocumentSource"),
                )
                .map_err(refuse)?,
            );
        }

        // Authorization is decided against the reranker model this call runs against —
        // the resource an identity-based policy for `bedrock:Rerank` names — and before
        // the ranking is computed.
        if let Err(denied) = crate::state::authorize(&*self.engine, ctx, Self::NAME, model_arn) {
            return Err(RerankErrorModeled::AccessDeniedException(denied).into());
        }

        // ONE permutation of the source list for the query: every source is scored
        // against it exactly once, and the score of a pair depends on that pair alone.
        let query_terms = QueryTerms::of(query_text);
        let mut ranked: Vec<(usize, f64)> = documents
            .iter()
            .enumerate()
            .map(|(position, text)| (position, relevance_score(&query_terms, text)))
            .collect();

        // Most relevant first. Two documents score the same only when they carry the
        // query's terms to the same extent — the measure is of the pair, so two
        // documents indistinguishable to it cannot be told apart by it either — and
        // those keep the order they were given in, so the permutation is total and one
        // request always answers the same way.
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(::std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });

        // `numberOfResults` keeps the leading n of that ranking. Absent, every source is
        // reported: dropping candidates the caller did not ask to have dropped would
        // lose results the caller sent, and the model states no smaller default.
        let kept = configuration
            .bedrock_reranking_configuration
            .number_of_results
            .and_then(|requested| usize::try_from(requested).ok())
            .filter(|requested| *requested > 0)
            .unwrap_or(ranked.len())
            .min(ranked.len());
        ranked.truncate(kept);

        Ok(RerankOutput {
            results: ranked
                .into_iter()
                .map(|(position, score)| RerankResult {
                    // The source's POSITION in the request's `sources` list, which is
                    // what identifies the document that was ranked.
                    index: i32::try_from(position).unwrap_or(i32::MAX),
                    relevance_score: score as f32,
                    // `document` is optional, and the caller supplied every document in
                    // this very request: the RANKING is what the call computes, so
                    // nothing is echoed back.
                    document: None,
                })
                .collect(),
            // See the `nextToken` refusal above: the whole ranking is in this response.
            next_token: None,
        })
    }
}

/// This operation's modelled `ValidationException` (HTTP 400). Every refusal it carries
/// states what the caller has to fix from the text alone: the member that was wrong and
/// what it requires, or — where the service words the refusal itself — that sentence
/// ([`INVALID_MODEL_ARN`]).
fn refuse(message: String) -> RerankError {
    RerankErrorModeled::ValidationException(ValidationException {
        message: Some(message),
    })
    .into()
}

/// The resolution the reranking `modelArn` must satisfy: it names a Bedrock FOUNDATION
/// MODEL — the resource a reranker is published as, whose ARN carries no account because
/// the service publishes it rather than an account creating it.
///
/// The modelled `BedrockModelArn` pattern is far wider than that (it admits a bare model
/// id, an account-scoped resource and a `sagemaker` ARN), so this is the handler's check
/// rather than the boundary validator's. Read through the ONE ARN parse this tree has
/// (`service_common::arn`), so this service cannot disagree with any other about what an
/// ARN is.
fn names_foundation_model(model_arn: &str) -> bool {
    let Some(parsed) = ::service_common::arn::parse(model_arn) else {
        return false;
    };
    if parsed.vendor != crate::state::ARN_NAMESPACE || !parsed.account.is_empty() {
        return false;
    }
    if ::service_common::arn::region_label(parsed.region).is_none() {
        return false;
    }
    ::service_common::arn::resource_id_of_type(model_arn, FOUNDATION_MODEL_RESOURCE_TYPE)
        .is_some_and(|model_id| !model_id.is_empty())
}

/// The resource type a foundation model's ARN addresses.
const FOUNDATION_MODEL_RESOURCE_TYPE: &str = "foundation-model";

/// The sentence a `modelArn` that names no Bedrock foundation model is refused with, as
/// AWS words it for this error class: ENTIRELY fixed text, stating the format the value
/// has to take (the angle-bracketed field names are part of the sentence, not values to
/// interpolate). The rejected value is not echoed and no member path is named.
const INVALID_MODEL_ARN: &str = "The provided model ARN for reranking is invalid. Please, use \
                                 a valid model ARN. Also check the model ARN follows this \
                                 format: arn:<partition>:bedrock:<region>::foundation-model/\
                                 <model-id>";

/// How sharply the reported score falls away as relevance does: the number of natural
/// logarithms between the score of a perfect match and the score of a document that
/// shares nothing with the query.
///
/// It is what puts the ranking on a LOGARITHMIC scale (see the module docs): a drop in
/// relevance DIVIDES the score rather than subtracting from it, so the gap between two
/// ranks grows with the gap in their relevance — a materially better match scores orders
/// of magnitude above a weak one, where a linear score would have crowded both into one
/// band. The figure is EMULATOR-CHOSEN — no reranker model runs here to publish one — and
/// only its size matters: it spreads the scale over about five decades.
const SCORE_DECAY: f64 = 12.0;

/// How much of the relevance is the share of the QUERY the document answers: the larger
/// part, because that is what the ordering is of — a document that carries more of what
/// the query asks about is the more relevant one, however long it is.
const COVERAGE_SHARE: f64 = 0.8;

/// How much of the relevance is the share of the DOCUMENT the query accounts for: the
/// smaller part, which separates a document that is ABOUT the query from a long one that
/// merely mentions it. It moves a pair's relevance by at most this much, so it decides
/// between documents whose coverage of the query is equal or close: a document covering
/// one more of the query's content terms than another still outranks it as long as the
/// query carries at most `COVERAGE_SHARE / DENSITY_SHARE` of them.
const DENSITY_SHARE: f64 = 1.0 - COVERAGE_SHARE;

/// The `relevanceScore` of one (query, document) pair: [`relevance`] on the logarithmic
/// scale of [`SCORE_DECAY`].
///
/// `1.0` exactly for a document that carries the whole query and nothing besides it, and
/// `exp(-SCORE_DECAY)` — a positive floor, never `0.0` — for one that shares no term with
/// it.
fn relevance_score(query: &QueryTerms, document: &str) -> f64 {
    let relevance = query.relevance(&stemmed_terms(document));
    (-SCORE_DECAY * (1.0 - relevance)).exp().clamp(0.0, 1.0)
}

/// The query, as the term set a document's relevance to it is measured against: what the
/// query ASKS ABOUT — its terms without the function words, which every document carries
/// alike and so distinguish none of them. A query of nothing BUT function words keeps
/// them all, so it still ranks its documents by the words it has rather than by none.
#[derive(Debug)]
struct QueryTerms {
    content: ::std::collections::BTreeSet<String>,
}

impl QueryTerms {
    /// The term set of `text`.
    fn of(text: &str) -> Self {
        let function_words = function_words();
        let content: ::std::collections::BTreeSet<String> = words(text)
            .filter(|word| !function_words.contains(word))
            .map(|word| stem(&word))
            .collect();
        let content = if content.is_empty() {
            stemmed_terms(text)
        } else {
            content
        };
        Self { content }
    }

    /// How relevant a document carrying `document` terms is to this query, in `[0, 1]`:
    /// [`COVERAGE_SHARE`] of the query's content terms the document carries, plus
    /// [`DENSITY_SHARE`] of the document's own terms that are those content terms. `1.0`
    /// exactly when the document carries every content term and carries nothing else;
    /// `0.0` when the two share no content term at all — the query's phrasing alone
    /// ranks nothing.
    fn relevance(&self, document: &::std::collections::BTreeSet<String>) -> f64 {
        if self.content.is_empty() || document.is_empty() {
            return 0.0;
        }
        let covered = self
            .content
            .iter()
            .filter(|term| document.contains(*term))
            .count();
        let coverage = covered as f64 / self.content.len() as f64;
        let density = covered as f64 / document.len() as f64;
        (COVERAGE_SHARE * coverage + DENSITY_SHARE * density).clamp(0.0, 1.0)
    }
}

/// The distinct stemmed terms of `text`.
fn stemmed_terms(text: &str) -> ::std::collections::BTreeSet<String> {
    words(text).map(|word| stem(&word)).collect()
}

/// The words of `text`: its maximal runs of alphanumeric characters, lowercased, so
/// matching is case- and punctuation-insensitive. Unicode-aware, because a document and
/// a query are free text in any script. Unstemmed, because a function word is named in
/// the published list by the word itself.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// One already-lowercased word's Snowball English stem.
///
/// A term is compared by its stem on BOTH sides, query and document, so a query term
/// matches whichever inflected form a document spells it in — a document about
/// "batteries" answers a query about a "battery".
fn stem(word: &str) -> String {
    stemmer().stem(word).into_owned()
}

/// The Snowball English stemmer, built once and shared.
fn stemmer() -> &'static ::rust_stemmers::Stemmer {
    static STEMMER: ::std::sync::OnceLock<::rust_stemmers::Stemmer> = ::std::sync::OnceLock::new();
    STEMMER.get_or_init(|| ::rust_stemmers::Stemmer::create(::rust_stemmers::Algorithm::English))
}

/// The English function words, lowercased: the published list the `stop-words` crate
/// carries, rather than a list written out here. Built once and shared.
fn function_words() -> &'static ::std::collections::BTreeSet<String> {
    static FUNCTION_WORDS: ::std::sync::OnceLock<::std::collections::BTreeSet<String>> =
        ::std::sync::OnceLock::new();
    FUNCTION_WORDS.get_or_init(|| {
        ::stop_words::get(::stop_words::LANGUAGE::English)
            .into_iter()
            .map(|word| word.to_lowercase())
            .collect()
    })
}

/// The text one source document carries, or the `ValidationException` message owed for
/// a document whose `type` and payload disagree.
///
/// The model makes both payload members optional while `type` is required, so the
/// boundary validator cannot refuse a `TEXT` document that carries no `textDocument` (or
/// a `JSON` one that carries no `jsonDocument`): that cross-member rule is this
/// handler's to enforce, and `path` is the member path of the document being read.
fn document_text(document: &RerankDocument, path: &str) -> Result<String, String> {
    match document.r#type.as_str() {
        DOCUMENT_TYPE_TEXT => document
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.text.clone())
            .ok_or_else(|| {
                format!("{path}.textDocument.text is required when type is {DOCUMENT_TYPE_TEXT}")
            }),
        DOCUMENT_TYPE_JSON => document
            .json_document
            .as_ref()
            .map(json_text)
            .ok_or_else(|| {
                format!("{path}.jsonDocument is required when type is {DOCUMENT_TYPE_JSON}")
            }),
        other => Err(format!(
            "{path}.type: {other:?} is not a document type this service offers. Supported \
             types: {}",
            RERANK_DOCUMENT_TYPE_VALUES.join(", ")
        )),
    }
}

/// The text a `JSON` source document carries: every object MEMBER NAME and every scalar
/// in it, joined by single spaces — the serialisation a reranker model is shown for a
/// JSON document, so the member names are part of its content.
///
/// Object members are walked in sorted key order, because the deserialized map is
/// unordered and one document must always yield one text; the walk stops at
/// [`MAX_DOCUMENT_DEPTH`], so a deeply nested document cannot drive unbounded recursion.
fn json_text(value: &JsonValue) -> String {
    fn walk(value: &JsonValue, depth: usize, out: &mut Vec<String>) {
        if depth > MAX_DOCUMENT_DEPTH {
            return;
        }
        match value {
            JsonValue::String(text) => out.push(text.clone()),
            JsonValue::Number(number) => out.push(match number {
                JsonNumber::PosInt(unsigned) => unsigned.to_string(),
                JsonNumber::NegInt(signed) => signed.to_string(),
                JsonNumber::Float(float) => float.to_string(),
            }),
            JsonValue::Bool(flag) => out.push(flag.to_string()),
            // A null member carries no text at all.
            JsonValue::Null => {}
            JsonValue::Array(items) => {
                for item in items {
                    walk(item, depth + 1, out);
                }
            }
            JsonValue::Object(members) => {
                let ordered: BTreeMap<&String, &JsonValue> = members.iter().collect();
                for (name, member) in ordered {
                    out.push(name.clone());
                    walk(member, depth + 1, out);
                }
            }
        }
    }

    let mut parts: Vec<String> = Vec::new();
    walk(value, 0, &mut parts);
    parts.join(" ")
}
