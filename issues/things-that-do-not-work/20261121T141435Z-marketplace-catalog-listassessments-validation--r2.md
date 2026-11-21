Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 8c92c7c2cb0689aef734153d4944deecfb6a59a7

# A FrameworkFilters union member that contradicts the request's FrameworkId is accepted and silently answers an empty page

_Filed: 2026-11-21T14:14:35Z_
_Operation: ListAssessments | Class: validation_

> _Note: this issue was identified against the code base at commit `8c92c7c2cb0689aef734153d4944deecfb6a59a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListAssessments` applies `FrameworkId` and `FrameworkFilters` as two INDEPENDENT, conjoined predicates. Each member of the `FrameworkFilters` union belongs to exactly one framework (`AMISecurityFilters` -> AMISecurity, `ContainerSecurityFilters` -> ContainerSecurity), so a request that names `FrameworkId: "ContainerSecurity"` together with `FrameworkFilters: {"AMISecurityFilters": {...}}` asks for assessments of two different frameworks at once — a request that contradicts itself. The handler neither refuses it nor reconciles it: `matches_framework_id` demands one framework and `matches_framework_filters` demands the other, so the conjunction can never hold and the call answers HTTP 200 with an empty `AssessmentSummaryList`.

That is the behaviour this crate refuses everywhere else. Its sibling list handler, `ListEntities`, judges exactly the analogous pairing — a per-type filter union member against the type being listed — and REFUSES a mismatch with the modelled `ValidationException`. And the crate's own filter rule states the principle in words: a value naming nothing the catalog has is "refused ... rather than silently selecting the empty page it would match". `ListAssessments` itself applies that rule to its other two members (`FrameworkId` is refused when it names no framework of this catalog; `AssessmentTargetFilter$EntityId` is refused when it is not an entity id) and then drops it for the one member whose own documentation says which framework it corresponds to.

## Evidence

Smithy model (smithy_query query="shape" shape="FrameworkFilters"):
  "Framework-specific filters used to scope <code>ListAssessments</code> results. Set exactly one member, corresponding to the framework you want to filter by."

The two predicates are merely conjoined — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/list_assessments.rs:264-281:
  "let mut matched: Vec<&crate::state::AssessmentRecord> = store
       .assessments
       .values()
       .filter(|record| {
           input
               .framework_id
               .as_deref()
               .is_none_or(|framework_id| record.matches_framework_id(framework_id))
               && input
                   .assessment_target_filter
                   .as_ref()
                   .is_none_or(|filter| record.matches_target(filter))
               && input
                   .framework_filters
                   .as_ref()
                   .is_none_or(|filters| record.matches_framework_filters(filters))
       })"

and each predicate pins a DIFFERENT framework — state.rs:1677-1689:
  "pub fn matches_framework_filters(&self, filters: &FrameworkFilters) -> bool {
       let (framework, delivery_option) = match filters {
           FrameworkFilters::AMISecurityFilters(filter) => {
               (FRAMEWORK_AMI_SECURITY, filter.delivery_option_id.as_deref())
           }
           FrameworkFilters::ContainerSecurityFilters(filter) => (
               FRAMEWORK_CONTAINER_SECURITY,
               filter.delivery_option_id.as_deref(),
           )
       };
       self.framework == framework
           && delivery_option.is_none_or(...)"

The sibling handler refuses the analogous mismatch — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/list_entities.rs:205-220:
  "let type_criteria = match input.entity_type_filters.as_ref() {
       None => Vec::new(),
       Some(filters) => {
           let (filtered, criteria) = type_criteria(filters);
           if filtered != input.entity_type {
               return Err(ListEntitiesErrorModeled::ValidationException(
                   crate::state::validation(
                       \"EntityTypeFilters\",
                       format!(
                           \"{filtered}Filters filters the {filtered} entity type, not {:?}\",
                           input.entity_type
                       ),"

The crate states the rule it breaks here — state.rs:152-156:
  "/// A filter selects on an attribute, so when the model enumerates that
   /// attribute's values, a filter value outside that enum names no value the
   /// attribute can hold — refused, as every other value of this service naming
   /// nothing the catalog has is refused, rather than silently selecting the empty
   /// page it would match."
and list_assessments.rs:186-190 repeats it for this very operation:
  "// a value naming no framework
   // at all names nothing an assessment can have been performed against and is
   // refused before anything is selected, as every other value of this service
   // outside a set the model publishes is."

Test coverage: tests/all/assessments.rs:822-834 exercises `FrameworkFilters` alone ("a framework's own filters", "a framework and a delivery option") and tests/all/framework_id_validation.rs exercises `FrameworkId` alone; NO test pins the combination of a `FrameworkId` with a `FrameworkFilters` member of another framework, so no stated reason covers the current answer.

## Steps to Reproduce

1. Start and let settle an Intent=APPLY change set that adds an AMI delivery option to an AmiProduct, so the catalog records one AMISecurity assessment (see tests/all/assessment_production.rs for the shape of such a change).
2. POST /ListAssessments with {"Catalog":"AWSMarketplace","FrameworkFilters":{"AMISecurityFilters":{}}} — the assessment is listed, confirming the record exists.
3. POST /ListAssessments with {"Catalog":"AWSMarketplace","FrameworkId":"ContainerSecurity","FrameworkFilters":{"AMISecurityFilters":{}}} — the service answers HTTP 200 with an empty AssessmentSummaryList instead of refusing the self-contradicting request, although `FrameworkFilters` is documented as the member "corresponding to the framework you want to filter by".
4. Compare with POST /ListEntities with {"Catalog":"AWSMarketplace","EntityType":"SaaSProduct","EntityTypeFilters":{"AmiProductFilters":{}}} — the sibling handler refuses that mismatch 422 ValidationException ("AmiProductFilters filters the AmiProduct entity type, not \"SaaSProduct\"").
