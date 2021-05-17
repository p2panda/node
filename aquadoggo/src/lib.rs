//! # aquadoggo
#![warn(
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    trivial_casts,
    trivial_numeric_casts,
    unsafe_code,
    unstable_features,
    unused_import_braces,
    unused_qualifications
)]

mod config;
mod db;
mod errors;
mod materializer;
mod rpc;
mod runtime;
mod task;

#[cfg(test)]
mod test_helpers;

pub use config::Configuration;
pub use runtime::Runtime;
