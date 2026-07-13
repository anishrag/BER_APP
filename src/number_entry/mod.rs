//! The NUMBER-ENTRY model: the input engine behind the Sketch page's
//! measurement box, kept free of any UI toolkit so it can grow on its own.
//!
//! A measurement is built up as a running text *expression* (a `+`/`-` sum,
//! e.g. `"3.2 + 1.5"`) held in a plain `String`. [`apply`] mutates that buffer
//! one [`Key`] at a time and [`parse_dimension_expression`] turns it into the
//! final metre value. The point of the split: a [`Key`] doesn't care where it
//! came from — an on-screen keypad tap, a hardware keyboard, or a paired
//! Bluetooth measuring device streaming a reading — so new input sources plug
//! in here without touching the renderer in `src/ui`.
//!
//! [`bluetooth`] is one such source: a Leica DISTO laser distance meter whose
//! readings arrive over BLE and land in the same expression buffer.

pub mod bluetooth;

/// One edit applied to a number-entry buffer, independent of its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Append a digit `'0'`–`'9'`.
    Digit(char),
    /// Append the decimal point.
    Dot,
    /// Delete the last character.
    Backspace,
    /// Add another term to the running sum (appends `" + "`).
    Plus,
    /// Subtract the next term from the running total (appends `" - "`).
    Minus,
    /// Clear the whole expression.
    Clear,
}

/// Apply one key to the running expression `buf`.
pub fn apply(buf: &mut String, key: Key) {
    match key {
        Key::Digit(d) => buf.push(d),
        Key::Dot => buf.push('.'),
        Key::Backspace => {
            buf.pop();
        }
        Key::Plus => append_op(buf, '+'),
        Key::Minus => append_op(buf, '-'),
        Key::Clear => buf.clear(),
    }
}

/// Apply one key to a buffer that may still hold an untouched SUGGESTION — a
/// value the box was PRE-FILLED with (a drawn length, the typical slab
/// thickness, the storey's ceiling height, …). While `pristine` is set the
/// suggestion is treated like pre-selected text: the first CONTENT key
/// (digit / dot / backspace) drops the whole suggestion first, so a typed
/// number REPLACES it instead of appending; a `+`/`−` keeps it as the running
/// sum's first term. The first key of any kind CONSUMES the flag, so every
/// subsequent key edits normally. A non-pristine buffer behaves exactly like
/// [`apply`].
pub fn apply_over_suggestion(buf: &mut String, key: Key, pristine: &mut bool) {
    if *pristine {
        *pristine = false;
        match key {
            // Typing (or backspacing) over the suggestion replaces it wholesale.
            Key::Digit(_) | Key::Dot | Key::Backspace => buf.clear(),
            // An operator keeps the suggestion as the first term; Clear empties
            // the buffer anyway.
            Key::Plus | Key::Minus | Key::Clear => {}
        }
    }
    apply(buf, key);
}

/// Append a ` + ` / ` - ` term separator — but only after a complete number, so
/// the expression never grows a dangling or doubled operator.
fn append_op(buf: &mut String, op: char) {
    let last = buf.trim_end().chars().last();
    if matches!(last, Some(c) if c.is_ascii_digit() || c == '.') {
        buf.push(' ');
        buf.push(op);
        buf.push(' ');
    }
}

/// Set the CURRENT (last) term of the expression to `metres`, keeping any
/// earlier `+`/`-` terms. Used when a laser reading lands: re-shooting without
/// an operator overwrites the unconfirmed value, but a reading after a pressed
/// `+`/`−` starts a fresh term in the running sum. Formatted to mm (3 dp).
pub fn set_current_term(buf: &mut String, metres: f32) {
    let formatted = format!("{metres:.3}");
    match buf.rfind(|c| c == '+' || c == '-') {
        // Keep everything through the last operator, then the new term.
        Some(op) => {
            buf.truncate(op + 1);
            buf.push(' ');
            buf.push_str(&formatted);
        }
        None => *buf = formatted,
    }
}

