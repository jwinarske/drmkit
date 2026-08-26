// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT

//! Just enough JSON to write a profile.
//!
//! Hand-rolled rather than serde: the workspace's tooling has no dependencies
//! and this needs none. Output is pretty-printed with one field per line, so
//! a baseline diff names the capability that moved rather than showing one
//! long line changed.

use std::fmt::Write as _;

/// A JSON object or array under construction.
pub(crate) struct Json {
    body: String,
    is_object: bool,
    empty: bool,
}

impl Json {
    /// An empty object.
    pub(crate) fn object() -> Self {
        Self {
            body: String::from("{"),
            is_object: true,
            empty: true,
        }
    }

    /// An empty array.
    pub(crate) fn array() -> Self {
        Self {
            body: String::from("["),
            is_object: false,
            empty: true,
        }
    }

    fn separate(&mut self) {
        if !self.empty {
            self.body.push(',');
        }
        self.empty = false;
    }

    /// A string field. The value is escaped.
    pub(crate) fn string(&mut self, key: &str, value: &str) {
        self.separate();
        let _ = write!(self.body, "\"{}\":\"{}\"", escape(key), escape(value));
    }

    /// A number field.
    pub(crate) fn number(&mut self, key: &str, value: usize) {
        self.separate();
        let _ = write!(self.body, "\"{}\":{value}", escape(key));
    }

    /// A boolean field.
    pub(crate) fn bool(&mut self, key: &str, value: bool) {
        self.separate();
        let _ = write!(self.body, "\"{}\":{value}", escape(key));
    }

    /// A field whose value is already JSON.
    pub(crate) fn raw(&mut self, key: &str, value: &str) {
        self.separate();
        let _ = write!(self.body, "\"{}\":{value}", escape(key));
    }

    /// Append to an array.
    pub(crate) fn push(&mut self, value: &str) {
        debug_assert!(!self.is_object, "push on an object");
        self.separate();
        self.body.push_str(value);
    }

    /// Close it.
    pub(crate) fn finish(mut self) -> String {
        self.body.push(if self.is_object { '}' } else { ']' });
        self.body
    }
}

/// The six escapes JSON requires, and nothing else.
///
/// A device model string is the only field here that comes from outside this
/// program, and it comes from the device tree.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Re-indent compact JSON, one field per line.
///
/// So a baseline diff names the capability that moved instead of reporting
/// that one very long line changed. Written as a pass over the finished text
/// rather than threading depth through the builder, and it tracks whether it
/// is inside a string: a device model from the device tree may contain a
/// brace, and a formatter that did not notice would split it.
pub(crate) fn pretty(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len() * 2);
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                depth += 1;
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::pretty;

    #[test]
    fn a_brace_inside_a_string_is_not_a_nesting_level() {
        // A device-tree model can contain anything. A formatter that split on
        // every brace would corrupt the line and, worse, make the baseline
        // diff unreadable exactly when the hardware changed.
        let compact = r#"{"model":"Board {rev A}","n":1}"#;
        let out = pretty(compact);
        assert!(out.contains(r#""model": "Board {rev A}""#), "{out}");
        // Open brace, the comma between the two fields, and the close: three.
        assert_eq!(
            out.matches('\n').count(),
            3,
            "two fields, one object: {out}"
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let compact = r#"{"a":"say \"hi\"","b":2}"#;
        let out = pretty(compact);
        assert!(out.contains(r#""a": "say \"hi\"""#), "{out}");
        assert!(out.contains(r#""b": 2"#), "{out}");
    }
}
