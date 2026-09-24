//! Environment guardrails: which contexts are production, what's allowed
//! where, and an optional organisation policy file that users can't override.
//!
//! Policy files are read in this order; later files win, and a system file
//! (normally managed by IT) overrides a user's own file:
//! 1. built-in defaults
//! 2. `~/.tessera/policy.json` (the user's own)
//! 3. system file: macOS `/Library/Application Support/Tessera/policy.json`,
//!    Linux `/etc/tessera/policy.json`, Windows `%ProgramData%\Tessera\policy.json`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Development,
    Other,
    Staging,
    Production,
}

impl Environment {
    /// Protection level: a user may raise a context's environment, never lower it.
    fn rank(self) -> u8 {
        match self {
            Environment::Development => 0,
            Environment::Other => 1,
            Environment::Staging => 2,
            Environment::Production => 3,
        }
    }
    pub fn stricter(self, other: Option<Environment>) -> Environment {
        match other {
            Some(o) if o.rank() > self.rank() => o,
            _ => self,
        }
    }
}

/// Fields are optional so a policy file only needs the rules it sets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyFile {
    pub allow_network_tests: Option<bool>,
    pub allow_network_tests_in_production: Option<bool>,
    pub allow_cloud_checks: Option<bool>,
    pub production_context_patterns: Option<Vec<String>>,
    pub staging_context_patterns: Option<Vec<String>>,
    pub development_context_patterns: Option<Vec<String>>,
    /// If non-empty, only matching contexts are shown or usable.
    pub allowed_contexts: Option<Vec<String>>,
    pub hidden_contexts: Option<Vec<String>>,
    pub audit_log: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub allow_network_tests: bool,
    pub allow_network_tests_in_production: bool,
    pub allow_cloud_checks: bool,
    pub production_context_patterns: Vec<String>,
    pub staging_context_patterns: Vec<String>,
    pub development_context_patterns: Vec<String>,
    pub allowed_contexts: Vec<String>,
    pub hidden_contexts: Vec<String>,
    pub audit_log: bool,
    /// Files that were loaded, in order.
    pub sources: Vec<String>,
    /// Rules set by the system (organisation) file, which the user can't change.
    pub locked: Vec<String>,
    /// Problems reading policy files; the app falls back to the stricter reading.
    pub errors: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
        Policy {
            allow_network_tests: true,
            allow_network_tests_in_production: false,
            allow_cloud_checks: true,
            production_context_patterns: v(&["*prod*", "*prd*", "*production*", "*live*"]),
            staging_context_patterns: v(&["*stag*", "*stg*", "*uat*", "*preprod*", "*qa*"]),
            development_context_patterns: v(&[
                "*dev*",
                "*test*",
                "*sandbox*",
                "*lab*",
                "kind-*",
                "minikube",
                "docker-desktop",
                "*local*",
            ]),
            allowed_contexts: vec![],
            hidden_contexts: vec![],
            audit_log: true,
            sources: vec![],
            locked: vec![],
            errors: vec![],
        }
    }
}

impl Policy {
    fn apply(&mut self, f: PolicyFile, lock: bool) {
        let set = |name: &str, locked: &mut Vec<String>| {
            if lock && !locked.iter().any(|l| l == name) {
                locked.push(name.to_string());
            }
        };
        if let Some(x) = f.allow_network_tests {
            self.allow_network_tests = x;
            set("allowNetworkTests", &mut self.locked);
        }
        if let Some(x) = f.allow_network_tests_in_production {
            self.allow_network_tests_in_production = x;
            set("allowNetworkTestsInProduction", &mut self.locked);
        }
        if let Some(x) = f.allow_cloud_checks {
            self.allow_cloud_checks = x;
            set("allowCloudChecks", &mut self.locked);
        }
        if let Some(x) = f.production_context_patterns {
            self.production_context_patterns = x;
            set("productionContextPatterns", &mut self.locked);
        }
        if let Some(x) = f.staging_context_patterns {
            self.staging_context_patterns = x;
            set("stagingContextPatterns", &mut self.locked);
        }
        if let Some(x) = f.development_context_patterns {
            self.development_context_patterns = x;
            set("developmentContextPatterns", &mut self.locked);
        }
        if let Some(x) = f.allowed_contexts {
            self.allowed_contexts = x;
            set("allowedContexts", &mut self.locked);
        }
        if let Some(x) = f.hidden_contexts {
            self.hidden_contexts = x;
            set("hiddenContexts", &mut self.locked);
        }
        if let Some(x) = f.audit_log {
            self.audit_log = x;
            set("auditLog", &mut self.locked);
        }
    }

    /// Build a policy from explicit files (user first, then system).
    pub fn from_files(user: Option<&Path>, system: Option<&Path>) -> Policy {
        let mut p = Policy::default();
        for (path, lock) in [(user, false), (system, true)] {
            let Some(path) = path else { continue };
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            match serde_json::from_str::<PolicyFile>(&text) {
                Ok(f) => {
                    p.apply(f, lock);
                    p.sources.push(path.display().to_string());
                }
                Err(e) => {
                    // Fail safe: an unreadable organisation policy disables active features.
                    p.errors.push(format!("Couldn't read {}: {e}", path.display()));
                    if lock {
                        p.allow_network_tests = false;
                        p.allow_cloud_checks = false;
                        p.locked.extend(["allowNetworkTests".to_string(), "allowCloudChecks".to_string()]);
                    }
                }
            }
        }
        p
    }

