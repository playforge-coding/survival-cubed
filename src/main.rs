//! Survival Cubed — a multiplayer-first 2D block game.
//!
//! Run with no arguments for the graphical client (with singleplayer/host/join
//! from the menu). Run `survival-cubed server [port]` for a dedicated headless
//! server that prints its certificate fingerprint for clients to verify.

mod block;
mod client;
mod daylight;
mod entity;
mod net;
mod protocol;
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

    let srv = server::start_server(server::host_bind(port), seed)?;
    println!("Survival Cubed dedicated server");
    println!("  listening on : {}", srv.addr);
    println!(
        "  fingerprint  : {}",
        net::fingerprint_hex(&srv.fingerprint)
    );
    println!("Press Ctrl+C to stop.");

    // Keep the process (and thus the server) alive.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
