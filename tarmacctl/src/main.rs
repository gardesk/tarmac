use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct Request {
    command: String,
    args: Vec<String>,
}

#[derive(Deserialize)]
struct Response {
    success: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

fn socket_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("TARMAC_SOCKET") {
        return std::path::PathBuf::from(path);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    std::path::PathBuf::from(format!("/tmp/tarmac-{}.sock", user))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        eprintln!("tarmacctl — control tarmac window manager");
        eprintln!();
        eprintln!("Usage: tarmacctl <command> [args...]");
        eprintln!();
        eprintln!("Commands:");
        eprintln!("  focus <left|right|up|down>    Focus window in direction");
        eprintln!("  swap <left|right|up|down>     Swap with window in direction");
        eprintln!("  move <left|right|up|down>     Relocate focused window in direction (HX-04)");
        eprintln!("  resize <left|right|up|down>   Resize split in direction");
        eprintln!("  set_ratio <0.1..0.9>          Set focused window's parent split ratio (HX-02)");
        eprintln!("  close                         Close focused window");
        eprintln!("  equalize                      Equalize all splits");
        eprintln!("  unstack                       Restore the focused stacked subtree");
        eprintln!("  promote_stack                 Move the focused stack window to the top");
        eprintln!("  toggle_split                  Flip focused window's parent split axis (HX-01)");
        eprintln!("  swap_split                    Swap left/right children at parent split (HX-01)");
        eprintln!("  promote_to_root               Lift focused window to a direct workspace-root child (HX-01)");
        eprintln!("  preselect <left|right|up|down>");
        eprintln!("                                Hint next new window's direction (HX-05)");
        eprintln!("  toggle_pseudo                 Toggle pseudo-tile (preferred size, centered) (HX-06)");
        eprintln!("  add_to_stack <left|right|up|down>");
        eprintln!("                                Merge focused window into adjacent leaf as Stack (HX-07)");
        eprintln!("  split_from_stack              Pull active stack member out as sibling (HX-07)");
        eprintln!("  cancel_drag                   Abort an in-flight drag and restore pre-drag state (HX-99-C)");
        eprintln!("  workspace <1-10>              Switch workspace");
        eprintln!("  move-to-workspace <1-10>      Move window to workspace");
        eprintln!("  toggle-floating               Toggle floating state");
        eprintln!("  reload                        Hot reload config");
        eprintln!("  get-workspaces                Get workspace info");
        eprintln!("  get-focused                   Get focused window info");
        eprintln!("  get-windows                   Get all window info");
        eprintln!("  get-monitors                  Get monitor info");
        eprintln!("  get-tree [workspace]           Get layout tree for workspace");
        eprintln!(
            "  subscribe [event_types...]    Stream events (workspace_changed, window_focused, etc.)"
        );
        eprintln!("  exec <command>                Execute shell command");
        std::process::exit(1);
    }

    let command = args[0].clone();
    let cmd_args = args[1..].to_vec();

    let request = Request {
        command,
        args: cmd_args,
    };

    let path = socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not connect to tarmac at {:?}: {}", path, e);
            eprintln!("is tarmac running?");
            std::process::exit(1);
        }
    };

    let is_subscribe = request.command == "subscribe";

    let mut writer = &stream;
    let json = serde_json::to_string(&request).expect("serialize request");
    writer.write_all(json.as_bytes()).expect("write request");
    writer.write_all(b"\n").expect("write newline");
    writer.flush().expect("flush");

    // For subscribe, keep connection open for streaming.
    // For regular commands, shut down write side.
    if !is_subscribe {
        stream.shutdown(std::net::Shutdown::Write).ok();
    }

    let reader = BufReader::new(&stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        if is_subscribe {
            // In subscribe mode, print each event line directly.
            // The first line is the ack response, then streaming events.
            println!("{}", line);
            continue;
        }

        let response: Response = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: invalid response: {}", e);
                std::process::exit(1);
            }
        };

        if response.success {
            if let Some(data) = response.data {
                println!("{}", serde_json::to_string_pretty(&data).unwrap());
            }
        } else {
            eprintln!(
                "error: {}",
                response.error.unwrap_or_else(|| "unknown".to_string())
            );
            std::process::exit(1);
        }
    }
}
