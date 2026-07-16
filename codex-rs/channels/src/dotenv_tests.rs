use std::collections::HashMap;

use pretty_assertions::assert_eq;

use super::parse_dotenv;

fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn parses_plain_assignments() {
    let parsed = parse_dotenv("DISCORD_BOT_TOKEN=abc123\nDISCORD_ALLOWED_USER_IDS=1,2\n");
    assert_eq!(
        parsed,
        map(&[
            ("DISCORD_BOT_TOKEN", "abc123"),
            ("DISCORD_ALLOWED_USER_IDS", "1,2"),
        ])
    );
}

#[test]
fn skips_comments_and_blank_lines() {
    let parsed = parse_dotenv("# a comment\n\n  \nKEY=value\n# another\n");
    assert_eq!(parsed, map(&[("KEY", "value")]));
}

#[test]
fn supports_export_prefix() {
    let parsed = parse_dotenv("export KEY=value\nexport  OTHER=two\n");
    assert_eq!(parsed, map(&[("KEY", "value"), ("OTHER", "two")]));
}

#[test]
fn strips_matching_quotes() {
    let parsed = parse_dotenv("A=\"double quoted\"\nB='single quoted'\nC=\"\"\n");
    assert_eq!(
        parsed,
        map(&[("A", "double quoted"), ("B", "single quoted"), ("C", "")])
    );
}

#[test]
fn double_quotes_support_escapes_and_single_quotes_are_literal() {
    let parsed = parse_dotenv("A=\"line1\\nline2\\t\\\"x\\\\\"\nB='no\\nescape'\n");
    assert_eq!(
        parsed,
        map(&[("A", "line1\nline2\t\"x\\"), ("B", "no\\nescape")])
    );
}

#[test]
fn unquoted_values_strip_trailing_comments_but_keep_hashes() {
    let parsed = parse_dotenv("A=value # trailing comment\nB=with#hash\n");
    assert_eq!(parsed, map(&[("A", "value"), ("B", "with#hash")]));
}

#[test]
fn quoted_values_keep_hash_content() {
    let parsed = parse_dotenv("A=\"value # not a comment\"\n");
    assert_eq!(parsed, map(&[("A", "value # not a comment")]));
}

#[test]
fn skips_malformed_lines_and_invalid_keys() {
    let parsed = parse_dotenv("JUSTAWORD\n1BAD=nope\nBAD KEY=nope\nOK=yes\n=empty\n");
    assert_eq!(parsed, map(&[("OK", "yes")]));
}

#[test]
fn later_assignments_win() {
    let parsed = parse_dotenv("KEY=first\nKEY=second\n");
    assert_eq!(parsed, map(&[("KEY", "second")]));
}
