//! Desktop apps launched from Finder, the Dock or a Linux launcher don't
//! inherit the PATH from the user's shell. kubeconfigs often call exec
//! credential plugins (`aws`, `gke-gcloud-auth-plugin`, `kubelogin`) that
//! live in Homebrew or ~/.local paths, so without this, auth fails with
//! "No such file or directory". We ask the login shell for its PATH once at
//! startup, and fall back to adding the usual locations.

#[cfg(unix)]
pub fn fix_path() {
    use std::process::Command;
    let current = std::env::var("PATH").unwrap_or_default();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let from_shell = Command::new(&shell)
        .args(["-ilc", "printf '__TESSERA_PATH__%s' \"$PATH\""])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.rsplit("__TESSERA_PATH__").next().map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty());

    let mut parts: Vec<String> = Vec::new();
    let mut push = |p: &str| {
        if !p.is_empty() && !parts.iter().any(|x| x == p) {
            parts.push(p.to_string());
        }
    };
    if let Some(p) = &from_shell {
        p.split(':').for_each(&mut push);
    }
    current.split(':').for_each(&mut push);
    let home = std::env::var("HOME").unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/snap/bin"] {
        push(extra);
    }
    push(&format!("{home}/.local/bin"));
    push(&format!("{home}/google-cloud-sdk/bin"));
    std::env::set_var("PATH", parts.join(":"));
}

#[cfg(not(unix))]
pub fn fix_path() {}
