use std::collections::HashMap;

/// Parses `.env`-style contents: `KEY=VALUE` lines, `#` comments, optional
/// single/double quotes, and an optional `export ` prefix. Later assignments
/// win. Malformed lines are skipped.
///
/// Double-quoted values support the `\n`, `\r`, `\t`, `\"` and `\\` escapes;
/// single-quoted values are taken literally. Unquoted values are trimmed and
/// support trailing ` # comment` stripping.
pub fn parse_dotenv(contents: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_valid_key(key) {
            continue;
        }
        if let Some(value) = parse_value(value.trim()) {
            vars.insert(key.to_string(), value);
        }
    }
    vars
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_value(raw: &str) -> Option<String> {
    if let Some(inner) = strip_surrounding(raw, '"') {
        return Some(unescape_double_quoted(inner));
    }
    if let Some(inner) = strip_surrounding(raw, '\'') {
        return Some(inner.to_string());
    }
    // Unquoted: strip a trailing comment introduced by whitespace + '#'.
    let value = match raw.find(" #") {
        Some(idx) => &raw[..idx],
        None => raw,
    };
    Some(value.trim().to_string())
}

fn strip_surrounding(raw: &str, quote: char) -> Option<&str> {
    if raw.len() >= 2 && raw.starts_with(quote) && raw.ends_with(quote) {
        Some(&raw[1..raw.len() - 1])
    } else {
        None
    }
}

fn unescape_double_quoted(inner: &str) -> String {
    let mut value = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => value.push('\n'),
            Some('r') => value.push('\r'),
            Some('t') => value.push('\t'),
            Some('"') => value.push('"'),
            Some('\\') => value.push('\\'),
            Some(other) => {
                value.push('\\');
                value.push(other);
            }
            None => value.push('\\'),
        }
    }
    value
}

#[cfg(test)]
#[path = "dotenv_tests.rs"]
mod tests;
