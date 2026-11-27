#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--service") {
        openremote::run_service();
    } else if args.iter().any(|a| a == "--host-worker") {
        openremote::run_host_worker();
    } else {
        openremote::run();
    }
}
