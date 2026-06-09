//! Discord Rich Presence: publish today's token total to the user's profile.
//!
//! Runs on a dedicated thread (see [`spawn`]) with its own read `Store`. The
//! presence shows how many tokens were burned since local midnight, e.g.
//! `🔥 123,456 tokens today`, refreshed on a slow interval. Shared by the
//! desktop app AND `cm-serve` (it has no GUI dependency).
//!
//! Resilience: Discord may not be running. The IPC `connect` then fails; we log
//! once and retry on a slow cadence rather than busy-looping or crashing. Any
//! error during an update drops the connection so the next loop reconnects.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};

use cmcore::store::Store;

/// How often the presence is refreshed while connected.
const REFRESH_INTERVAL: Duration = Duration::from_secs(20);

/// How long to wait before retrying after a failed connect/update.
const RETRY_INTERVAL: Duration = Duration::from_secs(45);

/// Spawn the Discord Rich Presence updater on its own thread with its own
/// `Store`. No-op unless `discord_enabled` is set AND a `discord_client_id` is
/// present in settings.json — the integration is opt-in and never ships a
/// hardcoded application id. Connection failures (Discord not running) are
/// handled inside the loop with slow retries; they never crash the process.
pub fn spawn(db_path: PathBuf) {
    let settings = crate::settings::load();
    let Some(client_id) = settings.discord_client_id.clone() else {
        return; // No client id: integration disabled.
    };
    if !settings.discord_enabled || client_id.trim().is_empty() {
        return;
    }

    std::thread::Builder::new()
        .name("cm-discord".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&client_id, &db_path);
            }))
            .is_err()
            {
                eprintln!("[discord] thread panicked");
            }
        })
        .expect("spawn discord thread");
}

/// Run the presence loop forever. `client_id` is the Discord application id;
/// `db_path` is reopened per query (the `Store` is not `Sync`, but this thread
/// owns it for its lifetime). Never returns under normal operation.
pub fn run(client_id: &str, db_path: &Path) {
    let mut client = match DiscordIpcClient::new(client_id) {
        Ok(c) => c,
        Err(e) => {
            // Constructing the client only fails on a malformed id; nothing to
            // retry, so log and give up on the integration.
            eprintln!("[discord] invalid client id, integration disabled: {e}");
            return;
        }
    };

    // Activity start timestamp: when the integration came up (stable for the
    // session, shows an "elapsed" timer in Discord).
    let started_at = unix_seconds();
    let mut connected = false;
    let mut logged_connect_failure = false;

    loop {
        if !connected {
            match client.connect() {
                Ok(()) => {
                    connected = true;
                    logged_connect_failure = false;
                    eprintln!("[discord] connected");
                }
                Err(e) => {
                    // Discord probably isn't running. Log once to avoid spam,
                    // then back off and retry.
                    if !logged_connect_failure {
                        eprintln!("[discord] connect failed (will retry): {e}");
                        logged_connect_failure = true;
                    }
                    std::thread::sleep(RETRY_INTERVAL);
                    continue;
                }
            }
        }

        match update_presence(&mut client, db_path, started_at) {
            Ok(()) => std::thread::sleep(REFRESH_INTERVAL),
            Err(e) => {
                // Drop the connection and reconnect on the next iteration.
                eprintln!("[discord] update failed, reconnecting: {e}");
                let _ = client.close();
                connected = false;
                std::thread::sleep(RETRY_INTERVAL);
            }
        }
    }
}

/// Query today's tokens and push a fresh activity to Discord.
fn update_presence(
    client: &mut DiscordIpcClient,
    db_path: &Path,
    started_at: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let tokens = today_tokens(db_path).unwrap_or(0);
    let details = format!(
        "🔥 {} tokens today",
        crate::util::format_thousands(tokens)
    );
    let activity = activity::Activity::new()
        .details(&details)
        .state("Claude Monitor")
        .timestamps(activity::Timestamps::new().start(started_at));
    client.set_activity(activity)?;
    Ok(())
}

/// Today's local-day token total from the store. Errors (e.g. db missing) are
/// surfaced to the caller, which logs and defaults to 0.
fn today_tokens(db_path: &Path) -> anyhow::Result<i64> {
    let store = Store::open(db_path)?;
    cmcore::query::today_tokens_local(&store)
}

/// Current unix time in whole seconds (Discord timestamps are in seconds).
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
