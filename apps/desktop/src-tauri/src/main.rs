#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]
// Windows release builds use the GUI subsystem so no console opens beside the
// overlay; the tradeoff is that CLI subcommands print nothing there. Hook mode
// still works: the agent passes stdin as an inherited pipe.

use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use agent_pin_core::hook::STDIN_MAX;

/// Longest the `hook` subcommand waits for stdin to finish before using
/// whatever bytes have arrived.
const HOOK_STDIN_DEADLINE: Duration = Duration::from_secs(2);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("hook") => hook(&args[2..]),
        Some("install-hooks") => install_hooks(),
        Some("reset") => reset(),
        _ => agent_pin_lib::run(),
    }
}

/// Bytes read from stdin so far, and whether the reader has finished.
#[derive(Default)]
struct StdinBuf {
    bytes: Vec<u8>,
    done: bool,
}

/// `agent-pin hook <provider> <event>`: records one hook event. Waits on
/// stdin for at most 2 s, then uses whatever bytes arrived (nothing arrived:
/// nothing is recorded). Prints errors only to stderr and always exits 0.
fn hook(args: &[String]) -> ! {
    let shared = Arc::new((Mutex::new(StdinBuf::default()), Condvar::new()));
    let reader = Arc::clone(&shared);
    std::thread::spawn(move || {
        let (lock, cvar) = &*reader;
        let mut stdin = std::io::stdin().lock();
        let mut chunk = [0u8; 8192];
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut buf = lock.lock().unwrap_or_else(|e| e.into_inner());
                    // Keep at most STDIN_MAX + 1 bytes; drain and discard the
                    // rest so the writer never sees a broken pipe.
                    let room = (STDIN_MAX as usize + 1).saturating_sub(buf.bytes.len());
                    buf.bytes.extend_from_slice(&chunk[..n.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        lock.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        cvar.notify_all();
    });

    let input = {
        let (lock, cvar) = &*shared;
        let deadline = Instant::now() + HOOK_STDIN_DEADLINE;
        let mut buf = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !buf.done {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            buf = cvar.wait_timeout(buf, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        std::mem::take(&mut buf.bytes)
    };

    if !input.is_empty() {
        let result = std::panic::catch_unwind(|| {
            agent_pin_core::run_hook(
                args,
                input.as_slice(),
                agent_pin_core::state_dir(),
                agent_pin_lib::now_ms(),
            )
        });
        match result {
            Ok(Err(error)) => eprintln!("agent-pin: {error}"),
            Err(_) => eprintln!("agent-pin: hook panicked"),
            Ok(Ok(())) => {}
        }
    }
    // Exit explicitly so a reader thread stuck on stdin can't keep us alive.
    std::process::exit(0);
}

/// `agent-pin install-hooks`: installs hooks and prints each provider set up.
fn install_hooks() {
    match agent_pin_lib::install_hooks() {
        Ok(ids) if ids.is_empty() => {
            println!("No supported agent config directory found (~/.claude or ~/.codex).")
        }
        Ok(ids) => {
            for id in ids {
                println!("{id}");
            }
        }
        Err(error) => {
            eprintln!("agent-pin: {error}");
            std::process::exit(1);
        }
    }
}

/// `agent-pin reset`: deletes all stored session state.
fn reset() {
    let Some(dir) = agent_pin_core::state_dir() else {
        eprintln!("agent-pin: no state directory: set AGENT_PIN_HOME or the home directory");
        std::process::exit(1);
    };
    if let Err(error) = agent_pin_core::reset(&dir) {
        eprintln!("agent-pin: reset {}: {error}", dir.display());
        std::process::exit(1);
    }
    println!("Cleared {}", dir.join("sessions").display());
}
