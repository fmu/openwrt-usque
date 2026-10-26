use std::collections::HashMap;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct HookEnv {
    pub values: HashMap<String, String>,
}

impl HookEnv {
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(key.into(), value.into());
        self
    }
}

pub fn run_hook(path: &str, extra: HookEnv) {
    if path.is_empty() {
        return;
    }

    let event = extra
        .values
        .get("USQUE_EVENT")
        .cloned()
        .unwrap_or_else(|| "hook".to_string());
    let prefix = format!("hook[{event}]");
    let path = path.to_string();
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    let mut keys: Vec<_> = extra.values.keys().cloned().collect();
    keys.sort();
    for key in keys {
        if let Some(value) = extra.values.get(&key) {
            env.push((key, value.clone()));
        }
    }

    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new(&path);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!("{prefix}: failed to start {path:?}: {err}");
                return;
            }
        };

        if let Some(stdout) = child.stdout.take() {
            let prefix = format!("{prefix} stdout");
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!("{prefix}: {line}");
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let prefix = format!("{prefix} stderr");
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!("{prefix}: {line}");
                }
            });
        }

        if let Err(err) = child.wait().await {
            warn!("{prefix}: {path:?} exited with error: {err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_env_builder() {
        let env = HookEnv::default()
            .with("USQUE_MODE", "socks")
            .with("USQUE_EVENT", "connect");
        assert_eq!(
            env.values.get("USQUE_MODE").map(String::as_str),
            Some("socks")
        );
        assert_eq!(
            env.values.get("USQUE_EVENT").map(String::as_str),
            Some("connect")
        );
    }

    #[test]
    fn empty_hook_path_is_noop() {
        run_hook("", HookEnv::default());
    }
}
