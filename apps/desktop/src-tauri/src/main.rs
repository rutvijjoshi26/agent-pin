use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

use agent_pin_core::hook::STDIN_MAX;

/// Longest the `hook` subcommand waits for stdin before running without it.
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

/// `agent-pin hook <provider> <event>`: records one hook event. Never waits
/// on stdin for more than 2 s, prints errors only to stderr, always exits 0.
fn hook(args: &[String]) -> ! {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = Vec::new();
        let _ = (&mut stdin).take(STDIN_MAX + 1).read_to_end(&mut buf);
        if buf.len() as u64 > STDIN_MAX {
            // Drain the rest (within the same deadline) so the writer never
            // sees a broken pipe; run_hook salvages what it needs from `buf`.
            let _ = std::io::copy(&mut stdin, &mut std::io::sink());
        }
        let _ = tx.send(buf);
    });
    let input = rx.recv_timeout(HOOK_STDIN_DEADLINE).unwrap_or_default();

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
