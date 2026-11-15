// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the WIDTH of the auto-generated `passphrase` a channel's `srt` block
//! carries, and for that secret surviving the operations `generated_ingest_secrets.rs` does
//! not drive, over the wire through `MultiServiceRouter::dispatch` on the harness
//! `channel_lifecycle` owns.
//!
//! What the passphrase is written in, that it is inside the window SRT admits, that no two
//! channels share one, that `GetChannel` answers the one the create reported and that the
//! `endpoint` beside it is the channel's ingest endpoint are `generated_ingest_secrets.rs`'s
//! rules, whose create table this suite reuses rather than restating. The two rules here are
//! the ones that file does not pin:
//!
//! 1. The width is the one AWS issues for the member, not one this service picked:
//!    `SrtPassphrase` carries no `@length` and no `@pattern`
//!    (`smithy_query(query="shape", shape="SrtPassphrase")`), so the value AWS answers is the
//!    authority — a long secret of [`AWS_SRT_PASSPHRASE_WIDTH`] characters, the same width for
//!    every channel however the create was written.
//! 2. That secret is the CHANNEL's: the create mints it once, and an update that rewrites
//!    every other member of the channel — the ingest flag it goes with included — does not
//!    re-mint it, nor does a batch read report another value.

use serde_json::json;

use super::channel_lifecycle::{text, Ivs};
use super::generated_ingest_secrets::create_requests;

/// The width AWS's auto-generated SRT passphrase has, and therefore the width this service
/// must issue (rule 1). The model pins none, so it is read off the value AWS answers for the
/// member — the `srt.passphrase` of the recorded `CreateChannel` responses this emulator is
/// held against, which the recording asserts by shape rather than by literal because the
/// secret itself is per-run: a long secret, far wider than the 32-character token this
/// service used to cut and than the secret field of a stream key.
const AWS_SRT_PASSPHRASE_WIDTH: usize = 64;

/// Rule 1 over the create table `generated_ingest_secrets.rs` spans the create surface with:
/// every channel's passphrase is of the width AWS issues, whatever the request named.
#[tokio::test]
async fn every_channel_reports_an_srt_passphrase_of_the_width_aws_issues() {
    let ivs = Ivs::new();

    for (case, request) in create_requests() {
        let created = ivs.create(request).await;
        let passphrase = text(&created["channel"]["srt"], "passphrase");
        assert_eq!(
            passphrase.chars().count(),
            AWS_SRT_PASSPHRASE_WIDTH,
            "case {case:?}: the srt passphrase must be the width AWS issues, got \
             {passphrase:?}"
        );
    }
}

/// Rule 2: the secret belongs to the channel, so the update and the batch read answer the
/// passphrase the create minted, at that same width.
#[tokio::test]
async fn an_update_and_a_batch_read_report_the_passphrase_the_create_minted() {
    let ivs = Ivs::new();

    for (case, request) in create_requests() {
        let created = ivs.create(request).await;
        let arn = text(&created["channel"], "arn");
        let minted = text(&created["channel"]["srt"], "passphrase");

        // The update names every member an update can reach, the ingest flag the passphrase
        // goes with included, so nothing about the call leaves the srt block alone.
        let rewritten = json!({
            "arn": arn,
            "name": "rewritten",
            "latencyMode": "NORMAL",
            "type": "BASIC",
            "authorized": true,
            "insecureIngest": true,
            "containerFormat": "TS",
        });
        // Rows: the two operations that project a channel back which this suite owns, and
        // where the channel sits in what each answers.
        let projections = [
            ("UpdateChannel", rewritten, false),
            ("BatchGetChannel", json!({ "arns": [arn] }), true),
        ];
        for (operation, body, batched) in projections {
            let answered = ivs.ok(operation, body).await;
            let channel = if batched {
                &answered["channels"][0]
            } else {
                &answered["channel"]
            };
            let reported = text(&channel["srt"], "passphrase");
            assert_eq!(
                reported, minted,
                "case {case:?}: {operation} must report the passphrase the create minted, got {}",
                channel["srt"]
            );
            assert_eq!(
                reported.chars().count(),
                AWS_SRT_PASSPHRASE_WIDTH,
                "case {case:?}: {operation} must report it at the width AWS issues, got \
                 {reported:?}"
            );
        }
    }
}
