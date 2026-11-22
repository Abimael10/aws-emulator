// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteDataAutomationLibrary` starts, and what the clock owes it.
//!
//! RULE. A delete of a DataAutomationLibrary is ACCEPTED rather than performed: the library
//! reports the non-terminal [`LIBRARY_DELETING`] status from that call onwards
//! (`smithy_query(shape="DataAutomationLibraryStatus")` -> `["ACTIVE", "DELETING"]`, and
//! `DeleteDataAutomationLibrary` answers the library's `status`), and the removal the call
//! owes falls due [`LIBRARY_DELETION_SPAN`] later — from which instant the library resolves
//! to nothing: a read of it is the modelled `ResourceNotFoundException`
//! ([`super::LIBRARY_ARN_NOT_FOUND_MESSAGE`]), a listing no longer enumerates it, and the
//! `libraryName` it held is free for another create. Nothing sleeps for the span: the due
//! instant is stored on the record ([`super::DataAutomationLibraryRecord::deletion_due`])
//! and compared against the clock by the next call that touches the store.
//!
//! The delete is `@idempotent` (`smithy_query(query="operation",
//! operation="DeleteDataAutomationLibrary")`), so repeating it leaves the library with the
//! due instant the FIRST delete set: a caller cannot postpone a settling deletion by asking
//! for it again.
//!
//! RULE. A library that is still IN USE is not released at all: while any DataAutomationProject
//! of this account names it in its stored configuration
//! ([`super::DataAutomationProjectRecord::library_references`]), the delete is refused with the
//! modelled `ValidationException` (HTTP 400) and [`LIBRARY_REFERENCED_MESSAGE`], and the
//! library is left exactly as it was — still `ACTIVE`, with no removal owed — so the referring
//! projects keep resolving the library they were configured with. The reference is dropped by
//! the project side (a project deleted, or updated to a configuration that no longer names the
//! library), and from that instant the identical delete is accepted.
//!
//! Single-homed here because every operation that resolves a `libraryArn` has to agree about
//! which libraries still exist: two readings of that are two answers about one library
//! waiting to drift.

use std::time::{Duration, SystemTime};

use super::ComAmazonawsBedrockdataautomationAmazonbedrockkeystonebuildtimeserviceState as ServiceState;
use super::DataAutomationLibraryRecord;
use crate::ops::types::ValidationException;

/// The status a library reports between the delete that was accepted for it and the removal
/// that delete owes.
///
/// Source: `smithy_query(shape="DataAutomationLibraryStatus")` -> `["ACTIVE", "DELETING"]` —
/// the other half of the pair [`super::LIBRARY_ACTIVE`] names, and the one the teardown path
/// moves a library onto.
pub const LIBRARY_DELETING: &str = "DELETING";

/// How long after `DeleteDataAutomationLibrary` the clock owes the library's removal.
///
/// AWS documents no duration for it — the delete answers a status rather than a removal,
/// which is why the modelled status set carries `DELETING` at all — so this span is CHOSEN,
/// not modelled, and it is deliberately non-zero: a caller reading the library straight
/// after the delete still observes `DELETING`, so the transient status the model declares is
/// never skipped.
pub const LIBRARY_DELETION_SPAN: Duration = Duration::from_secs(3);

/// What a delete of a library a project still references answers.
///
/// RECORDED behaviour, and entirely FIXED text: the sentence names neither the library nor any
/// referring project, interpolates no count and reads nothing from state — so it is spelled
/// here once and never formatted. The model states no cross-resource rule of its own, which is
/// why the words and the 400 come from the recording.
pub const LIBRARY_REFERENCED_MESSAGE: &str = "Unable to delete the library as it is being \
     referenced by some project. You must remove it from all referring projects before you can \
     delete the library.";

impl ServiceState {
    /// Accept a delete of the library `library_arn` names, as of `now`: the library moves to
    /// [`LIBRARY_DELETING`] and its removal falls due [`LIBRARY_DELETION_SPAN`] later.
    ///
    /// `None` when this store holds no library under that ARN — the caller answers that with
    /// the modelled `ResourceNotFoundException`. Repeating the delete keeps the due instant
    /// the first one set (the operation is `@idempotent`), and the record handed back is the
    /// one that is now STORED, so the status the delete answers is the status a later read
    /// reports.
    pub fn accept_library_deletion(
        &mut self,
        library_arn: &str,
        now: SystemTime,
    ) -> Option<&DataAutomationLibraryRecord> {
        let record = self.libraries.get_mut(library_arn)?;
        if record.deletion_due.is_none() {
            record.deletion_due = Some(now + LIBRARY_DELETION_SPAN);
        }
        Some(record)
    }

