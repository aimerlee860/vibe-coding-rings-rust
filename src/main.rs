mod config;
mod data_collector;
mod menubar;
mod providers;
mod server;

use std::sync::Arc;
use tokio::sync::RwLock;

fn main() {
    // Daemon mode: fork into background unless --foreground is given
    let args: Vec<String> = std::env::args().collect();
    let foreground = args.iter().any(|a| a == "--foreground" || a == "-f");

    if !foreground {
        // Check if already running by trying to connect to the port
        if let Ok(_stream) = std::net::TcpStream::connect(("127.0.0.1", server::PORT)) {
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

    let state = Arc::new(RwLock::new(server::AppState {
        goals_changed_callbacks: Vec::new(),
    }));

    // Start web server in background thread
    let server_state = state.clone();
    std::thread::spawn(move || {
        server::start_server(server_state);
    });

    // Run menubar on main thread (macOS requirement)
    menubar::run_menubar();
}
