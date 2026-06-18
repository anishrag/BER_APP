//! The NUMBER-ENTRY model: the input engine behind the Sketch page's
//! measurement box, kept free of any UI toolkit so it can grow on its own.
//!
//! A measurement is built up as a running text *expression* (a `+`/`-` sum,
//! e.g. `"3.2 + 1.5"`) held in a plain `String`. [`apply`] mutates that buffer
//! one [`Key`] at a time and [`parse_dimension_expression`] turns it into the
//! final metre value. The point of the split: a [`Key`] doesn't care where it
//! came from — an on-screen keypad tap, a hardware keyboard, or (later) a
//! paired Bluetooth measuring device streaming a reading — so new input sources
//! plug in here without touching the renderer in `src/ui`.

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

    #[test]
    fn backspace_and_clear() {
        let mut buf = "4.2".to_string();
        apply(&mut buf, Key::Backspace);
        assert_eq!(buf, "4.");
        apply(&mut buf, Key::Clear);
        assert_eq!(buf, "");
    }
}
