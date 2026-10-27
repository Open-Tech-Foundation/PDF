//! Resource limits and strictness (docs/PLAN.md §2, "Untrusted input").

use std::fmt;
use std::sync::Arc;

/// Whether malformed input may be repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strictness {
    /// Any deviation from the specification that needs a repair is an error.
    Strict,
    /// Repairs are made and recorded as diagnostics.
    #[default]
    Lenient,
}

/// A CPU budget hook: work is charged in abstract units (roughly, bytes or
/// objects processed), and the hook stops processing by returning `false`.
pub trait Budget: Send + Sync {
    /// Charges `units` of work; returns whether processing may continue.
    fn charge(&self, units: u64) -> bool;
}

/// Limits applied while reading. Every one is enforced; exceeding one is an
/// error ([`crate::Error::Limit`]), never a panic or unbounded work.
#[derive(Clone)]
pub struct Limits {
    /// Nesting depth of arrays and dictionaries within one object, and of
    /// reference chains followed while resolving.
    pub max_depth: u32,
    /// Highest object number accepted from an xref section or a scan.
    pub max_objects: u32,
    /// Maximum decoded size of one stream, in bytes.
    pub max_decoded_size: u64,
    /// Maximum ratio of decoded to encoded size for one stream, applied once
    /// the output passes [`Limits::ratio_floor`] bytes.
    pub max_compression_ratio: u64,
    /// Decoded size below which the ratio is not checked: small streams
    /// legitimately compress far better than large ones.
    pub ratio_floor: u64,
    /// Maximum number of xref sections followed through `/Prev`.
    pub max_xref_sections: u32,
    /// Maximum size of one object's syntax, excluding stream data.
    pub max_object_size: u64,
    /// Maximum number of pages enumerated.
    pub max_pages: u32,
    /// Optional CPU budget hook.
    pub budget: Option<Arc<dyn Budget>>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 256,
            max_objects: 8_388_607,
            max_decoded_size: 512 << 20,
            max_compression_ratio: 1100,
            ratio_floor: 16 << 20,
            max_xref_sections: 1024,
            max_object_size: 64 << 20,
            max_pages: 1_000_000,
            budget: None,
        }
    }
}

impl fmt::Debug for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Limits")
            .field("max_depth", &self.max_depth)
            .field("max_objects", &self.max_objects)
            .field("max_decoded_size", &self.max_decoded_size)
            .field("max_compression_ratio", &self.max_compression_ratio)
            .field("ratio_floor", &self.ratio_floor)
            .field("max_xref_sections", &self.max_xref_sections)
            .field("max_object_size", &self.max_object_size)
            .field("max_pages", &self.max_pages)
            .field("budget", &self.budget.is_some())
            .finish()
    }
}

impl Limits {
    /// Charges the CPU budget, if one is set.
    pub fn charge(&self, units: u64) -> Result<(), crate::Error> {
        match &self.budget {
            Some(b) if !b.charge(units) => Err(crate::Error::Limit(LimitKind::Budget)),
            _ => Ok(()),
        }
    }
}

/// Which limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// [`Limits::max_depth`].
    Depth,
    /// [`Limits::max_objects`].
    Objects,
    /// [`Limits::max_decoded_size`].
    DecodedSize,
    /// [`Limits::max_compression_ratio`].
    CompressionRatio,
    /// [`Limits::max_xref_sections`].
    XrefSections,
    /// [`Limits::max_object_size`].
    ObjectSize,
    /// [`Limits::max_pages`].
    Pages,
    /// The [`Budget`] hook refused more work.
    Budget,
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Depth => "nesting depth",
            Self::Objects => "object count",
            Self::DecodedSize => "decoded stream size",
            Self::CompressionRatio => "compression ratio",
            Self::XrefSections => "xref section count",
            Self::ObjectSize => "object size",
            Self::Pages => "page count",
            Self::Budget => "CPU budget",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Allow(u64);

    impl Budget for Allow {
        fn charge(&self, units: u64) -> bool {
            units <= self.0
        }
    }

    #[test]
    fn budget_hook() {
        let mut limits = Limits::default();
        assert!(limits.charge(u64::MAX).is_ok());
        limits.budget = Some(Arc::new(Allow(10)));
        assert!(limits.charge(10).is_ok());
        assert_eq!(
            limits.charge(11),
            Err(crate::Error::Limit(LimitKind::Budget))
        );
        assert!(format!("{limits:?}").contains("budget: true"));
    }

    #[test]
    fn display() {
        assert_eq!(LimitKind::DecodedSize.to_string(), "decoded stream size");
        assert_eq!(Strictness::default(), Strictness::Lenient);
    }
}
