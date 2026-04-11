mod config;
mod data_collector;
mod menubar;
mod providers;
mod server;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn find_static_dir() -> PathBuf {
    // In a .app bundle, Resources is two levels above the binary
    let exe = std::env::current_exe().unwrap_or_default();
    if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
        let resources = parent.join("Resources").join("static");
        if resources.exists() {
            return resources;
        }
    }
    // Development mode — look for static/ relative to source
    let local = PathBuf::from("static");
    if local.exists() {
        return local;
    }
    // Try relative to Cargo.toml
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")
}

fn main() {
    // Daemon mode: fork into background unless --foreground is given
    let args: Vec<String> = std::env::args().collect();
    let foreground = args.iter().any(|a| a == "--foreground" || a == "-f");

    if !foreground {
        // Check if already running by trying to connect to the port
        if let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", server::PORT)) {
            eprintln!("Vibe Coding Rings is already running on port {}", server::PORT);
            // Open dashboard in browser
            let _ = std::process::Command::new("open")
                .arg(format!("http://localhost:{}", server::PORT))
                .spawn();
            return;
        }

        unsafe {
            match libc::fork() {
                0 => {
                    // Child: detach from terminal
                    libc::setsid();
                    libc::close(0); // stdin
                    libc::close(1); // stdout
                    libc::close(2); // stderr
                }
                pid if pid > 0 => {
                    // Parent: just exit
                    return;
                }
                _ => {
                    eprintln!("Failed to fork");
                    return;
                }
            }
        }
    }

    let static_dir = find_static_dir();
    let state = Arc::new(RwLock::new(server::AppState {
        goals_changed_callbacks: Vec::new(),
    }));

    // Start web server in background thread
    let server_static = static_dir.to_string_lossy().to_string();
    let server_state = state.clone();
    std::thread::spawn(move || {
        server::start_server(server_static, server_state);
    });

    // Run menubar on main thread (macOS requirement)
    menubar::run_menubar();
}
