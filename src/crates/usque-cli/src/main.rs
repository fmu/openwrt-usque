use anyhow::Result;
use clap::Parser;
use tracing::level_filters::LevelFilter;
use usque_cli::Cli;
use usque_crypto::init as init_crypto;

/// Parse the subset of `RUST_LOG` this daemon documents: a global level
/// (`error`/`warn`/`info`/`debug`/`trace`) and/or `usque=<level>` directives.
/// The full `tracing` env-filter syntax (regex-backed) is not compiled in: it
/// costs ~1/6 MB of `.text` on armv7 for functionality nobody uses here.
fn log_level_from_env() -> LevelFilter {
    let Some(spec) = std::env::var("RUST_LOG").ok().filter(|v| !v.trim().is_empty()) else {
        return LevelFilter::INFO;
    };
    let mut level = None;
    for directive in spec.split(',') {
        let directive = directive.trim();
        let candidate = match directive.split_once('=') {
            // Per-target directives: only `usque` (and its crates) matter to
            // operators; other targets keep the global level.
            Some((target, level)) if target.trim().starts_with("usque") => Some(level.trim()),
            Some(_) => None,
            None => Some(directive),
        };
        if let Some(candidate) = candidate {
            if let Ok(parsed) = candidate.parse::<LevelFilter>() {
                level = Some(parsed);
            }
        }
    }
    level.unwrap_or(LevelFilter::INFO)
}

#[tokio::main]
async fn main() -> Result<()> {
    init_crypto();
    // Tune crossfire's channel backoff for the host: on single-core
    // machines (VPS/VM) the lockless channels yield instead of
    // spinning, which crossfire reports as a ~2x gain there. No-op on
    // multi-core; idempotent, so library-level callers are safe too.
    crossfire::detect_backoff_cfg();

    tracing_subscriber::fmt()
        .with_max_level(log_level_from_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    usque_cli::cmd::execute(cli.command, &cli.config).await
}

#[cfg(test)]
mod tests {
    use super::log_level_from_env;
    use tracing::level_filters::LevelFilter;

    fn with_rust_log<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let previous = std::env::var("RUST_LOG").ok();
        match value {
            Some(value) => std::env::set_var("RUST_LOG", value),
            None => std::env::remove_var("RUST_LOG"),
        }
        let result = f();
        match previous {
            Some(previous) => std::env::set_var("RUST_LOG", previous),
            None => std::env::remove_var("RUST_LOG"),
        }
        result
    }

    #[test]
    fn defaults_to_info() {
        assert_eq!(with_rust_log(None, log_level_from_env), LevelFilter::INFO);
        assert_eq!(with_rust_log(Some(""), log_level_from_env), LevelFilter::INFO);
    }

    #[test]
    fn parses_global_and_target_levels() {
        assert_eq!(with_rust_log(Some("debug"), log_level_from_env), LevelFilter::DEBUG);
        assert_eq!(
            with_rust_log(Some("warn,usque=trace"), log_level_from_env),
            LevelFilter::TRACE
        );
        assert_eq!(
            with_rust_log(Some("quinn=debug,usque=error"), log_level_from_env),
            LevelFilter::ERROR
        );
        assert_eq!(with_rust_log(Some("nonsense"), log_level_from_env), LevelFilter::INFO);
    }
}
