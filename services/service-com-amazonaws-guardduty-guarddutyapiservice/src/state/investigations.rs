// SPDX-License-Identifier: CC0-1.0

//! AI-analyst investigations: the record one investigation is held as, the
//! Regions the capability is offered in, and the ordering a listing reports them
//! in.

use super::*;

/// `InvestigationStatus`, the status a created investigation starts in: the
/// analysis is under way. One of `RUNNING | COMPLETED | FAILED`
/// (`smithy_query(shape="InvestigationStatus")`).
pub const INVESTIGATION_RUNNING: &str = "RUNNING";

/// The `InvestigationStatus` an investigation reaches when its analysis pass has
/// run: the terminal status of an accepted investigation, whose conclusions the
/// record then carries ([`InvestigationAnalysis`]). See [`INVESTIGATION_RUNNING`]
/// for the enum.
pub const INVESTIGATION_COMPLETED: &str = "COMPLETED";

/// How long a created investigation stays [`INVESTIGATION_RUNNING`] before the
/// clock owes it a terminal status.
///
/// GuardDuty documents no duration for an investigation, so this emulator CHOOSES
/// one. It is deliberately non-zero: a caller that reads the investigation back
/// immediately after creating it still observes the transient `RUNNING` status.
pub const INVESTIGATION_TRANSITION_MILLIS: i64 = 500;

/// The window the per-day investigation limit is counted over: "you can initiate
/// up to 10 investigations per account per day"
/// (`smithy_query(query="doc",shape="CreateInvestigation")`).
pub const INVESTIGATION_DAY_MILLIS: i64 = DAY_MILLIS;

/// How many investigations an account may initiate per day: "During the preview,
/// you can initiate up to 10 investigations per account per day"
/// (`smithy_query(query="doc",shape="CreateInvestigation")`).
pub const INVESTIGATIONS_PER_DAY: usize = 10;

/// How many investigations an account may initiate in total: "with a total limit
/// of 100 investigations per account"
/// (`smithy_query(query="doc",shape="CreateInvestigation")`).
pub const INVESTIGATIONS_PER_ACCOUNT: usize = 100;

/// The Regions GuardDuty Investigation is available in, as `CreateInvestigation`
/// and `GetInvestigation` both list them: "US East (N. Virginia), US East (Ohio),
/// US West (Oregon), Canada (Central), Europe (Frankfurt), Europe (Ireland),
/// Europe (London), Europe (Paris), Europe (Stockholm), and Asia Pacific (Tokyo)"
/// (`smithy_query(query="doc",shape="CreateInvestigation")`), in that order.
pub const INVESTIGATION_REGIONS: [&str; 10] = [
    "us-east-1",
    "us-east-2",
    "us-west-2",
    "ca-central-1",
    "eu-central-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-north-1",
    "ap-northeast-1",
];

/// Is GuardDuty Investigation available through `region`'s endpoint? Decided from
/// the documented [`INVESTIGATION_REGIONS`] catalogue, not from what the store
/// happens to hold.
pub fn investigations_available_in(region: &str) -> bool {
    INVESTIGATION_REGIONS.contains(&region)
}

/// The message a request to a Region the feature is not available in is refused
/// with. It does not name the caller's Region: the endpoint already carries it.
pub const INVESTIGATION_REGION_MESSAGE: &str =
    "The request is rejected because GuardDuty Investigation is not available in the current Region.";

/// The message `GetInvestigation` answers an `InvestigationId` this detector does
/// not hold with: the Investigation family's own not-found sentence, a short fixed
/// text naming neither the operation, nor the member that carried the id, nor the
/// id itself — GuardDuty does not echo the caller's id back.
pub const INVESTIGATION_NOT_FOUND_MESSAGE: &str = "Resource not found.";

/// The message `CreateInvestigation` refuses a detector that does not have the
/// AI_ANALYST feature enabled with — "to use this operation, the AI_ANALYST feature
/// must be enabled on your detector"
/// (`smithy_query(query="doc",shape="CreateInvestigation")`).
///
/// Entirely FIXED text, naming no account and no detector: it states the
/// prerequisite and how to meet it, and nothing of the request is interpolated.
pub const INVESTIGATION_FEATURE_DENIED_MESSAGE: &str =
    "The request is rejected because the AI Analyst feature is not enabled for the delegated administrator account. Enable the AI_ANALYST feature on your delegated administrator's detector to use the Investigation API.";

/// The message a request past the documented per-day investigation limit is
/// refused with.
pub fn investigation_daily_limit_message() -> String {
    format!(
        "The request is rejected because the account has reached the limit of {INVESTIGATIONS_PER_DAY} investigations per day."
    )
}

/// The message a request past the documented total investigation limit is refused
/// with.
pub fn investigation_total_limit_message() -> String {
    format!(
        "The request is rejected because the account has reached the limit of {INVESTIGATIONS_PER_ACCOUNT} investigations."
    )
}

/// `CloudProvider`'s only member — "The cloud provider. Currently, only `AWS` is
/// supported" (`smithy_query(query="doc",shape="CloudDetails",member="Provider")`).
pub const CLOUD_PROVIDER_AWS: &str = "AWS";
const _: () = assert!(enum_carries(
    crate::ops::types::shared_types::CLOUD_PROVIDER_VALUES,
    CLOUD_PROVIDER_AWS
));

