pub(super) const HAP_FACTORY: &str = "__hap_factory";
pub(super) const STATE_FACTORY: &str = "__state_factory";
pub(super) const FRACTION_FACTORY: &str = "__fraction_factory";
pub(super) const FRACTION_COERCE: &str = "__fraction_coerce";
pub(super) const NATIVE_QUERY_MARKER: &str = "__rustel_native_query";
pub(super) const WCHOOSE_PAIR_AT: &str = "__wchoose_pair_at";
pub(super) const ARRAY_ITEMS: &str = "__rustel_array_items";
pub(super) const LEXICAL_REIFY: &str = "__rustel_lexical_reify";
pub(super) const CALLBACK_SOURCE: &str = "__rustel_callback_source";
pub(super) const LEXICAL_SET_STRING_PARSER: &str = "__rustel_lexical_set_string_parser";
pub(super) const RAW_ARP_SELECTOR_FACTORY: &str = "__rustel_raw_arp_selector_factory";
pub(super) const MINI_STRING_COERCE: &str = "__rustel_mini_string_coerce";
pub(super) const PICK_LOOKUP_SHAPE: &str = "__rustel_pick_lookup_shape";
pub(super) const SQUEEZE_LOOKUP_SHAPE: &str = "__rustel_squeeze_lookup_shape";
pub(super) const LANE_RESET: &str = "__rustel_lane_reset";
pub(super) const LANE_FINISH: &str = "__rustel_lane_finish";

/// Array nesting depth at which list constructors and pattern arguments are
/// refused. Each level is a native recursion step, so a self-containing array
/// stops here with a catchable RangeError before it exhausts the Rust stack.
pub(super) const MAX_LIST_DEPTH: usize = 256;

pub(super) const SUPPORTED_GLOBAL_NAMES: &str = include_str!("../assets/supported-globals.txt");
pub(super) const SUPPORTED_GLOBAL_COUNT: usize = 51;

pub fn supported_global_names() -> impl Iterator<Item = &'static str> {
    SUPPORTED_GLOBAL_NAMES
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}
