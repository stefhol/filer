//! Wire format version for every JSON output.
//!
//! Bumped only for a breaking change to the shape of `search`, `deps`,
//! `bundle`, `show` or `index` JSON. Additive fields do not bump it.

pub const SCHEMA_VERSION: u32 = 1;
