// SPDX-License-Identifier: CC0-1.0

//! Two SPELLINGS of one stored object stay apart, and the endpoint a read arrives at is no
//! part of what it answers.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`] (the
//! harness lives in the `detector_lifecycle` suite), and the uploaded-list table, families
//! and read endpoints are the `uploaded_list_location_round_trip` suite's, reused here
//! rather than copied.
//!
//! The rule under test: an uploaded list's `Location` is a plain string with a `len 1..300`
//! and no pattern, no default and no trait licensing re-addressing
//! (`smithy_query(shape="Location")`), documented as the URI of the file that contains the
//! list, so a read reports the stored byte sequence and never a value derived from it.
//!
//! What that leaves to assert beyond the round-trip table: a read-back CANNOT be the output
//! of a parse-and-re-emit projection, because such a projection has no way to keep two
//! spellings of ONE object distinct. Two lists stored under the raw and the percent-ENCODED
//! spelling of the same key therefore each report their own URI — a service that normalised,
//! escaped or re-hosted the value would answer one string for both — and neither answer
//! carries any part of the endpoint authority the read was sent to, whichever of this
//! emulator's addresses that is.

use crate::uploaded_list_location_round_trip::{
    create_list, detector, read_endpoints, read_list, FAMILIES,
};

/// One object, written two ways: a key carrying a non-ASCII character raw, and the same key
/// with that character percent-encoded. Both address the same file and are different
/// strings, so each must read back as itself.
const SPELLINGS: [(&str, &str); 2] = [
    ("the raw spelling", "s3://gd-lists/listes/liste-dé.txt"),
    (
        "the percent-encoded spelling",
        "s3://gd-lists/listes/liste-d%C3%A9.txt",
    ),
];

#[tokio::test]
async fn two_spellings_of_one_object_each_read_back_as_themselves() {
    for family in &FAMILIES {
        for (index, (why, location)) in SPELLINGS.iter().enumerate() {
            let other = SPELLINGS[(index + 1) % SPELLINGS.len()].1;
            let (router, detector_id) = detector().await;
            let case = format!("{}: {why}", family.segment);
            let id = create_list(
                &router,
                &detector_id,
                family,
                "spelling-list",
                location,
                &case,
            )
            .await;
            let path = format!("/detector/{detector_id}/{}/{id}", family.segment);

            for endpoint in read_endpoints() {
                let case = format!("{case} read at {endpoint}");
                let read = read_list(&router, endpoint, &path, &case).await;
                let served = read["location"].as_str().unwrap_or_default();
                assert_eq!(
                    served, *location,
                    "case {case:?}: expected the stored URI {location:?}, got {served:?}"
                );
                assert_ne!(
                    served, other,
                    "case {case:?}: the two spellings of one object must not collapse into \
                     one, got {served:?}"
                );
                // The authority a read was sent to is no part of the answer, so no fragment
                // of it — host or port — may appear in the reported URI.
                for part in endpoint.split(':') {
                    assert!(
                        !served.contains(part),
                        "case {case:?}: the endpoint the read arrived at must take no part \
                         in the answer, but {part:?} appears in {served:?}"
                    );
                }
            }
        }
    }
}