    pub fn load() -> Policy {
        Policy::from_files(user_policy_path().as_deref(), system_policy_path().as_deref())
    }

    pub fn classify(&self, context: &str) -> Environment {
        let c = context.to_lowercase();
        let any = |pats: &[String]| pats.iter().any(|p| glob(&p.to_lowercase(), &c));
        // Production wins over everything, so "prod-dev-copy" counts as production.
        if any(&self.production_context_patterns) {
            Environment::Production
        } else if any(&self.staging_context_patterns) {
            Environment::Staging
        } else if any(&self.development_context_patterns) {
            Environment::Development
        } else {
            Environment::Other
        }
    }

    pub fn context_allowed(&self, context: &str) -> bool {
        let c = context.to_lowercase();
        let m = |pats: &[String]| pats.iter().any(|p| glob(&p.to_lowercase(), &c));
        (self.allowed_contexts.is_empty() || m(&self.allowed_contexts)) && !m(&self.hidden_contexts)
    }

    /// Why a network test may not run here, if it may not.
    pub fn network_test_blocked(&self, env: Environment) -> Option<String> {
        if !self.allow_network_tests {
            return Some("Network tests are turned off by policy.".into());
        }
        if env == Environment::Production && !self.allow_network_tests_in_production {
            return Some("Network tests are turned off for production contexts. An administrator can allow them with allowNetworkTestsInProduction in the policy file.".into());
        }
        None
    }
}

pub fn user_policy_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".tessera").join("policy.json"))
}

pub fn system_policy_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/Library/Application Support/Tessera/policy.json"))
    } else if cfg!(windows) {
        std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("Tessera").join("policy.json"))
    } else {
        Some(PathBuf::from("/etc/tessera/policy.json"))
    }
}

/// Minimal glob: `*` matches any run of characters.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            return text.len() >= pos + part.len() && text.ends_with(part);
        } else {
            match text[pos..].find(part) {
                Some(j) => pos += j + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_example_policy_is_valid() {
        let f: PolicyFile = serde_json::from_str(include_str!("../../../docs/policy.example.json")).unwrap();
        assert_eq!(f.allow_network_tests_in_production, Some(false));
    }

    #[test]
    fn globbing() {
        assert!(glob("*prod*", "arn:aws:eks:us-west-2:1:cluster/prod-payments"));
        assert!(glob("kind-*", "kind-tessera-lab"));
        assert!(!glob("kind-*", "my-kind-lab"));
        assert!(glob("minikube", "minikube"));
        assert!(glob("a*c*e", "abcde"));
        assert!(!glob("a*c*e", "abcd"));
    }

    #[test]
    fn classification_prefers_production() {
        let p = Policy::default();
        assert_eq!(p.classify("prod-usw2-payments"), Environment::Production);
        assert_eq!(p.classify("dev-copy-of-prod"), Environment::Production);
        assert_eq!(p.classify("stg-use1-api"), Environment::Staging);
        assert_eq!(p.classify("kind-tessera-lab"), Environment::Development);
        assert_eq!(p.classify("payments-east"), Environment::Other);
    }

    #[test]
    fn overrides_can_only_raise_protection() {
        assert_eq!(Environment::Development.stricter(Some(Environment::Production)), Environment::Production);
        assert_eq!(Environment::Production.stricter(Some(Environment::Development)), Environment::Production);
    }

    #[test]
    fn production_blocks_network_tests_by_default() {
        let p = Policy::default();
        assert!(p.network_test_blocked(Environment::Production).is_some());
        assert!(p.network_test_blocked(Environment::Development).is_none());
    }

    #[test]
    fn system_file_overrides_user_and_locks() {
        let dir = std::env::temp_dir().join(format!("tessera-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.json");
        let system = dir.join("system.json");
        std::fs::write(&user, r#"{"allowNetworkTestsInProduction": true, "hiddenContexts": ["old-*"]}"#).unwrap();
        std::fs::write(
            &system,
            r#"{"allowNetworkTestsInProduction": false, "allowedContexts": ["*-payments", "old-*"]}"#,
        )
        .unwrap();
        let p = Policy::from_files(Some(&user), Some(&system));
        assert!(!p.allow_network_tests_in_production);
        assert!(p.locked.contains(&"allowNetworkTestsInProduction".to_string()));
        assert!(p.context_allowed("prod-usw2-payments"));
        assert!(!p.context_allowed("prod-usw2-orders"));
        assert!(!p.context_allowed("old-payments"), "hidden wins over allowed");
        std::fs::write(&system, "{ not json").unwrap();
        let broken = Policy::from_files(None, Some(&system));
        assert!(!broken.allow_network_tests && !broken.allow_cloud_checks, "fails safe");
        assert_eq!(broken.errors.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
