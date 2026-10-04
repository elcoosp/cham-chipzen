//! Runnable entry point — port of `run.py` from the reference bot.
//!
//! Connects to the lobby, waits to be matched, plays one match end-to-end, and
//! exits (`--loop` keeps cycling). With `--agent-dir` (default `artifacts/agent`)
//! pointing at a CHAMELEON artifact bundle, decisions come from the routed
//! blueprint agent; otherwise the trivial check/call/fold reference policy runs.

use cham_chipzen::agent::ChamBrain;
use cham_chipzen::client::run_once;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "cham-chipzen",
    about = "Chipzen External-API bot driven by the CHAMELEON poker agent",
    version
)]
struct Args {
    /// Platform origin (e.g. wss://staging.chipzen.ai or ws://localhost:8001).
    /// Env: CHIPZEN_BASE_URL
    #[arg(long, env = "CHIPZEN_BASE_URL")]
    base_url: Option<String>,

    /// The External-API bot's UUID. Env: CHIPZEN_BOT_ID
    #[arg(long, env = "CHIPZEN_BOT_ID")]
    bot_id: Option<String>,

    /// The cz_extbot_ API token. Env: CHIPZEN_EXTBOT_TOKEN
    #[arg(long, env = "CHIPZEN_EXTBOT_TOKEN")]
    token: Option<String>,

    /// Keep cycling: after a match ends, reconnect the lobby and wait for the
    /// next match.
    #[arg(long)]
    r#loop: bool,

    /// CHAMELEON artifact bundle directory (layout produced by cham-cli
    /// train-buckets + train-bp + train-router). Default: artifacts/agent
    #[arg(long, default_value = "artifacts/agent")]
    agent_dir: PathBuf,

    /// Routing mode passed to cham_agent::loader (mixture|argmax|robust-only|bayes).
    #[arg(long, default_value = "mixture")]
    routing: String,

    /// Stack depth in bb for the shadow engine (must match the trained set).
    #[arg(long, default_value_t = 100)]
    depth_bb: i64,

    /// Deterministic seed for hole sampling / decision draws per match.
    #[arg(long, default_value_t = 0xCE41_3E7)]
    seed: u64,

    /// Enable debug logging (or set RUST_LOG).
    #[arg(short, long)]
    verbose: bool,

    /// Refuse to run without a loadable CHAMELEON artifact bundle. When set,
    /// a load failure is a fatal error instead of a WARN + trivial-policy
    /// fallback. Use this in production so a bot cannot silently run on the
    /// reference policy when the real brain fails to load.
    #[arg(long)]
    require_agent: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();

    let filter = if args.verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        base_url = ?args.base_url,
        bot_id = ?args.bot_id,
        token_len = args.token.as_ref().map(|t| t.len()),
        loop_mode = args.r#loop,
        agent_dir = %args.agent_dir.display(),
        routing = %args.routing,
        depth_bb = args.depth_bb,
        seed = format!("0x{:x}", args.seed),
        "cham-chipzen starting"
    );

    // Required-argument check (mirrors run.py's exit code 2 path).
    let missing: Vec<&str> = [
        ("--base-url", &args.base_url),
        ("--bot-id", &args.bot_id),
        ("--token", &args.token),
    ]
    .iter()
    .filter(|(_, v)| v.is_none())
    .map(|(n, _)| *n)
    .collect();
    if !missing.is_empty() {
        eprintln!(
            "error: missing required argument(s): {}\n\
             pass them as flags or via CHIPZEN_BASE_URL / CHIPZEN_BOT_ID / CHIPZEN_EXTBOT_TOKEN",
            missing.join(", ")
        );
        return ExitCodes::usage();
    }
    let base_url = args.base_url.as_deref().unwrap();
    let bot_id = args.bot_id.as_deref().unwrap();
    let token = args.token.as_deref().unwrap();

    // CHAMELEON brain: load artifacts when available; degrade to the trivial
    // reference policy (like the Python example) when not — unless the
    // operator explicitly required a real agent (`--require-agent`).
    let brain = match ChamBrain::load(&args.agent_dir, &args.routing, args.depth_bb, args.seed) {
        Ok(b) => Some(Arc::new(Mutex::new(b))),
        Err(e) => {
            if args.require_agent {
                eprintln!(
                    "error: --require-agent set but CHAMELEON bundle failed to load: {e}"
                );
                return ExitCodes::fail();
            }
            tracing::warn!("running with TRIVIAL reference policy: {e}");
            None
        }
    };

    loop {
        match run_once(base_url, bot_id, token, brain.clone()).await {
            Ok(Some(end)) => {
                println!(
                    "match ended: reason={} results={}",
                    end.reason
                        .as_deref()
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "?".into()),
                    end
                        .results
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "null".into())
                );
            }
            Ok(None) => println!("match ended without a clean match_end frame"),
            Err(e) => {
                eprintln!("error: {e}");
                if !args.r#loop {
                    return ExitCodes::fail();
                }
                tracing::warn!("reconnecting lobby after error (loop mode)");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        }
        if !args.r#loop {
            break;
        }
    }
    ExitCodes::ok()
}

