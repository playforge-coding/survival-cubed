//! Survival Cubed — a multiplayer-first 2D block game.
//!
//! Run with no arguments for the graphical client (with singleplayer/host/join
//! from the menu). Run `survival-cubed server [port]` for a dedicated headless
//! server that prints its certificate fingerprint for clients to verify.

mod block;
mod client;
mod daylight;
mod discovery;
mod entity;
mod net;
mod protocol;
mod save;
mod server;
mod world;
mod worldgen;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // QUIC/TLS needs a process-wide crypto provider.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("server") => {
            let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5000);
            run_dedicated(port)
        }
        Some(other) => {
            eprintln!("unknown command '{other}'. Usage: survival-cubed [server [port]]");
            std::process::exit(1);
        }
        None => client::run(),
    }
}

fn run_dedicated(port: u16) -> anyhow::Result<()> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(1337);

    let save_dir = save::world_dir(&format!("server-{port}"));
    let mut srv = server::start_server(server::host_bind(port), seed, save_dir.clone())?;
    srv.advertise(&format!("Survival Cubed :{port}"));
    println!("Survival Cubed dedicated server");
    println!("  listening on : {}", srv.addr);
    println!("  world save   : {}", save_dir.display());
    println!(
        "  fingerprint  : {}",
        net::fingerprint_hex(&srv.fingerprint)
    );
    println!("Press Ctrl+C to stop.");

    // Block until the OS asks us to stop, then drop the server: its `Drop`
    // flushes the world to disk, so a clean shutdown loses nothing.
    wait_for_shutdown();
    println!("Shutting down; saving world...");
    drop(srv);
    println!("Saved. Bye.");
    Ok(())
}

/// Park the main thread until the process receives Ctrl+C (SIGINT) or, on Unix,
/// SIGTERM (e.g. `systemctl stop`, `docker stop`).
fn wait_for_shutdown() {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Without a runtime we can't await signals; park forever so the
            // server keeps running and at least its periodic autosave persists.
            log::error!("could not build signal runtime ({e:#}); Ctrl+C save disabled");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    };
    rt.block_on(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    });
}
