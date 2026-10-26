//! Public release documentation must cover the actual shipped defaults, not incidental prose.
use enshrouded_headroom::config::DEFAULT_INI;
use std::collections::BTreeMap;

/// Read section/key/default triples independently of the documentation generator.
fn defaults() -> BTreeMap<(String, String), String> {
    let mut section = "";
    let mut values = BTreeMap::new();
    for line in DEFAULT_INI.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name;
        } else if !line.starts_with([';', '#']) {
            if let Some((key, value)) = line.split_once('=') {
                assert!(values
                    .insert(
                        (section.to_owned(), key.trim().to_owned()),
                        value.trim().to_owned()
                    )
                    .is_none());
            }
        }
    }
    values
}
/// Validate only the delimited table, including defaults and nonempty range/meaning cells.
fn validate(readme: &str) -> Result<(), String> {
    let (_, table) = readme
        .split_once("<!-- BEGIN GENERATED SETTINGS -->")
        .ok_or("missing table start")?;
    let (table, _) = table
        .split_once("<!-- END GENERATED SETTINGS -->")
        .ok_or("missing table end")?;
    let mut rows = BTreeMap::new();
    for line in table.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 5 || cells.iter().any(|cell| cell.is_empty()) {
            return Err(format!("invalid row: {line}"));
        }
        let key = (
            cells[0].trim_matches('`').to_owned(),
            cells[1].trim_matches('`').to_owned(),
        );
        if rows
            .insert(key, cells[2].trim_matches('`').to_owned())
            .is_some()
        {
            return Err("duplicate row".into());
        }
    }
    if rows != defaults() {
        return Err(format!("settings table differs from DEFAULT_INI: {rows:?}"));
    }
    Ok(())
}
#[test]
fn readme_table_covers_every_shipped_key_default_range_and_meaning() {
    validate(include_str!("../README.md")).unwrap();
}
#[test]
fn removing_a_table_row_fails_even_if_the_key_is_mentioned_elsewhere() {
    let readme = include_str!("../README.md");
    validate(readme).unwrap();
    let row = readme
        .lines()
        .find(|line| line.starts_with("| `General` | `Enabled` |"))
        .unwrap();
    let removed = readme.replacen(row, "", 1) + "\nEnabled=1 exists in prose outside the table.\n";
    assert!(validate(&removed).is_err());
}
