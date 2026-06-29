//! Survival Cubed — a multiplayer-first 2D block game.
//!
//! Run with no arguments for the graphical client (with singleplayer/host/join
//! from the menu). Run
//! `survival-cubed server [port] [creator] [upnp] [voice] [voice-port=N]` for a
//! dedicated headless server that prints its certificate fingerprint for clients
//! to verify. Pass `creator` to make it a creator-type server (every player may
//! enter creator mode); omit it for a survival server. Pass `upnp` to forward
//! the port on the local router via UPnP (exposes the server to the internet —
//! see [`upnp::SECURITY_WARNING`]). Pass `voice` to enable voice chat over a MOQ
//! relay (see [`voice`]); pass `webcam` to enable webcam video over the same relay
//! (a separate toggle); `voice-port=N` overrides the relay's UDP port (default:
//! game port + 1).

mod assets;
mod auth;
mod block;
mod client;
mod daylight;
mod discovery;
mod entity;
mod inventory;
mod net;
mod protocol;
mod recipe;
mod save;
mod server;
mod structure;
mod upnp;
mod voice;
mod voice_relay;
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
            // Remaining tokens are order-independent flags: `creator` makes this
            // a creator-type server; `upnp` opens the port on the router.
            let flags: Vec<String> = args.collect();
            let creator_world = flags.iter().any(|f| f == "creator");
            let upnp = flags.iter().any(|f| f == "upnp");
            // `voice` enables the optional voice-chat relay; `webcam` enables the
            // optional webcam video (a separate toggle sharing the same relay);
            // `voice-port=N` overrides the relay port (default: game port + 1).
            let voice = flags.iter().any(|f| f == "voice");
            let webcam = flags.iter().any(|f| f == "webcam");
            let voice_port = flags
                .iter()
                .find_map(|f| f.strip_prefix("voice-port=").and_then(|s| s.parse().ok()))
                .unwrap_or(port.wrapping_add(1));
            run_dedicated(port, creator_world, upnp, voice, webcam, voice_port)
        }
        Some(other) => {
            eprintln!("unknown command '{other}'. Usage: survival-cubed [server [port]]");
            std::process::exit(1);
        }
        None => client::run(),
    }
}

fn run_dedicated(
    port: u16,
    creator_world: bool,
    upnp: bool,
    voice: bool,
    webcam: bool,
    voice_port: u16,
) -> anyhow::Result<()> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(1337);

    let save_dir = save::world_dir(&format!("server-{port}"));
    let mut srv = server::start_server(
        server::host_bind(port),
        seed,
        save_dir.clone(),
        creator_world,
    )?;
    srv.advertise(&format!("Survival Cubed :{port}"));
    if upnp {
        // Surface the warning before opening the port so an operator scanning
        // the logs sees exactly what was exposed and why.
        eprintln!("WARNING: UPnP enabled. {}", upnp::SECURITY_WARNING);
        srv.forward_port();
    }
    // Optional voice-chat relay. Failure is non-fatal: the game server keeps
    // running without voice.
    let voice_status = if voice {
        match srv.enable_voice(server::host_bind(voice_port), upnp) {
            Ok(p) => format!("on (port {p})"),
            Err(e) => {
                eprintln!("WARNING: voice chat failed to start: {e:#}");
                "failed".to_string()
            }
        }
    } else {
        "off".to_string()
    };
    // Optional webcam video. A separate toggle from voice, sharing the same relay
    // (so the second of the two to start just reuses the first's endpoint).
    let webcam_status = if webcam {
        match srv.enable_webcam(server::host_bind(voice_port), upnp) {
            Ok(p) => format!("on (port {p})"),
            Err(e) => {
                eprintln!("WARNING: webcam video failed to start: {e:#}");
                "failed".to_string()
            }
        }
    } else {
        "off".to_string()
    };
    println!("Survival Cubed dedicated server");
    println!("  listening on : {}", srv.addr);
    println!("  world save   : {}", save_dir.display());
    println!(
        "  mode         : {}",
        if creator_world { "creator" } else { "survival" }
    );
    println!("  upnp         : {}", if upnp { "on" } else { "off" });
    println!("  voice        : {voice_status}");
    println!("  webcam       : {webcam_status}");
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
