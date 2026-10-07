//! Rustel-specific score extensions.
//!
//! These names extend the shared Strudel language. Declaring their origin
//! lets `check` report when a score depends on Rustel-specific features.
//!
//! Each public extension function owns its implementation, registration,
//! and reference metadata in one source file. This module only assembles
//! the statically linked collection.

use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{Origin, Registry};

use crate::Extension;

/// Origin reported for Rustel-specific score extensions.
pub const ORIGIN: Origin = Origin::new("rustel");

/// Reference entries for Rustel-specific names.
///
/// `limit` stores its documentation in the control table. It is also listed
/// here so Studio labels it as a Rustel extension; the panel clears `origin`
/// when ingesting ordinary control entries.
const REFERENCE_ENTRIES: &[ReferenceEntry] = &[rustel_core::controls_generated::LIMIT_REFERENCE];

pub(crate) const EXTENSION: Extension = Extension {
    origin: ORIGIN,
    install_patterns: install,
    pattern_states: &[],
    pattern_callables: &[],
    value_callables: &[],
    reference_groups: &[REFERENCE_ENTRIES],
};

fn install(_registry: &mut Registry) {}
