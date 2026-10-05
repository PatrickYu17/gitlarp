//! Locale support: all user-facing strings live in `locales/en.toml`
//! (and `locales/es.toml`), embedded at compile time. `t!("key")`
//! fetches a string and `t!("key", name = value)` interpolates
//! `{name}` placeholders. The language is selected once at startup
//! from `GITLARP_LANG` (default `en`; unknown values fall back to
//! `en`); a key missing from the selected table falls back to the
//! English table, then to the key itself. No runtime panics.

use std::collections::HashMap;
use std::sync::OnceLock;

const EN: &str = include_str!("../locales/en.toml");
const ES: &str = include_str!("../locales/es.toml");

/// Resolve the locale source from a raw `GITLARP_LANG` value.
/// Pure so tests cover every branch; unknown/empty -> en.
fn lang_src(v: Option<&str>) -> &'static str {
    match v {
        Some(v) if v.trim().eq_ignore_ascii_case("es") => ES,
        _ => EN,
    }
}

fn active_src() -> &'static str {
    lang_src(std::env::var("GITLARP_LANG").ok().as_deref())
}

/// One parsed locale file: `key -> template`.
type Table = HashMap<&'static str, &'static str>;

/// Parse a locale file into `key -> value` (multiline `key = [`
/// arrays join their lines with newlines). Panics only on a
/// malformed EMBEDDED file (developer error, caught by the
/// locale tests in CI), never by user input.
fn parse(src: &'static str) -> Table {
    let mut map: HashMap<&'static str, &'static str> = HashMap::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("bad locale line: {line}"));
        let key = key.trim();
        let value = value.trim();
        if value == "[" {
            let mut parts = Vec::new();
            for l in lines.by_ref() {
                let l = l.trim().trim_end_matches(',');
                if l == "]" {
                    break;
                }
                parts.push(unquote(l));
            }
            map.insert(key, String::leak(parts.join("\n")));
        } else {
            map.insert(key, String::leak(unquote(value)));
        }
    }
    map
}

fn tables() -> &'static (Table, Table) {
    static TABLES: OnceLock<(Table, Table)> = OnceLock::new();
    TABLES.get_or_init(|| (parse(EN), parse(ES)))
}

fn table() -> &'static Table {
    let (en, es) = tables();
    if std::ptr::eq(active_src(), ES) {
        es
    } else {
        en
    }
}

fn en_table() -> &'static Table {
    &tables().0
}

fn unquote(s: &str) -> String {
    let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    s.replace("\\\"", "\"")
}

/// Key lookup: selected table -> English table -> the key itself.
/// The last resort keeps the CLI usable (no panic) if a locale ever
/// ships with a key missing.
pub fn t(key: &str) -> &'static str {
    table()
        .get(key)
        .or_else(|| en_table().get(key))
        .copied()
        .unwrap_or_else(|| String::leak(key.to_string()))
}

pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let template = t(key);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        // unclosed '{' -> treat the rest as literal text
        let Some(close) = rest[start..].find('}') else {
            out.push_str(rest);
            rest = "";
            break;
        };
        let end = start + close;
        out.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        match args.iter().find(|(k, _)| *k == name) {
            // missing arg -> keep the raw placeholder visible
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[start..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::locale::t($key).to_string()
    };
    ($key:literal $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::locale::tf($key, &[$( (stringify!($name), &format!("{}", $value)) ),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_selection() {
        assert_eq!(lang_src(None), EN);
        assert_eq!(lang_src(Some("")), EN);
        assert_eq!(lang_src(Some("en")), EN);
        assert_eq!(lang_src(Some("fr")), EN, "unknown language falls back to en");
        assert_eq!(lang_src(Some("es")), ES);
        assert_eq!(lang_src(Some("ES")), ES);
        assert_eq!(lang_src(Some(" es ")), ES);
    }

    /// Both embedded files parse cleanly and every key of es.toml
    /// exists in en.toml with identical {placeholder} names, so a
    /// locale typo fails in CI, not at a user's terminal.
    #[test]
    fn es_matches_en_keys_and_placeholders() {
        let en = parse(EN);
        let es = parse(ES);
        assert!(!es.is_empty());
        for (key, value) in &es {
            let template = en.get(*key).unwrap_or_else(|| panic!("es key missing in en: {key}"));
            assert_eq!(placeholders(template), placeholders(value), "placeholder mismatch in {key}");
        }
    }

    #[test]
    fn en_table_complete() {
        let en = parse(EN);
        for key in [
            "usage", "err.prefix", "err.gh_token", "err.nothing_to_wipe", "warn.clamped",
            "warn.unsigned", "info.created", "info.sched_line", "info.installed_macos",
            "cfg.badline", "state.bad",
        ] {
            assert!(en.contains_key(key), "missing en key: {key}");
        }
    }

    fn placeholders(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            match rest[start..].find('}') {
                Some(close) => {
                    out.push(rest[start + 1..start + close].to_string());
                    rest = &rest[start + close + 1..];
                }
                None => break,
            }
        }
        out.sort();
        out
    }

    #[test]
    fn missing_arg_keeps_placeholder_visible() {
        assert_eq!(tf("err.prefix", &[]), "error: {e}");
        assert_eq!(tf("err.prefix", &[("e", "boom")]), "error: boom");
    }

    #[test]
    fn unknown_key_falls_back_to_itself() {
        assert_eq!(t!("no.such.key"), "no.such.key");
    }

    #[test]
    fn interpolation_resolves() {
        assert_eq!(tf("err.unknown_flag", &[("flag", "--nope")]), "unknown flag --nope");
    }
}
