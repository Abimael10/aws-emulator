// SPDX-License-Identifier: CC0-1.0

//! EMPTY — this file carries no test, so nothing here runs.
//!
//! It held a throwaway probe used while tracing the async-invoke retention check: it drove
//! the recorded sequence (create the bucket, `StartAsyncInvoke`, poll `GetAsyncInvoke` to a
//! terminal status, then `ListObjectsV2`, `HeadObject`, `GetObject` and `ListAsyncInvokes`
//! over the delivered output) and reported each answer, which is how that check's remaining
//! divergence was placed outside this emulator; what it showed is now asserted properly in
//! `all/async_invoke_output_artifact.rs`. It then held the one-shot writer that rebuilt
//! `core/router/tests/all.rs` from the listing of `core/router/tests/all/`, after concurrent
//! landings had grown that root into hundreds of re-typed copies of its own header. Its
//! content is gone rather than the file because this fixer's tool set can write a file but
//! not remove one; the next change to touch this directory should delete it.
