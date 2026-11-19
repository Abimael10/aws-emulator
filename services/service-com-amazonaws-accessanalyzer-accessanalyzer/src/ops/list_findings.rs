// SPDX-License-Identifier: CC0-1.0

//! @op ListFindings
//!
//! Reports the external-access findings of the analyzer the request names, COMPUTED from
//! the resource-based policies this emulator holds for the resources of the analyzer's own
//! account: a finding is the access one `Allow` statement of such a policy grants to a
//! principal outside that account.
//!
//! Nothing is stored for a finding whose access is still granted. Every such finding is
//! derived on each read from the policies their owning services hold
//! ([`crate::state::monitored_resources`]) by the crate's ONE external-access analysis
//! ([`crate::state::external_access_findings`]), so two reads of one analyzer report the same
//! findings with the same ids — the id is DERIVED from what it identifies
//! ([`crate::state::derived_uuid`]) rather than minted, which is what makes it stable without
//! a stored table of ids.
//!
//! What a policy cannot state is the finding of an access it NO LONGER grants. A finding the
//! analyzer reported for an access that has since been removed is `RESOLVED`, not gone, so
//! the analyzer keeps a record of every finding it has reported
//! ([`crate::state::ReportedFinding`]) and this listing reports those records too. The
//! analyzer has been analyzing since it was created, so the comparison of its last analysis
//! with the account's current policies is owed whatever a caller does, and this operation
//! materialises it ([`crate::state::reconcile_reported_findings`]); reconciling twice over
//! the same policies records nothing further, so one listing reports what the listing before
//! it did.
//!
//! ## What a finding is
//!
//! The analysis and its rule live in [`crate::state`], shared with the access-preview
//! analysis so the two cannot drift: one finding per principal outside the zone of trust
//! (for an account analyzer, the account that owns the resource) that an `Allow` statement
//! grants access to, marked `isPublic` exactly when the statement admits every principal
//! with nothing in its `Condition` binding the caller. Each finding carries the statement's
//! own `Action` list and `Condition` keys, and `status` `ACTIVE` while the statement still
//! grants the access, `RESOLVED` once it does not.
//!
//! ## What this operation adds to it
//!
//! * `analyzerArn` must name a stored analyzer (404 otherwise), and that analyzer must be
//!   an EXTERNAL access analyzer: "ListFindings is supported only for external access
//!   analyzers. You must use ListFindingsV2 for internal and unused access analyzers", so a
//!   request naming an analyzer of the unused-access or internal-access family is refused
//!   with the service's fixed `ValidationException` for an unsupported action rather than
//!   answered with findings it does not report.
//! * `filter` selects the findings a criterion holds for, over the members of a finding;
//!   `sort` orders them by one of those members; `maxResults`/`nextToken` page over the
//!   result, so a caller that follows tokens to exhaustion sees every finding once.
//!
//! ## What bounds the work
//!
//! A finding whose access is still granted is not stored, so EVERY call re-derives it from
//! the account's policies and then matches it against the request's `filter`. The policy
//! parser bounds each factor of that work (document bytes, statements, element values) but
//! not their product, and it is the product that decides how much there is to do — so the
//! whole analysis is bounded: at most [`crate::state::MAX_MONITORED_RESOURCES`] resources
//! read, at most [`MAX_DERIVED_FINDINGS`] findings, at most
//! [`crate::state::MAX_RESOLVED_FINDINGS_PER_ANALYZER`] records of findings whose access is
//! gone, at most [`crate::state::MAX_FILTER_CRITERIA`] criteria, and at most
//! [`MAX_ANALYSIS_UNITS`] units of work charged for every policy read, statement visited,
//! principal considered, string copied into a finding and
//! [`crate::state::FILTER_BYTES_PER_UNIT`] bytes compared by a criterion. A call
//! past any of those ceilings is refused with the modelled `ValidationException` naming the
//! member to change rather than run. Both stages run on the blocking pool with every store
//! lock already released, exactly as `CheckNoNewAccess` runs its policy comparison, so a call
//! at the top of its budget cannot starve the async worker or hold up another request.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `ListFindings::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ListFindings`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-accessanalyzer-accessanalyzer/smithy-model/com-amazonaws-accessanalyzer-accessanalyzer.json")
//! ```
//!
//! Retrieves a list of findings generated by the specified analyzer. ListFindings and
//! ListFindingsV2 both use access-analyzer:ListFindings in the Action element of an IAM
//! policy statement. You must have permission to perform the access-analyzer:ListFindings
//! action. To learn about filter keys that you can use to retrieve a list of findings, see
//! IAM Access Analyzer filter keys in the IAM User Guide. ListFindings is supported only
//! for external access analyzers. You must use ListFindingsV2 for internal and unused
//! access analyzers.
//!
//! ## Input
//!
//! * `analyzerArn` (AnalyzerArn, pattern ^[^:]*:[^:]*:[^:]*:[^:]*:[^:]*:analyzer/.{1,255}$)
//!   **required** — The ARN of the analyzer to retrieve findings from.
//! * `filter` (FilterCriteriaMap) — A filter to match for the findings to return.
//! * `maxResults` (Integer) — The maximum number of results to return in the response.
//! * `nextToken` (Token) — A token used for pagination of results returned.
//! * `sort` (SortCriteria) — The sort order for the findings returned.
//!
//! ## Output
//!
//! * `findings` (FindingsList) **required** — A list of findings retrieved from the analyzer
//!   that match the filter criteria specified, if any.
//! * `nextToken` (Token) — A token used for pagination of results returned.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You do not have sufficient access to perform this
//!   action.
//! * `InternalServerException` (HTTP 500) — Internal server error.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource could not be found.
//! * `ThrottlingException` (HTTP 429) — Throttling limit exceeded error.
//! * `ValidationException` (HTTP 400) — Validation exception error.

