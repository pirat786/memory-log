use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ActivePowerRule {
    pub condition: String,
    pub action: String,
    pub delay_secs: u64,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PowerStatus {
    pub keep_awake: bool,
    pub rule: Option<ActivePowerRule>,
}

#[derive(Default)]
pub struct PowerGuard {
    pub keep_awake: bool,
    condition: Option<String>,
    action: Option<String>,
    delay_secs: u64,
    started_at: i64,
    low_since: Option<i64>,
}

impl PowerGuard {
    pub fn set_rule(
        &mut self,
        condition: String,
        action: String,
        delay_mins: u64,
        now: i64,
    ) -> Result<(), String> {
        if !matches!(condition.as_str(), "net_idle" | "cpu_idle" | "timer") {
            return Err("Invalid power condition".into());
        }
        if !matches!(action.as_str(), "sleep" | "shutdown") {
            return Err("Invalid power action".into());
        }
        self.condition = Some(condition);
        self.action = Some(action);
        self.delay_secs = delay_mins.max(1) * 60;
        self.started_at = now;
        self.low_since = None;
        Ok(())
    }

    pub fn cancel_rule(&mut self) {
        self.condition = None;
        self.action = None;
        self.low_since = None;
    }

    pub fn status(&self, now: i64) -> PowerStatus {
        let rule = match (self.condition.as_ref(), self.action.as_ref()) {
            (Some(condition), Some(action)) => {
                let elapsed_secs = match condition.as_str() {
                    "timer" => now.saturating_sub(self.started_at) as u64,
                    _ => self
                        .low_since
                        .map(|since| now.saturating_sub(since) as u64)
                        .unwrap_or(0),
                };
                Some(ActivePowerRule {
                    condition: condition.clone(),
                    action: action.clone(),
                    delay_secs: self.delay_secs,
                    elapsed_secs: elapsed_secs.min(self.delay_secs),
                })
            }
            _ => None,
        };
        PowerStatus {
            keep_awake: self.keep_awake,
            rule,
        }
    }

    pub fn evaluate(&mut self, net_bytes_10s: u64, cpu_pct: f32, now: i64) -> Option<String> {
        let condition = self.condition.as_deref()?;
        let triggered = match condition {
            "net_idle" => {
                if net_bytes_10s < 100 * 1024 * 10 {
                    let since = *self.low_since.get_or_insert(now);
                    now.saturating_sub(since) as u64 >= self.delay_secs
                } else {
                    self.low_since = None;
                    false
                }
            }
            "cpu_idle" => {
                if cpu_pct < 10.0 {
                    let since = *self.low_since.get_or_insert(now);
                    now.saturating_sub(since) as u64 >= self.delay_secs
                } else {
                    self.low_since = None;
                    false
                }
            }
            "timer" => now.saturating_sub(self.started_at) as u64 >= self.delay_secs,
            _ => false,
        };
        if triggered {
            let action = self.action.take();
            self.cancel_rule();
            action
        } else {
            None
        }
    }
}