/// The cloud environment ONE investigation covered, held on that investigation's own
/// record: the three `CloudDetails` members — the provider, the Region and the
/// account of the environment the analysis pass ran over
/// (`smithy_query(shape="CloudDetails")`).
///
/// It is resolved exactly once, when the pass ENDS
/// ([`InvestigationRecord::settle`]), and read back from the record afterwards, so
/// the environment a reader reports is the one THIS investigation covered rather
/// than anything about the request the read arrives on: an investigation that has
/// not finished its pass has resolved no environment and holds none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationCloud {
    /// `CloudDetails$Provider` — see [`CLOUD_PROVIDER_AWS`].
    pub provider: String,
    /// `CloudDetails$Region`: the Region whose resources the pass covered.
    pub region: String,
    /// `CloudDetails$Account`: the account whose resources the pass covered.
    pub account: String,
}

/// One stored investigation. Timestamps are epoch milliseconds, the single source
/// for the instants the wire carries.
#[derive(Debug, Clone)]
pub struct InvestigationRecord {
    pub id: String,
    /// `TriggerPrompt`: what the caller asked to be investigated, stored verbatim.
    pub trigger_prompt: String,
    /// `TriggeredBy`: "The account that initiated the investigation"
    /// (`smithy_query(shape="Investigation")`).
    pub triggered_by: String,
    /// The Region the investigation was initiated through.
    pub region: String,
    /// The `ClientToken` the create carried, so a replay returns this same
    /// investigation instead of initiating a second one.
    pub client_token: Option<String>,
    /// `InvestigationStatus`.
    pub status: String,
    pub started_at_millis: i64,
    /// When the clock owes this investigation its terminal status. See
    /// [`INVESTIGATION_TRANSITION_MILLIS`].
    pub terminal_due_millis: i64,
    /// When it reached a terminal status, once it has.
    pub ended_at_millis: Option<i64>,
    /// What the analysis pass concluded, once it has run. See
    /// [`InvestigationAnalysis`], and [`Self::settle`] for when it is computed.
    pub analysis: Option<InvestigationAnalysis>,
    /// `Cloud`: the environment this investigation's pass covered, once the pass has
    /// ended and resolved it. See [`InvestigationCloud`].
    pub cloud: Option<InvestigationCloud>,
}

impl InvestigationRecord {
    /// An investigation initiated at `now`, still [`INVESTIGATION_RUNNING`].
    pub fn started(
        id: String,
        trigger_prompt: String,
        triggered_by: String,
        region: String,
        client_token: Option<String>,
        now: i64,
    ) -> Self {
        Self {
            id,
            trigger_prompt,
            triggered_by,
            region,
            client_token,
            status: INVESTIGATION_RUNNING.to_string(),
            started_at_millis: now,
            terminal_due_millis: now.saturating_add(INVESTIGATION_TRANSITION_MILLIS),
            ended_at_millis: None,
            // The pass has not run yet, so it has reached no conclusions.
            analysis: None,
            // The pass has not run yet, so it has resolved no environment: a reader
            // reports no `Cloud` member for an investigation still
            // [`INVESTIGATION_RUNNING`].
            cloud: None,
        }
    }

    /// The `Cloud` member a reader reports for this investigation: the environment
    /// stored on the record, and NOTHING while the record has resolved none.
    pub fn cloud_details(&self) -> Option<crate::ops::types::CloudDetails> {
        self.cloud
            .as_ref()
            .map(|cloud| crate::ops::types::CloudDetails {
                provider: cloud.provider.clone(),
                region: cloud.region.clone(),
                account: cloud.account.clone(),
            })
    }

    /// The `Metadata` member a reader reports for this investigation: the product
    /// and engine version that produced its conclusions, and NOTHING while no pass
    /// has produced any.
    pub fn metadata(&self) -> Option<crate::ops::types::InvestigationMetadata> {
        self.analysis
            .as_ref()
            .map(|_| InvestigationAnalysis::metadata())
    }

    /// Apply the status change the CLOCK already made due, if any, and with it the
    /// conclusions of the analysis pass that change ends.
    ///
    /// The move out of `RUNNING` was owed at [`Self::terminal_due_millis`] whether
    /// or not anybody called, which is why a reporting operation may apply it. It
    /// is applied exactly once: a terminal investigation never moves again.
    ///
    /// `evidence` is the findings the detector holds ([`FindingEvidence`]); the pass
    /// reads the ones that existed when it ENDED, so the assessment it stores is the
    /// one the service owed at that instant and does not depend on when a caller
    /// first asked for it.
    pub fn settle(&mut self, now: i64, evidence: &[FindingEvidence]) {
        if self.status != INVESTIGATION_RUNNING || now < self.terminal_due_millis {
            return;
        }
        let covered: Vec<FindingEvidence> = evidence
            .iter()
            .filter(|finding| finding.created_at_millis <= self.terminal_due_millis)
            .cloned()
            .collect();
        let covered = evidence_for_prompt(&self.trigger_prompt, &covered);
        self.analysis = Some(InvestigationAnalysis::of_evidence(
            &covered,
            &self.triggered_by,
            &self.region,
        ));
        self.status = INVESTIGATION_COMPLETED.to_string();
        self.ended_at_millis = Some(self.terminal_due_millis);
        // The pass covered ONE environment — the account the investigation was
        // initiated for, through the Region its endpoint served, both persisted on
        // this record when the create minted it — so that environment is resolved
        // the instant the pass ends. Storing it here is what lets every later read
        // report the same provider/Region/account for this investigation, whatever
        // endpoint or caller the read arrives on.
        self.cloud = Some(InvestigationCloud {
            provider: CLOUD_PROVIDER_AWS.to_string(),
            region: self.region.clone(),
            account: self.triggered_by.clone(),
        });
    }
}

