// §C.1 crate hardening. Allows below are the sanctioned pedantic false-positive
// set (§C.12), each justified:
#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery, clippy::excessive_nesting)]
// A CLI's error enum is self-documenting via its Display remediation text;
// per-method # Errors sections would restate every variant.
#![allow(clippy::missing_errors_doc)]
// Module-qualified names (registry::Registry) are idiomatic here.
#![allow(clippy::module_name_repetitions)]
