//! Shared figure-annotation markers.
//!
//! The report's SVG capture (BERreport `egui_svg`) wraps DETAIL annotations —
//! dimension strings, per-corner heights, area/exposure sub-labels — in a
//! classed group so the page can trim figures to their labels, revealing the
//! detail on hover / via a checkbox (print always shows everything).
//!
//! Painters mark a detail run by emitting a zero-length, alpha-0 line segment
//! in one of these sentinel colours before and after it: invisible to every
//! real renderer (the tablet draws nothing for an alpha-0 zero-length line),
//! but the SVG serializer recognises the colours and opens/closes the group.
//! The values live HERE because `ber_render` (which paints some detail text)
//! and the report (which serializes) must agree on them, and `domain` is the
//! one crate both see — the same rule as the default orbit angles.

/// RGBA sentinel: a detail annotation run STARTS after this shape.
pub const DETAIL_ON_RGBA: [u8; 4] = [210, 121, 6, 0];
/// RGBA sentinel: the current detail annotation run ENDS at this shape.
pub const DETAIL_OFF_RGBA: [u8; 4] = [211, 122, 7, 0];
