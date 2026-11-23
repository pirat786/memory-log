use serde::Deserialize;

use crate::model::{Category, CategoryOverride, Session};
use std::collections::HashMap;

pub fn apply_overrides(sessions: &mut [Session], overrides: &[CategoryOverride]) {
    let rules: HashMap<_, _> = overrides.iter().filter_map(|rule| {
        rule.category.map(|category| ((rule.scope.as_str(), rule.ts, rule.app.as_str(), rule.title.as_str()), category))
    }).collect();
    for session in sessions {
        let app = session.app.as_str();
        let title = session.title.as_str();
        if let Some(&category) = rules.get(&("visit", session.start, app, title))
            .or_else(|| rules.get(&("window", 0, app, title)))
            .or_else(|| rules.get(&("app", 0, app, "")))
        {
            session.category = category;
        }
    }
}

/// Правила по умолчанию лежат в `config/categories.toml` и вшиваются в программу.
/// Пользовательский файл с тем же форматом может их заменить.
pub const DEFAULT_RULES: &str = include_str!("../../../config/categories.toml");

#[derive(Debug, thiserror::Error)]
pub enum CategorizeError {
    #[error("не удалось разобрать правила категорий: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Deserialize)]
struct RuleFile {
    #[serde(default)]
    rule: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    category: Category,
    #[serde(default)]
    app: Vec<String>,
    #[serde(default)]
    title: Vec<String>,
}

struct Rule {
    category: Category,
    app: Vec<String>,
    title: Vec<String>,
}

pub struct Categorizer {
    rules: Vec<Rule>,
}

impl Categorizer {
    pub fn from_toml(src: &str) -> Result<Self, CategorizeError> {
        let file: RuleFile = toml::from_str(src)?;
        let rules = file
            .rule
            .into_iter()
            .map(|r| Rule {
                category: r.category,
                app: r.app.into_iter().map(|s| s.to_lowercase()).collect(),
                title: r.title.into_iter().map(|s| s.to_lowercase()).collect(),
            })
            .collect();
        Ok(Self { rules })
    }

    pub fn with_defaults() -> Self {
        Self::from_toml(DEFAULT_RULES).expect("встроенные правила категорий должны быть корректны")
    }

    pub fn categorize(&self, app: &str, title: &str) -> Category {
        let mut app_lower: Option<String> = None;
        let mut title_lower: Option<String> = None;
        for rule in &self.rules {
            let by_app = rule.app.iter().any(|p| contains_ci(app, p, &mut app_lower));
            if by_app {
                return rule.category;
            }
            let by_title = rule.title.iter().any(|p| contains_ci(title, p, &mut title_lower));
            if by_title {
                return rule.category;
            }
        }
        Category::Other
    }
}

fn contains_ci(haystack: &str, needle_lower: &str, lower_cache: &mut Option<String>) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() {
        if !needle_lower.is_ascii() {
            return false;
        }
        if !haystack.bytes().any(|b| b.is_ascii_uppercase()) {
            return haystack.contains(needle_lower);
        }
        if haystack.len() <= 256 {
            let mut buf = [0u8; 256];
            for (dst, &src) in buf.iter_mut().zip(haystack.as_bytes()) {
                *dst = src.to_ascii_lowercase();
            }
            // SAFETY: input is ASCII, and to_ascii_lowercase preserves ASCII validity.
            let lower = unsafe { std::str::from_utf8_unchecked(&buf[..haystack.len()]) };
            return lower.contains(needle_lower);
        }
    }
    lower_cache
        .get_or_insert_with(|| haystack.to_lowercase())
        .contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let _ = Categorizer::with_defaults();
    }

    #[test]
    fn matches_by_app_and_title() {
        let c = Categorizer::with_defaults();
        assert_eq!(c.categorize("Telegram", ""), Category::Communication);
        assert_eq!(c.categorize("chrome", "Funny cats - YouTube"), Category::Entertainment);
        assert_eq!(c.categorize("unknown", "что-то"), Category::Other);
    }

    #[test]
    fn category_overrides_prefer_visit_then_title_then_app() {
        use crate::model::OverrideScope;
        let make_session = |start, title: &str| Session {
            start, end: start + 3, app: "chrome".into(), title: title.into(), category: Category::Other,
        };
        let mut sessions = [make_session(10, "YouTube"), make_session(20, "YouTube"), make_session(30, "Docs")];
        let rules = [
            (OverrideScope::App, 0, "", Category::Work),
            (OverrideScope::Window, 0, "YouTube", Category::Study),
            (OverrideScope::Visit, 10, "YouTube", Category::Entertainment),
        ].map(|(scope, ts, title, category)| CategoryOverride {
            scope, ts, app: "chrome".into(), title: title.into(), category: Some(category),
        });
        apply_overrides(&mut sessions, &rules);
        assert_eq!(sessions.map(|s| s.category), [Category::Entertainment, Category::Study, Category::Work]);
    }
}