    /// Every project of this store whose STORED configuration still references the library
    /// `library_arn` names, by `projectArn`.
    ///
    /// Read off the configuration the create or the latest update persisted — the same
    /// references a listing's library filter narrows on — so a project stops referring exactly
    /// when it is deleted or updated to a configuration that no longer names the library.
    /// Served lazily, so the refusal below settles the question on the first referrer it finds.
    pub fn projects_referencing_library<'store>(
        &'store self,
        library_arn: &'store str,
    ) -> impl Iterator<Item = &'store str> + 'store {
        self.projects
            .values()
            .filter(move |project| {
                project
                    .library_references()
                    .any(|item| item.library_arn == library_arn)
            })
            .map(|project| project.project_arn.as_str())
    }

    /// The refusal a delete of `library_arn` answers while a project of this store still
    /// references it, or `None` when nothing does.
    ///
    /// Handed back as the modelled shape rather than as a response, so the operation lifts it
    /// into its OWN error channel. It carries no `fieldList`: the sentence names no input
    /// member, so an entry invented here would report one the service does not.
    #[must_use]
    pub fn library_reference_refusal(&self, library_arn: &str) -> Option<ValidationException> {
        self.projects_referencing_library(library_arn)
            .next()
            .map(|_| ValidationException {
                message: Some(LIBRARY_REFERENCED_MESSAGE.to_string()),
                field_list: None,
            })
    }

    /// Materialise every library removal the clock has already made due at `now`.
    ///
    /// The write is owed at that same instant whether or not anybody called, which is why
    /// even a REPORTING operation of this service runs it before it resolves a `libraryArn`:
    /// it moves nothing the clock has not already moved.
    pub fn settle_library_deletions(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .libraries
            .values()
            .filter(|record| record.deletion_due.is_some_and(|at| now >= at))
            .map(|record| record.library_arn.clone())
            .collect();
        for arn in due {
            self.remove_library(&arn);
        }
    }

    /// Forget one library entirely: the record, its `libraryName` reservation, the create
    /// tokens that resolve to it, and the ingestion jobs that were submitted against it
    /// (with their own submission tokens) — a job is a resource OF the library, so a
    /// released library releases them with it, and nothing in the store outlives what it
    /// points at.
    pub fn remove_library(&mut self, library_arn: &str) -> Option<DataAutomationLibraryRecord> {
        let record = self.libraries.remove(library_arn)?;
        self.library_names.remove(&record.library_name);
        self.library_create_idempotency
            .retain(|_, stored| stored != library_arn);
        let jobs: Vec<String> = self
            .ingestion_jobs
            .values()
            .filter(|job| job.library_arn == library_arn)
            .map(|job| job.job_arn.clone())
            .collect();
        self.ingestion_jobs
            .retain(|_, job| job.library_arn != library_arn);
        self.ingestion_job_idempotency
            .retain(|_, stored| !jobs.contains(stored));
        Some(record)
    }

    /// The library `library_arn` names as of `now`, once every removal the clock has made
    /// due is materialised — the ONE lookup every operation that resolves a `libraryArn`
    /// takes, so a library whose deletion has settled resolves to nothing on every path.
    pub fn live_library(
        &mut self,
        library_arn: &str,
        now: SystemTime,
    ) -> Option<&DataAutomationLibraryRecord> {
        self.settle_library_deletions(now);
        self.libraries.get(library_arn)
    }

    /// [`Self::live_library`] for a call that CHANGES the library it resolves, so a write
    /// and a read agree about which libraries exist.
    pub fn live_library_mut(
        &mut self,
        library_arn: &str,
        now: SystemTime,
    ) -> Option<&mut DataAutomationLibraryRecord> {
        self.settle_library_deletions(now);
        self.libraries.get_mut(library_arn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DataAutomationLibraryRecord, LIBRARY_ACTIVE};

    /// One stored library, standing up ACTIVE as a create leaves it.
    fn library(arn: &str, name: &str) -> DataAutomationLibraryRecord {
        DataAutomationLibraryRecord {
            library_arn: arn.to_string(),
            library_name: name.to_string(),
            library_description: None,
            creation_time: ::aws_smithy_types::DateTime::from_secs(0),
            entity_types: Vec::new(),
            deletion_due: None,
            kms_key_id: None,
            kms_encryption_context: None,
            tags: Vec::new(),
        }
    }

    /// A store holding one library.
    fn store_with(arn: &str, name: &str) -> ServiceState {
        let mut store = ServiceState::default();
        store.libraries.insert(arn.to_string(), library(arn, name));
        store.library_names.insert(name.to_string(), arn.to_string());
        store
    }

    /// One stored project whose configuration references the library `library_arn` names, as
    /// a create carrying a `dataAutomationLibraryConfiguration` leaves it.
    fn referring_project(
        project_arn: &str,
        library_arn: &str,
    ) -> crate::state::DataAutomationProjectRecord {
        crate::state::DataAutomationProjectRecord {
            project_arn: project_arn.to_string(),
            project_name: "a_project".to_string(),
            project_stage: crate::state::DEFAULT_PROJECT_STAGE.to_string(),
            project_type: "ASYNC".to_string(),
            project_description: None,
            standard_output_configuration: None,
            custom_output_configuration: None,
            override_configuration: None,
            data_automation_library_configuration: Some(
                crate::ops::types::DataAutomationLibraryConfiguration {
                    libraries: Some(vec![crate::ops::types::DataAutomationLibraryItem {
                        library_arn: library_arn.to_string(),
                    }]),
                },
            ),
            creation_time: ::aws_smithy_types::DateTime::from_secs(0),
            last_modified_time: ::aws_smithy_types::DateTime::from_secs(0),
            kms_key_id: None,
            kms_encryption_context: None,
            completes_at: SystemTime::UNIX_EPOCH,
            tags: Vec::new(),
        }
    }

    /// RULE: the delete is accepted, the library reports DELETING for the span, and from the
    /// instant the removal falls due it resolves to nothing and its name is free again.
    #[test]
    fn a_deleted_library_reports_deleting_then_resolves_to_nothing() {
        let arn = "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdef";
        let mut store = store_with(arn, "a_library");
        let accepted_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        let before = store
            .live_library(arn, accepted_at)
            .map(DataAutomationLibraryRecord::status);
        assert_eq!(
            before,
            Some(LIBRARY_ACTIVE),
            "case \"before the delete\": expected {LIBRARY_ACTIVE:?}, got {before:?}"
        );

        let accepted = store
            .accept_library_deletion(arn, accepted_at)
            .map(DataAutomationLibraryRecord::status);
        assert_eq!(
            accepted,
            Some(LIBRARY_DELETING),
            "case \"the delete\": expected {LIBRARY_DELETING:?}, got {accepted:?}"
        );

        // (case, how long after the delete, the status a read reports)
        let rows: [(&str, Duration, Option<&str>); 4] = [
            ("at the delete", Duration::ZERO, Some(LIBRARY_DELETING)),
            (
                "inside the span",
                LIBRARY_DELETION_SPAN / 2,
                Some(LIBRARY_DELETING),
            ),
            ("at the due instant", LIBRARY_DELETION_SPAN, None),
            (
                "past the due instant",
                LIBRARY_DELETION_SPAN * 4,
                None,
            ),
        ];
        for (case, elapsed, expected) in rows {
            let mut store = store.clone_for_case(arn, accepted_at);
            let actual = store
                .live_library(arn, accepted_at + elapsed)
                .map(DataAutomationLibraryRecord::status);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }

        store.settle_library_deletions(accepted_at + LIBRARY_DELETION_SPAN);
        assert!(
            !store.library_names.contains_key("a_library"),
            "case \"the name reservation\": expected the released library's name to be free, \
             got {:?}",
            store.library_names.keys().collect::<Vec<_>>()
        );
    }

    /// RULE: a library a project's stored configuration still references earns the in-use
    /// refusal, and one nothing references does not.
    #[test]
    fn a_library_a_project_references_earns_the_in_use_refusal() {
        let referenced =
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdef";
        let free = "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/0123456789ab";
        let mut store = store_with(referenced, "a_library");
        store.libraries.insert(free.to_string(), library(free, "a_free_library"));
        let project_arn =
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-project/abcdefabcdef";
        store.projects.insert(
            project_arn.to_string(),
            referring_project(project_arn, referenced),
        );

        // (case, the library asked about, the projects expected to refer to it)
        let rows: [(&str, &str, Vec<&str>); 2] = [
            ("the referenced library", referenced, vec![project_arn]),
            ("a library nothing references", free, Vec::new()),
        ];
        for (case, library_arn, expected) in rows {
            let mut referrers: Vec<&str> = store.projects_referencing_library(library_arn).collect();
            referrers.sort_unstable();
            assert_eq!(
                referrers, expected,
                "case {case:?}: expected the referrers {expected:?}, got {referrers:?}"
            );
            let refusal = store
                .library_reference_refusal(library_arn)
                .and_then(|refusal| refusal.message);
            let expected_message =
                (!expected.is_empty()).then(|| LIBRARY_REFERENCED_MESSAGE.to_string());
            assert_eq!(
                refusal, expected_message,
                "case {case:?}: expected {expected_message:?}, got {refusal:?}"
            );
        }
    }

    /// RULE: repeating the `@idempotent` delete keeps the due instant the first one set.
    #[test]
    fn repeating_the_delete_does_not_postpone_the_removal() {
        let arn = "arn:aws:bedrock:eu-west-1:000000000000:data-automation-library/0123456789ab";
        let mut store = store_with(arn, "another_library");
        let first = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let due = store
            .accept_library_deletion(arn, first)
            .and_then(|record| record.deletion_due);
        let again = store
            .accept_library_deletion(arn, first + Duration::from_secs(60))
            .and_then(|record| record.deletion_due);
        assert_eq!(
            again, due,
            "case \"a repeated delete\": expected the first due instant {due:?}, got {again:?}"
        );
    }

    /// RULE: a released library takes the ingestion jobs submitted against it with it, and
    /// leaves the jobs of every other library alone.
    #[test]
    fn a_released_library_releases_its_ingestion_jobs() {
        let arn = "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdef";
        let other = "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/0123456789ab";
        let mut store = store_with(arn, "a_library");
        store
            .libraries
            .insert(other.to_string(), library(other, "other_library"));
        store
            .library_names
            .insert("other_library".to_string(), other.to_string());
        for (job_arn, library_arn) in [("job-of-this", arn), ("job-of-other", other)] {
            store.ingestion_jobs.insert(
                job_arn.to_string(),
                crate::state::library_ingestion::DataAutomationLibraryIngestionJobRecord {
                    job_arn: job_arn.to_string(),
                    library_arn: library_arn.to_string(),
                    entity_type: "VOCABULARY".to_string(),
                    operation_type: "UPSERT".to_string(),
                    input_configuration: crate::ops::types::InputConfiguration {
                        s3_object: None,
                        inline_payload: None,
                    },
                    output_configuration: crate::ops::types::OutputConfiguration {
                        s3_uri: "s3://bucket/prefix".to_string(),
                    },
                    notification_configuration: None,
                    creation_time: ::aws_smithy_types::DateTime::from_secs(0),
                    completes_at: SystemTime::UNIX_EPOCH,
                    completion_time: ::aws_smithy_types::DateTime::from_secs(0),
                    tags: Vec::new(),
                },
            );
            store
                .ingestion_job_idempotency
                .insert(format!("token-{job_arn}"), job_arn.to_string());
        }

        store.remove_library(arn);
        let held: Vec<&String> = store.ingestion_jobs.keys().collect();
        assert_eq!(
            held,
            vec![&"job-of-other".to_string()],
            "case \"the released library's jobs\": expected only the other library's job to \
             remain, got {held:?}"
        );
        let tokens: Vec<&String> = store.ingestion_job_idempotency.values().collect();
        assert_eq!(
            tokens,
            vec![&"job-of-other".to_string()],
            "case \"the released library's submission tokens\": expected only the other \
             library's token to remain, got {tokens:?}"
        );
    }

    impl ServiceState {
        /// A fresh store holding the same library with the same accepted deletion, so each
        /// row of a table reads a store no earlier row has already settled.
        fn clone_for_case(&self, arn: &str, accepted_at: SystemTime) -> ServiceState {
            let record = self
                .libraries
                .get(arn)
                .cloned()
                .unwrap_or_else(|| library(arn, "a_library"));
            let mut store = store_with(arn, &record.library_name);
            store.accept_library_deletion(arn, accepted_at);
            store
        }
    }
}
