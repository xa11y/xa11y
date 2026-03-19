//! Fuzz target for xa11y-core selector parser (NOT platform providers).
//! Parses random and structured strings as CSS-like selectors.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use xa11y_core::Selector;

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Raw bytes interpreted as a selector string.
    raw: Vec<u8>,
    /// A structured selector-like string built from components.
    role_part: Option<String>,
    attr_name: Option<String>,
    attr_op: u8,
    attr_value: Option<String>,
    nth: Option<u16>,
    combinator: u8,
    second_role: Option<String>,
}

fuzz_target!(|input: FuzzInput| {
    // Strategy 1: Parse raw bytes as UTF-8 selector.
    if let Ok(s) = std::str::from_utf8(&input.raw) {
        let _ = Selector::parse(s);
    }

    // Strategy 2: Build a structured selector string and parse it.
    let mut selector = String::new();

    if let Some(ref role) = input.role_part {
        selector.push_str(role);
    }

    if let Some(ref attr_name) = input.attr_name {
        let op = match input.attr_op % 4 {
            0 => "=",
            1 => "*=",
            2 => "^=",
            _ => "$=",
        };
        selector.push('[');
        selector.push_str(attr_name);
        selector.push_str(op);
        selector.push('"');
        if let Some(ref val) = input.attr_value {
            selector.push_str(val);
        }
        selector.push('"');
        selector.push(']');
    }

    if let Some(nth) = input.nth {
        if nth > 0 {
            selector.push_str(&format!(":nth({})", nth));
        }
    }

    if input.second_role.is_some() || input.combinator % 3 != 0 {
        let comb = match input.combinator % 3 {
            1 => " > ",
            2 => " ",
            _ => " ",
        };
        selector.push_str(comb);
        if let Some(ref role2) = input.second_role {
            selector.push_str(role2);
        }
    }

    if !selector.is_empty() {
        let _ = Selector::parse(&selector);
    }
});
