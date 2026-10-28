use crate::model::Sample;

pub const HIDDEN_APP: &str = "(скрыто)";

/// Прячет чувствительные окна ДО записи в базу: то, чего нет в базе, нельзя украсть.
pub struct PrivacyFilter {
    hidden_apps: Vec<String>,
    hidden_titles: Vec<String>,
}

impl Default for PrivacyFilter {
    fn default() -> Self {
        let lower = |items: &[&str]| items.iter().map(|s| s.to_lowercase()).collect();
        Self {
            hidden_apps: lower(&["keepass", "bitwarden", "1password", "lastpass"]),
            hidden_titles: lower(&[
                "password", "пароль", "банк", "bank", "incognito", "инкогнито", "private browsing",
            ]),
        }
    }
}

impl PrivacyFilter {
    pub fn new(hidden_apps: Vec<String>, hidden_titles: Vec<String>) -> Self {
        Self {
            hidden_apps: hidden_apps.into_iter().map(|s| s.to_lowercase()).collect(),
            hidden_titles: hidden_titles.into_iter().map(|s| s.to_lowercase()).collect(),
        }
    }

    pub fn apply(&self, mut sample: Sample) -> Sample {
        let app = sample.app.to_lowercase();
        let title = sample.title.to_lowercase();
        let hidden = self.hidden_apps.iter().any(|p| app.contains(p))
            || self.hidden_titles.iter().any(|p| title.contains(p));
        if hidden {
            sample.app = HIDDEN_APP.to_string();
            sample.title.clear();
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_sensitive_titles() {
        let f = PrivacyFilter::default();
        let s = Sample { ts: 0, app: "chrome".into(), title: "Мой банк — вход".into() };
        let out = f.apply(s);
        assert_eq!(out.app, HIDDEN_APP);
        assert!(out.title.is_empty());
    }
}