/// Process exit codes mirroring run.py (0 ok, 2 usage, 1 failure, 130 ^C is
/// handled by the shell/job control). Provided as fns because
/// `ExitCode::from(u8)` is not a const fn.
struct ExitCodes;

impl ExitCodes {
    fn ok() -> std::process::ExitCode {
        std::process::ExitCode::SUCCESS
    }
    fn fail() -> std::process::ExitCode {
        std::process::ExitCode::from(1)
    }
    fn usage() -> std::process::ExitCode {
        std::process::ExitCode::from(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_have_expected_values() {
        assert_eq!(ExitCodes::ok(), std::process::ExitCode::SUCCESS);
        // ExitCode doesn't expose its inner u8 directly; we can at least
        // assert that fail() and usage() are distinct from ok() and from each
        // other by re-parsing the Debug repr (stable for these variants).
        let dbg_fail = format!("{:?}", ExitCodes::fail());
        let dbg_usage = format!("{:?}", ExitCodes::usage());
        let dbg_ok = format!("{:?}", ExitCodes::ok());
        assert_ne!(dbg_fail, dbg_ok);
        assert_ne!(dbg_usage, dbg_ok);
        assert_ne!(dbg_fail, dbg_usage);
    }

    #[test]
    fn args_parse_with_all_required_flags() {
        let args = Args::try_parse_from([
            "cham-chipzen",
            "--base-url",
            "wss://staging.chipzen.ai",
            "--bot-id",
            "bot-1",
            "--token",
            "cz_extbot_abc",
        ])
        .expect("args should parse");
        assert_eq!(args.base_url.as_deref(), Some("wss://staging.chipzen.ai"));
        assert_eq!(args.bot_id.as_deref(), Some("bot-1"));
        assert_eq!(args.token.as_deref(), Some("cz_extbot_abc"));
        assert!(!args.r#loop);
        assert!(!args.require_agent);
        assert_eq!(args.routing, "mixture");
        assert_eq!(args.depth_bb, 100);
    }

    #[test]
    fn args_default_agent_dir_is_artifacts_agent() {
        let args = Args::try_parse_from([
            "cham-chipzen",
            "--base-url",
            "wss://x",
            "--bot-id",
            "b",
            "--token",
            "t",
        ])
        .unwrap();
        assert_eq!(args.agent_dir, PathBuf::from("artifacts/agent"));
    }

    #[test]
    fn args_accept_loop_and_require_agent() {
        let args = Args::try_parse_from([
            "cham-chipzen",
            "--base-url",
            "wss://x",
            "--bot-id",
            "b",
            "--token",
            "t",
            "--loop",
            "--require-agent",
        ])
        .unwrap();
        assert!(args.r#loop);
        assert!(args.require_agent);
    }

    #[test]
    fn args_parse_with_no_flags_leaves_required_fields_none() {
        // The three "required" fields (`base_url`, `bot_id`, `token`) are
        // modeled as `Option<String>` so clap accepts their absence; main()
        // performs the presence check afterwards (mirroring run.py's exit-2
        // path). When no CLI flags and no env vars are set, all three are
        // `None` and the missing-arg check fires.
        //
        // If a caller's environment happens to define CHIPZEN_BASE_URL /
        // CHIPZEN_BOT_ID / CHIPZEN_EXTBOT_TOKEN, clap's `env` feature will
        // populate them; we therefore skip the None assertion in that case.
        let args = Args::try_parse_from(["cham-chipzen"]).expect("parse should succeed");
        let any_env_set = std::env::var_os("CHIPZEN_BASE_URL").is_some()
            || std::env::var_os("CHIPZEN_BOT_ID").is_some()
            || std::env::var_os("CHIPZEN_EXTBOT_TOKEN").is_some();
        if !any_env_set {
            assert!(args.base_url.is_none());
            assert!(args.bot_id.is_none());
            assert!(args.token.is_none());
        }
    }

    #[test]
    fn args_accept_routing_and_depth_overrides() {
        let args = Args::try_parse_from([
            "cham-chipzen",
            "--base-url",
            "wss://x",
            "--bot-id",
            "b",
            "--token",
            "t",
            "--routing",
            "argmax",
            "--depth-bb",
            "200",
            "--seed",
            "42",
        ])
        .unwrap();
        assert_eq!(args.routing, "argmax");
        assert_eq!(args.depth_bb, 200);
        assert_eq!(args.seed, 42);
    }
}