/// `OrderBy`, the order a sort runs in: the enum is `ASC | DESC`
/// (`smithy_query(shape="OrderBy")`), the two values the generated
/// [`crate::ops::types::shared_types::ORDER_BY_VALUES`] list carries. Every sort
/// in this service reads the member through these two constants, so none of them
/// can spell the order differently.
pub const ORDER_BY_ASCENDING: &str = "ASC";

/// The other `OrderBy` member — see [`ORDER_BY_ASCENDING`].
pub const ORDER_BY_DESCENDING: &str = "DESC";

/// The attributes a `ListInvestigations` sort may name: the
/// `InvestigationSortField` members, which the model lists as `START_TIME |
/// END_TIME | STATUS | RISK_LEVEL | CONFIDENCE`
/// (`smithy_query(shape="InvestigationSortField")`) and the generated enum carries
/// as [`crate::ops::types::shared_types::INVESTIGATION_SORT_FIELD_VALUES`]. Read
/// from the generated list so the two can never drift.
pub fn investigation_sort_fields() -> &'static [&'static str] {
    crate::ops::types::shared_types::INVESTIGATION_SORT_FIELD_VALUES
}

/// The attribute a `ListInvestigations` sort orders by when the request names
/// none: when the investigation started, which with [`DESCENDING_BY_DEFAULT`] puts
/// the newest investigation first — the order the other reporting operations of
/// this service answer an unordered request in.
pub const INVESTIGATION_DEFAULT_SORT_FIELD: &str = "START_TIME";

/// Resolve one investigation attribute by its `InvestigationSortField` name, or
/// `None` when that is not a member of the enum.
///
/// `RISK_LEVEL` and `CONFIDENCE` are the analysis's conclusions, so they resolve to
/// the RANK of the stored member on its own scale ([`risk_level_rank`],
/// [`confidence_rank`]) rather than to its spelling: ordering by a graded enum is
/// ordering by the grade, and an investigation whose pass has reached no conclusion
/// ranks below every one that has.
pub fn investigation_attribute(investigation: &InvestigationRecord, field: &str) -> Option<Attribute> {
    let analysis = investigation.analysis.as_ref();
    let value = match field {
        "START_TIME" => Attribute::Number(investigation.started_at_millis as f64),
        "END_TIME" => Attribute::Number(investigation.ended_at_millis.unwrap_or(0) as f64),
        "STATUS" => Attribute::Text(investigation.status.clone()),
        "RISK_LEVEL" => Attribute::Number(
            analysis.map_or(UNRANKED, |analysis| risk_level_rank(&analysis.risk_level)),
        ),
        "CONFIDENCE" => Attribute::Number(
            analysis.map_or(UNRANKED, |analysis| confidence_rank(&analysis.confidence)),
        ),
        _ => return None,
    };
    Some(value)
}

/// Order `investigations` for `ListInvestigations`.
///
/// With no `SortCriteria` the newest investigation comes first, with the
/// investigation id breaking ties so the order is TOTAL and a page token means the
/// same thing on the call that hands it back. `Err(attribute)` means the request
/// named something that is not an `InvestigationSortField` member.
pub fn sort_investigations(
    investigations: &mut [&InvestigationRecord],
    sort: Option<&crate::ops::types::InvestigationSortCriteria>,
) -> Result<(), String> {
    let attribute = sort
        .and_then(|s| s.attribute_name.as_deref())
        .filter(|a| !a.is_empty());
    let descending = match sort.and_then(|s| s.order_by.as_deref()) {
        Some(ORDER_BY_ASCENDING) => false,
        Some(ORDER_BY_DESCENDING) => true,
        // No explicit order: newest-first for the default attribute, ascending for
        // an attribute the caller named — the rule `sort_malware_scans` follows.
        _ => attribute.is_none() && DESCENDING_BY_DEFAULT,
    };
    let attribute = attribute.unwrap_or(INVESTIGATION_DEFAULT_SORT_FIELD);
    if !investigation_sort_fields().contains(&attribute) {
        return Err(attribute.to_string());
    }

    investigations.sort_by(|a, b| {
        compare_by_attribute(
            *a,
            *b,
            descending,
            |investigation| investigation_attribute(investigation, attribute),
            |investigation| investigation.id.clone(),
        )
    });
    Ok(())
}