#[allow(unused_imports)]
use super::types::*;

// What bounds one call's analysis, shared with the access-preview analysis so the two
// cannot answer the same question differently.
use crate::state::{Budget, Exhausted};

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListFindingsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-accessanalyzer-accessanalyzer.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListFindingsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.accessanalyzer#ListFindings")]
#[derive(Debug)]
pub enum ListFindingsErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_findings::ListFindingsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListFindingsError = crate::scaffolded::Scaffolded<ListFindingsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListFindingsErrorModeled> for ListFindingsError {
    fn from(modeled: ListFindingsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListFindings` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListFindings<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListFindings<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListFindings<Eng>
{
    type Input = ListFindingsInput;
    type Output = ListFindingsOutput;
    type Error = ListFindingsError;

    const NAME: &'static str = "ListFindings";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_FINDINGS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_FINDINGS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The findings are a function of the account's stored resource policies, the
        // analyzer's own record of what it has reported and the clock, so nothing of the
        // request metadata is consulted: what this call writes is the analysis the analyzer
        // owes (below), never anything about the caller.
        let _ = ctx;

        // The filter and the sort are refused before any store is read: a criterion naming
        // no member of a finding cannot be applied to any finding, whatever the account
        // holds.
        // The criteria are read by the crate's ONE filter match, shared with the archive
        // rule that archives the findings a filter selects, so a key this listing accepts is
        // a key that rule accepts too.
        let criteria = crate::state::finding_filter_criteria(input.filter.as_ref())
            .map_err(ListFindingsErrorModeled::ValidationException)?;
        let sort = sort_order(input.sort.as_ref())?;

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            ListFindingsErrorModeled::InternalServerException(crate::state::store_unavailable())
        })?;
        // The analyzer is read under this service's lock, which is RELEASED (the block ends
        // here) before any other store is read or any analysis runs.
        let (analyzer_arn, analyzer_created_at) = {
            let state = state.lock();

            // Fixed text, the ARN the caller named in the modelled `resourceId`: every
            // resource-scoped refusal in this service goes through `crate::state`'s
            // constructors.
            let analyzer = state.analyzer_by_arn(&input.analyzer_arn).ok_or_else(|| {
                ListFindingsErrorModeled::ResourceNotFoundException(
                    crate::state::analyzer_not_found(&input.analyzer_arn),
                )
            })?;

            // "ListFindings is supported only for external access analyzers. You must use
            // ListFindingsV2 for internal and unused access analyzers" — an analyzer of
            // either configured family reports no external access, so the request is
            // refused rather than answered with an empty list, which would claim the
            // account grants none. The refusal is about the analyzer the request names
            // rather than about one malformed member, so it carries no `fieldList` and its
            // message is the service's fixed text for an unsupported action; see
            // [`crate::state::unsupported_action_exception`], which `StartResourceScan`
            // refuses the same family with.
            if analyzer.configured_access().is_some() {
                return Err(ListFindingsErrorModeled::ValidationException(
                    crate::state::unsupported_action_exception(),
                )
                .into());
            }

            (analyzer.arn.clone(), analyzer.created_at)
        };

        // The zone of trust of an account analyzer is the account it monitors and the
        // Region it monitors it in, both of which its own ARN names; the resources of that
        // account are read out of the services that own them, each under its own lock and
        // none of them held past the read.
        let resources = crate::state::monitored_resources(&*self.engine, &analyzer_arn);

        // The whole analysis — deriving the findings from the stored policies, matching
        // them against the request's criteria and ordering them — reads documents,
        // allocates findings and compares strings, so it is CPU-bound: it runs on the
        // blocking pool, the way `CheckNoNewAccess` sends its policy comparison there,
        // under ONE budget of [`MAX_ANALYSIS_UNITS`], which is carried from the derivation
        // into the match. The budget bounds the work and the allocation of both stages
        // together; the pool keeps them off the async worker that serves health checks and
        // every other request. What is left on the worker is reconciling the analyzer's own
        // records with the derivation and taking one page (at most
        // [`crate::state::MAX_PAGE_SIZE`] items) out of the result.
        let analysis_arn = analyzer_arn.clone();
        let derivation = ::tokio::task::spawn_blocking(move || {
            let mut budget = Budget::new(MAX_ANALYSIS_UNITS);
            crate::state::external_access_findings(
                &analysis_arn,
                analyzer_created_at,
                &resources,
                MAX_DERIVED_FINDINGS,
                &mut budget,
            )
            .map(|findings| (findings, budget))
        })
        .await;
        let (derived, mut budget) = match derivation {
            Ok(Ok(derived)) => derived,
            Ok(Err(exhausted)) => return Err(budget_refusal(Self::NAME, exhausted)),
            Err(error) => return Err(analysis_fault(Self::NAME, &error)),
        };

        // The analyzer has been analyzing since it was created, so the comparison of its
        // last analysis with the account's policies as they are now is owed whatever this
        // call does: it is materialised here, which is what makes a finding whose access is
        // no longer granted reported as the resolved finding it is rather than dropped from
        // the listing. Reconciling twice over the same policies records nothing further, so
        // this listing reports what the listing before it did. Bounded work under the lock:
        // the derivation holds at most [`MAX_DERIVED_FINDINGS`] findings and the analyzer at
        // most [`crate::state::MAX_RESOLVED_FINDINGS_PER_ANALYZER`] resolved records.
        let reported = {
            let mut state = state.lock();
            let analyzer = state.analyzer_by_arn_mut(&analyzer_arn).ok_or_else(|| {
                ListFindingsErrorModeled::ResourceNotFoundException(
                    crate::state::analyzer_not_found(&input.analyzer_arn),
                )
            })?;
            crate::state::reconcile_reported_findings(
                &mut analyzer.reported_findings,
                derived,
                ::std::time::SystemTime::now(),
            )
        };

        let selected = ::tokio::task::spawn_blocking(move || {
            let matching = crate::state::findings_matching_filter(reported, &criteria, &mut budget)?;
            Ok::<_, Exhausted>(ordered(matching, sort.as_ref()))
        })
        .await;
        let matching = match selected {
            Ok(Ok(matching)) => matching,
            // The account's policies are analyzable, but reporting their findings would
            // cost more than this emulator spends on one call: refused, naming the request
            // member at fault, rather than allocating further.
            Ok(Err(exhausted)) => return Err(budget_refusal(Self::NAME, exhausted)),
            // A panic or a cancellation on the pool says nothing about the account's
            // policies, so it is reported as the server fault it is rather than as an empty
            // result.
            Err(error) => return Err(analysis_fault(Self::NAME, &error)),
        };

        let page = crate::state::page(matching, input.max_results, input.next_token.as_deref())
            .map_err(|error| {
                ListFindingsErrorModeled::ValidationException(
                    crate::state::page_validation_exception(&error),
                )
            })?;

        Ok(ListFindingsOutput {
            findings: page.items,
            next_token: page.next_token,
        })
    }
}

// ---------------------------------------------------------------------------
// What a call that cannot be answered is refused with
// ---------------------------------------------------------------------------

/// The refusal for an analysis past one of this emulator's ceilings: the modelled
/// `ValidationException`, naming the request member a client narrows to get under it.
///
/// One constructor for both stages of the analysis — the derivation and the match — so a
/// refusal reads the same whichever of them ran out of budget.
fn budget_refusal(operation: &'static str, exhausted: Exhausted) -> ListFindingsError {
    ::tracing::info!(
        operation,
        refusal = "budget",
        "refused a findings listing past this emulator's analysis ceiling"
    );
    ListFindingsErrorModeled::ValidationException(crate::state::field_validation_exception(
        exhausted.member,
        exhausted.detail,
    ))
    .into()
}

/// The refusal for an analysis that did not complete at all: a panic or a cancellation on
/// the blocking pool says nothing about the account's policies, so it is the server fault it
/// is rather than an empty result.
fn analysis_fault(operation: &'static str, error: &::tokio::task::JoinError) -> ListFindingsError {
    ::tracing::error!(
        operation,
        error = %error,
        "the external access analysis did not complete"
    );
    ListFindingsErrorModeled::InternalServerException(InternalServerException {
        message: "the external access analysis did not complete".to_string(),
        retry_after_seconds: None,
    })
    .into()
}

// ---------------------------------------------------------------------------
// What one call of this operation may spend
// ---------------------------------------------------------------------------

/// How many findings this emulator will DERIVE for ONE analyzer.
///
/// A finding whose access is still granted is not stored, so every call re-derives it: this
/// is the emulator's OWN ceiling on that work, generous beside the ten full pages
/// ([`crate::state::MAX_PAGE_SIZE`]) a realistic listing walks, and far below what the
/// account's stored policies could otherwise ask for — the policy parser bounds each factor
/// (documents, statements, element values) but not their PRODUCT, and it is the product
/// that decides how many findings exist. The records of findings whose access is GONE are
/// bounded separately, by [`crate::state::MAX_RESOLVED_FINDINGS_PER_ANALYZER`].
const MAX_DERIVED_FINDINGS: usize = 1_000;

/// How much work ONE call's analysis may spend: deriving the findings AND matching them
/// against the request's criteria, under one ceiling.
///
/// A unit is one policy read, one statement visited, one principal considered, one string
/// copied into a finding, or [`crate::state::FILTER_BYTES_PER_UNIT`] bytes compared by a filter
/// criterion — so this bounds the ALLOCATION and the byte-level work of both stages, in the
/// PRODUCT of their factors rather than in each factor alone, whatever the account's
/// policies and the request's filter look like. The counterpart of the policy engine's own
/// budgets (`check_no_new_access::policy::MAX_ACCESS_REQUESTS` and its
/// `MAX_COMPARISON_STEPS`), which bound the comparison `CheckNoNewAccess` runs.
const MAX_ANALYSIS_UNITS: u64 = 500_000;

/// The request member a sort refusal names: the attribute the request asked to order by.
const SORT_ATTRIBUTE_MEMBER: &str = "sort.attributeName";

// ---------------------------------------------------------------------------
// `sort`: in what order the findings are returned
// ---------------------------------------------------------------------------

/// What the request asked the findings to be ordered by: which attribute, and in which
/// direction.
#[derive(Debug)]
struct SortOrder {
    attribute: SortAttribute,
    /// `true` for `DESC`, the model's other `OrderBy` value.
    descending: bool,
}

/// The attribute a listing is ordered by: a member a filter also reads, one of the
/// finding's three instants, which order by the instant and not by its rendering, or the
/// order the analysis derived.
#[derive(Debug)]
enum SortAttribute {
    Member(crate::state::FindingMember),
    Instant(fn(&FindingSummary) -> ::aws_smithy_types::DateTime),
    /// What an order that names no attribute orders by: nothing of the finding, so every
    /// finding keys the same and the derivation order stands.
    Derivation,
}

/// The three instants a finding carries, each sortable by its own value. From the model's
/// `FindingSummary` members.
const SORTABLE_INSTANTS: &[&str] = &["createdAt", "analyzedAt", "updatedAt"];

/// The `OrderBy` value that reverses the order, from the model's enum
/// ([`ORDER_BY_VALUES`]); the other one is the ascending order a request that names none
/// gets.
const ORDER_BY_DESCENDING: &str = "DESC";

impl SortAttribute {
    /// The attribute `name` addresses, or `None` when it addresses no member of a finding.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "createdAt" => Some(Self::Instant(|finding| finding.created_at)),
            "analyzedAt" => Some(Self::Instant(|finding| finding.analyzed_at)),
            "updatedAt" => Some(Self::Instant(|finding| finding.updated_at)),
            _ => crate::state::FindingMember::parse(name).map(Self::Member),
        }
    }

    /// The key `finding` sorts by: an instant as a fixed-width epoch (so it orders by the
    /// instant), and any other member as its values in order, joined — a member holding
    /// none sorts before every member that holds one, which is where a finding that does
    /// not carry the attribute belongs.
    fn key(&self, finding: &FindingSummary) -> String {
        match self {
            Self::Derivation => String::new(),
            Self::Instant(read) => {
                let at = read(finding);
                format!("{:020}.{:09}", at.secs(), at.subsec_nanos())
            }
            Self::Member(member) => {
                let mut held = member.values(finding);
                held.sort_unstable();
                held.join("\u{1f}")
            }
        }
    }
}

/// Read the request's `sort` into the order the findings are reported in.
///
/// `None` when the request names no sort: the findings are then reported in the order the
/// analysis derived them (resource, then statement, then principal), which the model
/// documents no alternative default for. An `attributeName` naming no member of a finding is
/// refused with the operation's own `ValidationException` naming it; `orderBy` carries the
/// model's `OrderBy` enum, so a value outside it is already refused before this handler
/// runs and what reaches here is one of the two the enum lists.
fn sort_order(sort: Option<&SortCriteria>) -> Result<Option<SortOrder>, ListFindingsError> {
    let Some(sort) = sort else {
        return Ok(None);
    };
    // The model gives the member no default, so a request that names no order is ordered
    // ascending — the direction the analysis derives in.
    let descending = sort.order_by.as_deref() == Some(ORDER_BY_DESCENDING);
    let Some(name) = sort.attribute_name.as_deref() else {
        // An order without an attribute names nothing to order BY, so the derivation order
        // stands; a `DESC` of it is still a reversal, which is what the caller asked for.
        return Ok(Some(SortOrder {
            attribute: SortAttribute::Derivation,
            descending,
        }));
    };
    let attribute = SortAttribute::parse(name).ok_or_else(|| {
        ListFindingsError::from(ListFindingsErrorModeled::ValidationException(
            crate::state::field_validation_exception(
                SORT_ATTRIBUTE_MEMBER,
                format!(
                    "sort.attributeName {name:?} names no member of a finding; the attributes a \
                     listing can be ordered by are {}, {}, and a map-valued member addressed at \
                     one of its keys (principal.<principal type>, condition.<condition key>)",
                    crate::state::FILTERABLE_FINDING_MEMBERS.join(", "),
                    SORTABLE_INSTANTS.join(", ")
                ),
            ),
        ))
    })?;
    Ok(Some(SortOrder {
        attribute,
        descending,
    }))
}

/// The findings in the order the request asked for, each paired with the key the page is
/// taken by.
///
/// The key is the finding's POSITION in that order, so the continuation token names the
/// place in the ordered result a caller resumes at whatever the order was — the derivation
/// key cannot do that once a sort reorders the findings. Sorting needs no budget of its
/// own: it runs over at most [`MAX_DERIVED_FINDINGS`] keys, each built from members the
/// policy parser has already bounded.
fn ordered(
    findings: Vec<(String, FindingSummary)>,
    sort: Option<&SortOrder>,
) -> Vec<(String, FindingSummary)> {
    let mut findings = findings;
    if let Some(sort) = sort {
        // Keyed once per finding rather than per comparison, and the derivation key breaks
        // a tie, so two findings with the same attribute value keep a stable order.
        let mut keyed: Vec<(String, String, FindingSummary)> = findings
            .into_iter()
            .map(|(derived, finding)| (sort.attribute.key(&finding), derived, finding))
            .collect();
        keyed.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        if sort.descending {
            keyed.reverse();
        }
        findings = keyed
            .into_iter()
            .map(|(_, derived, finding)| (derived, finding))
            .collect();
    }
    findings
        .into_iter()
        .enumerate()
        .map(|(position, (_, finding))| (format!("{position:08}"), finding))
        .collect()
}