/// Insert a COPIED wall length (the number on another wall's canvas label,
/// hence the label's 2 dp formatting) into the expression as the TERM at the
/// caret. `cursor` is a char index into `buf` (`None` / out-of-range = end —
/// the keypad and laser flows are caret-less). Returns the caret position
/// (char index) just after the inserted text.
///
/// Semantics — "a tap SETS the term at the caret", the tap-input twin of the
/// laser's [`set_current_term`]:
/// - Caret touching a number (inside / at either edge, or across the spaces
///   of an operator gap): that whole term is REPLACED — so tapping a second
///   wall corrects the term, like re-shooting the laser.
/// - Caret in a fresh spot (empty buffer, or right after a `+`/`-`): the
///   length is inserted as a new term — `"3.2 + "` + tap → `"3.2 + 1.45"`.
///
/// Either way a previously parseable expression stays parseable.
pub fn insert_copied_length(buf: &mut String, cursor: Option<usize>, metres: f32) -> usize {
    let text = format!("{metres:.2}");
    let chars: Vec<char> = buf.chars().collect();
    let len = chars.len();
    let c = cursor.unwrap_or(len).min(len);
    let is_num = |ch: char| ch.is_ascii_digit() || ch == '.';

    // An index INSIDE the term the caret touches: directly adjacent first,
    // else across the spaces of an operator gap (left neighbour wins).
    let anchor: Option<usize> = if c > 0 && is_num(chars[c - 1]) {
        Some(c - 1)
    } else if c < len && is_num(chars[c]) {
        Some(c)
    } else {
        let mut l = c;
        while l > 0 && chars[l - 1] == ' ' {
            l -= 1;
        }
        if l > 0 && is_num(chars[l - 1]) {
            Some(l - 1)
        } else {
            let mut r = c;
            while r < len && chars[r] == ' ' {
                r += 1;
            }
            (r < len && is_num(chars[r])).then_some(r)
        }
    };

    let (start, end_excl) = match anchor {
        Some(a) => {
            let mut s = a;
            while s > 0 && is_num(chars[s - 1]) {
                s -= 1;
            }
            let mut e = a + 1;
            while e < len && is_num(chars[e]) {
                e += 1;
            }
            (s, e)
        }
        None => (c, c),
    };
    let head: String = chars[..start].iter().collect();
    let tail: String = chars[end_excl..].iter().collect();
    *buf = format!("{head}{text}{tail}");
    start + text.chars().count()
}

