fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("hook") {
        if let Err(error) = agent_pin_lib::write_hook_event(&args[2..]) {
            eprintln!("{error}");
            std::process::exit(2);
        }
        return;
    }

    agent_pin_lib::run();
}
