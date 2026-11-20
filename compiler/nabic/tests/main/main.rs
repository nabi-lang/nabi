//! The `nabic` integration tests, one binary with a module per area:
//! `cargo test -p nabic --test main integration::` runs the corpus,
//! `cli::deps` the tests whose path contains that text, and so on.
//! Shared plumbing and the case directives: `common`.

mod cli;
mod common;
mod devtools;
mod differential;
mod fuzz;
mod integration;
mod native;
mod timing;
