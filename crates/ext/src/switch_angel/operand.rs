use rustel_core::Pattern;
use rustel_core::extension_node::ExtensionPatternNode;
use rustel_core::ops::PatOps;
use rustel_core::purity::PurePattern;

/// Pattern operand accepted by Switch Angel's native nodes.
///
/// This extension-owned bridge keeps authored graph nodes out of core's
/// generic `PatOps` surface while preserving pure-pattern construction.
pub(crate) trait NativeExtensionOperand: PatOps {
    fn pattern_handle(&self) -> Pattern;
    fn from_extension_node(node: impl ExtensionPatternNode + 'static) -> Self;
    fn arp_indices(&self, indices: Pattern) -> Self;
}

impl NativeExtensionOperand for Pattern {
    fn pattern_handle(&self) -> Pattern {
        self.clone()
    }

    fn from_extension_node(node: impl ExtensionPatternNode + 'static) -> Self {
        Pattern::extension_node(node)
    }

    fn arp_indices(&self, indices: Pattern) -> Self {
        rustel_core::arp(self.clone(), indices)
    }
}

impl NativeExtensionOperand for PurePattern {
    fn pattern_handle(&self) -> Pattern {
        self.pattern().clone()
    }

    fn from_extension_node(node: impl ExtensionPatternNode + 'static) -> Self {
        PurePattern::assert_pure(Pattern::extension_node(node))
    }

    fn arp_indices(&self, indices: Pattern) -> Self {
        PurePattern::assert_pure(rustel_core::arp(self.pattern().clone(), indices))
    }
}
