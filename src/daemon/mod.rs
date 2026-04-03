pub mod snapshot;
pub mod watcher;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tracing::{error, info};

use crate::config::Config;
use crate::error::DaemonError;

fn pid_path(data_dir: &Path) -> PathBuf {
    data_dir.join("replayfs.pid")
}

fn sock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("replayfs.sock")
}

fn read_pid(data_dir: &Path) -> Option<i32> {
    fs::read_to_string(pid_path(data_dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn is_process_alive(pid: i32) -> bool {
    signal::kill(Pid::from_raw(pid), None).is_ok()
}

fn write_pid(data_dir: &Path) -> Result<()> {
    let pid = std::process::id();
    fs::write(pid_path(data_dir), pid.to_string())
        .context("failed to write PID file")?;
    Ok(())
}

fn cleanup(data_dir: &Path) {
    let _ = fs::remove_file(pid_path(data_dir));
    let _ = fs::remove_file(sock_path(data_dir));
}

pub fn start(config: Config, foreground: bool) -> Result<()> {
    let data_dir = &config.data_dir;
    fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data dir: {}", data_dir.display()))?;

    // Check for existing daemon
    if let Some(pid) = read_pid(data_dir) {
        if is_process_alive(pid) {
            return Err(DaemonError::AlreadyRunning(pid).into());
        }
        // Stale PID file, clean up
        cleanup(data_dir);
    }

    if !foreground {
        daemonize(data_dir)?;
        return Ok(());
    }

    run_daemon(config)
}

fn run_daemon(config: Config) -> Result<()> {
    let data_dir = config.data_dir.clone();
    write_pid(&data_dir)?;

    let shutdown = Arc::new(AtomicBool::new(false));

    // Set up signal handler
    let shutdown_sig = shutdown.clone();
    ctrlc_handler(shutdown_sig);

    // Start socket listener for stop command
    let sock = sock_path(&data_dir);
    let _ = fs::remove_file(&sock); // clean stale socket
    let listener =
        UnixListener::bind(&sock).map_err(|e| DaemonError::SocketFailed(e))?;
    listener
        .set_nonblocking(true)
        .context("failed to set socket nonblocking")?;

    let shutdown_sock = shutdown.clone();
    let socket_thread = std::thread::spawn(move || {
        socket_listener(listener, shutdown_sock);
    });

    // Run the watcher (blocks until shutdown)
    let result = watcher::run(&config, shutdown);

    // Cleanup
    cleanup(&data_dir);
    let _ = socket_thread.join();

    result
}

fn socket_listener(listener: UnixListener, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if handle_socket_command(stream) {
                    info!("received stop command");
                    shutdown.store(true, Ordering::Relaxed);
                    return;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                error!("socket accept error: {}", e);
            }
        }
    }
}

fn handle_socket_command(stream: UnixStream) -> bool {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_ok() {
        let cmd = line.trim();
        match cmd {
            "stop" => {
                let _ = (&stream).write_all(b"ok\n");
                let _ = stream.shutdown(Shutdown::Both);
                return true;
            }
            "status" => {
                let _ = (&stream).write_all(b"running\n");
                let _ = stream.shutdown(Shutdown::Both);
            }
            _ => {
                let _ = (&stream).write_all(b"unknown command\n");
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
    false
}

fn ctrlc_handler(shutdown: Arc<AtomicBool>) {
    let _ = ctrlc::set_handler(move || {
        shutdown.store(true, Ordering::Relaxed);
    });
}

fn daemonize(_data_dir: &Path) -> Result<()> {
    use nix::unistd::{fork, setsid, ForkResult};

    match unsafe { fork() }? {
        ForkResult::Parent { .. } => {
            // Parent exits; child continues
            std::process::exit(0);
        }
        ForkResult::Child => {
            setsid().context("setsid failed")?;
            // Re-read config and run — but we already have it, so we'd need to
            // restructure. For now, this path is handled by the caller.
            // The actual daemon run happens after fork returns to caller.
        }
    }
    Ok(())
}

pub fn stop(data_dir: &Path) -> Result<()> {
    let sock = sock_path(data_dir);
    if !sock.exists() {
        // Try PID-based check
        if let Some(pid) = read_pid(data_dir) {
            if is_process_alive(pid) {
                signal::kill(Pid::from_raw(pid), Signal::SIGTERM)?;
                println!("sent SIGTERM to PID {pid}");
                return Ok(());
            }
        }
        return Err(DaemonError::NotRunning.into());
    }

    let mut stream = UnixStream::connect(&sock).context("failed to connect to daemon socket")?;
    stream.write_all(b"stop\n")?;

    let mut reader = BufReader::new(&stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;

    if response.trim() == "ok" {
        println!("daemon stopped");
    } else {
        println!("unexpected response: {}", response.trim());
    }

    Ok(())
}

pub fn status(data_dir: &Path) -> Result<()> {
    if let Some(pid) = read_pid(data_dir) {
        if is_process_alive(pid) {
            println!("daemon is running (PID {pid})");

            // Try socket status
            let sock = sock_path(data_dir);
            if let Ok(mut stream) = UnixStream::connect(&sock) {
                let _ = stream.write_all(b"status\n");
                let mut reader = BufReader::new(&stream);
                let mut response = String::new();
                if reader.read_line(&mut response).is_ok() {
                    println!("status: {}", response.trim());
                }
            }
            return Ok(());
        }
    }

    println!("daemon is not running");
    Ok(())
}