/// Parse a dimension expression. Supports:
/// - A single non-negative number: `"4.2"`, `"0.5"`, `"0"`
/// - Sums and differences of numbers: `"3.2 + 1.5"`, `"5 - 1.8 + 0.3"`
/// - Optional leading `+`; a leading `-` is rejected since dimensions can't
///   be negative.
///
/// Zero is accepted — it collapses the wall (used to zero a trim remnant so a
/// cut snaps onto a corner). Whitespace is ignored. Returns `None` for
/// malformed input or a negative result.
pub fn parse_dimension_expression(input: &str) -> Option<f32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut total: f32 = 0.0;
    let mut sign: f32 = 1.0;
    let mut current = String::new();
    let mut started = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '+' || ch == '-' {
            if !started {
                if ch == '-' {
                    return None;
                }
                started = true;
                continue;
            }
            if current.is_empty() {
                return None;
            }
            let v: f32 = current.parse().ok()?;
            total += sign * v;
            sign = if ch == '+' { 1.0 } else { -1.0 };
            current.clear();
        } else {
            current.push(ch);
            started = true;
        }
    }
    if current.is_empty() {
        return None;
    }
    let v: f32 = current.parse().ok()?;
    total += sign * v;
    // Zero is allowed: entering 0 collapses the wall (e.g. zeroing a trim
    // remnant to snap a cut onto a corner). Negatives are still rejected.
    if total.is_finite() && total >= 0.0 {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod expression_tests {
    use super::parse_dimension_expression;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn single_number() {
        assert!(approx(parse_dimension_expression("4.2").unwrap(), 4.2));
        assert!(approx(parse_dimension_expression("  3  ").unwrap(), 3.0));
    }

    #[test]
    fn sum_and_difference() {
        assert!(approx(parse_dimension_expression("3 + 1.5").unwrap(), 4.5));
        assert!(approx(parse_dimension_expression("5 - 1.8").unwrap(), 3.2));
        assert!(approx(
            parse_dimension_expression("2 + 3.5 - 0.3").unwrap(),
            5.2
        ));
    }

    #[test]
    fn leading_plus_allowed_leading_minus_rejected() {
        assert!(approx(parse_dimension_expression("+4.2").unwrap(), 4.2));
        assert!(parse_dimension_expression("-4.2").is_none());
    }

    #[test]
    fn malformed_inputs_rejected() {
        assert!(parse_dimension_expression("").is_none());
        assert!(parse_dimension_expression("3 +").is_none());
        assert!(parse_dimension_expression("3 + + 2").is_none());
        assert!(parse_dimension_expression("hello").is_none());
        assert!(parse_dimension_expression("3 - 5").is_none()); // negative result
    }

    #[test]
    fn zero_accepted_for_collapsing_a_wall() {
        // 0 is a valid dimension: it collapses the wall (e.g. zero a trim
        // remnant). Only negatives are rejected.
        assert!(approx(parse_dimension_expression("0").unwrap(), 0.0));
        assert!(approx(parse_dimension_expression("3 - 3").unwrap(), 0.0));
    }
}

#[cfg(test)]
mod copied_length_tests {
    use super::{insert_copied_length, parse_dimension_expression};

    /// Empty buffer: the tapped length becomes the first term (2 dp, matching
    /// the wall's canvas label).
    #[test]
    fn tap_into_an_empty_buffer_starts_the_expression() {
        let mut buf = String::new();
        let caret = insert_copied_length(&mut buf, None, 3.2);
        assert_eq!(buf, "3.20");
        assert_eq!(caret, 4);
        assert!(parse_dimension_expression(&buf).is_some());
    }

    /// Label rounding is applied HERE (one place): 2 dp, like the canvas label.
    #[test]
    fn inserted_text_uses_the_label_precision() {
        let mut buf = String::new();
        insert_copied_length(&mut buf, None, 1.456);
        assert_eq!(buf, "1.46");
    }

    /// The headline chaining flow: `+` then tap appends the next term.
    #[test]
    fn tap_after_an_operator_appends_a_new_term() {
        let mut buf = "3.2 + ".to_string();
        let caret = insert_copied_length(&mut buf, None, 1.45);
        assert_eq!(buf, "3.2 + 1.45");
        assert_eq!(caret, buf.chars().count());
        assert!((parse_dimension_expression(&buf).unwrap() - 4.65).abs() < 1e-4);
        // Hardware-typed bare "+" works the same (the coordinator example).
        let mut buf = "+".to_string();
        insert_copied_length(&mut buf, None, 3.2);
        assert_eq!(buf, "+3.20");
        assert!(parse_dimension_expression(&buf).is_some());
    }

    /// A tap with the caret on a complete term REPLACES it — the laser's
    /// re-shoot semantics, and it keeps the buffer parseable (no `3.201.45`).
    #[test]
    fn tap_on_a_bare_term_replaces_it() {
        let mut buf = "3.20".to_string();
        let caret = insert_copied_length(&mut buf, None, 1.45);
        assert_eq!(buf, "1.45");
        assert_eq!(caret, 4);
    }

    /// Cursor-aware: a caret INSIDE a mid-expression term replaces just that
    /// term, leaving the neighbours alone; the caret lands after the new text.
    #[test]
    fn tap_with_caret_inside_a_middle_term_replaces_only_it() {
        let mut buf = "3.2 + 1.5 - 0.6".to_string();
        // Caret between '1' and '.5' (char index 7).
        let caret = insert_copied_length(&mut buf, Some(7), 2.0);
        assert_eq!(buf, "3.2 + 2.00 - 0.6");
        assert_eq!(caret, 10);
        assert!((parse_dimension_expression(&buf).unwrap() - 4.6).abs() < 1e-4);
    }

    /// A caret parked in an operator gap adopts the number across the spaces
    /// (left first, else right) rather than gluing digits together.
    #[test]
    fn tap_in_an_operator_gap_replaces_the_adjacent_term() {
        // "3 +| 2": right neighbour.
        let mut buf = "3 + 2".to_string();
        insert_copied_length(&mut buf, Some(3), 1.45);
        assert_eq!(buf, "3 + 1.45");
        // "3.2 |+ 1": left neighbour.
        let mut buf = "3.2 + 1".to_string();
        insert_copied_length(&mut buf, Some(4), 9.0);
        assert_eq!(buf, "9.00 + 1");
        assert!(parse_dimension_expression(&buf).is_some());
    }

    /// An out-of-range caret (stale state) clamps to the end instead of
    /// panicking.
    #[test]
    fn out_of_range_cursor_clamps_to_end() {
        let mut buf = "2 + ".to_string();
        insert_copied_length(&mut buf, Some(99), 1.0);
        assert_eq!(buf, "2 + 1.00");
    }
}

#[cfg(test)]
mod key_tests {
    use super::{apply, parse_dimension_expression, Key};

    /// Driving the buffer through keys reproduces a typed expression.
    #[test]
    fn keys_build_an_expression() {
        let mut buf = String::new();
        for k in [
            Key::Digit('3'),
            Key::Dot,
            Key::Digit('2'),
            Key::Plus,
            Key::Digit('1'),
            Key::Minus,
            Key::Digit('4'),
        ] {
            apply(&mut buf, k);
        }
        assert_eq!(buf, "3.2 + 1 - 4");
        assert!((parse_dimension_expression(&buf).unwrap() - 0.2).abs() < 1e-4);
    }

    /// An operator is a no-op until a complete number precedes it, so the
    /// expression never grows a dangling or doubled operator.
    #[test]
    fn operator_needs_a_number_first() {
        let mut buf = String::new();
        apply(&mut buf, Key::Plus);
        apply(&mut buf, Key::Minus);
        assert_eq!(buf, "", "no leading operator");
        apply(&mut buf, Key::Digit('5'));
        apply(&mut buf, Key::Plus);
        apply(&mut buf, Key::Minus);
        assert_eq!(buf, "5 + ", "second operator ignored");
    }

    /// The first digit typed over a pre-filled SUGGESTION replaces it
    /// wholesale; later digits append normally.
    #[test]
    fn first_digit_replaces_a_pristine_suggestion() {
        use super::apply_over_suggestion;
        let mut buf = "0.25".to_string();
        let mut pristine = true;
        apply_over_suggestion(&mut buf, Key::Digit('5'), &mut pristine);
        assert_eq!(buf, "5", "first digit replaces the suggestion");
        assert!(!pristine, "the flag is consumed on the first edit");
        apply_over_suggestion(&mut buf, Key::Digit('0'), &mut pristine);
        assert_eq!(buf, "50", "subsequent digits append");
    }

    /// A `+`/`−` over a pristine suggestion keeps it as the running sum's first
    /// term rather than dropping it.
    #[test]
    fn operator_keeps_a_pristine_suggestion_as_first_term() {
        use super::apply_over_suggestion;
        let mut buf = "0.25".to_string();
        let mut pristine = true;
        apply_over_suggestion(&mut buf, Key::Plus, &mut pristine);
        assert_eq!(buf, "0.25 + ");
        assert!(!pristine);
        apply_over_suggestion(&mut buf, Key::Digit('1'), &mut pristine);
        assert_eq!(buf, "0.25 + 1", "the next digit appends to the new term");
    }

    /// Backspace is the "first edit" too — it consumes the flag and drops the
    /// whole suggestion (pre-selected-text semantics).
    #[test]
    fn backspace_clears_a_pristine_suggestion() {
        use super::apply_over_suggestion;
        let mut buf = "3.50".to_string();
        let mut pristine = true;
        apply_over_suggestion(&mut buf, Key::Backspace, &mut pristine);
        assert_eq!(buf, "");
        assert!(!pristine);
    }

    /// With no pristine suggestion, keys behave exactly like [`apply`] — the
    /// no-suggestion box is unaffected.
    #[test]
    fn non_pristine_appends_like_apply() {
        use super::apply_over_suggestion;
        let mut buf = "4".to_string();
        let mut pristine = false;
        apply_over_suggestion(&mut buf, Key::Digit('2'), &mut pristine);
        assert_eq!(buf, "42");
        assert!(!pristine);
    }

    #[test]
    fn backspace_and_clear() {
        let mut buf = "4.2".to_string();
        apply(&mut buf, Key::Backspace);
        assert_eq!(buf, "4.");
        apply(&mut buf, Key::Clear);
        assert_eq!(buf, "");
    }

    /// Laser readings overwrite the last term, but `+`/`−` between shots keeps
    /// the earlier ones — so multiple shots sum.
    #[test]
    fn laser_readings_replace_the_current_term() {
        use super::set_current_term;
        let mut buf = String::new();
        set_current_term(&mut buf, 3.247);
        assert_eq!(buf, "3.247");
        set_current_term(&mut buf, 3.250); // re-shoot overwrites
        assert_eq!(buf, "3.250");
        apply(&mut buf, Key::Plus);
        set_current_term(&mut buf, 1.5); // new term after +
        assert_eq!(buf, "3.250 + 1.500");
        set_current_term(&mut buf, 1.6); // overwrite just that term
        assert_eq!(buf, "3.250 + 1.600");
        assert!((parse_dimension_expression(&buf).unwrap() - 4.85).abs() < 1e-4);
    }
}
