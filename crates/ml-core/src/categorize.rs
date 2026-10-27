use serde::Deserialize;

use crate::model::Category;

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
        let app = app.to_lowercase();
        let title = title.to_lowercase();
        for rule in &self.rules {
            let by_app = rule.app.iter().any(|p| app.contains(p));
            let by_title = rule.title.iter().any(|p| title.contains(p));
            if by_app || by_title {
                return rule.category;
            }
        }
        Category::Other
    }
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
}
