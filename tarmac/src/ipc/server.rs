use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{mpsc, Arc};

use super::events::EventBus;
use super::protocol::{Request, Response, socket_path};

/// A command received from an IPC client, with a channel to send the response back.
pub struct IpcCommand {
    pub request: Request,
    pub response_tx: mpsc::Sender<Response>,
}

/// Start the IPC server on a background thread.
/// Returns a receiver for incoming commands.
pub fn start_server(event_bus: Arc<EventBus>) -> mpsc::Receiver<IpcCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let path = socket_path();

        // Clean up stale socket from previous run
        let _ = std::fs::remove_file(&path);

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(err = %e, ?path, "failed to bind IPC socket");
                return;
            }
        };

        // Set permissions 0600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }

        tracing::info!(?path, "IPC server listening");

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = cmd_tx.clone();
                    let bus = event_bus.clone();
                    std::thread::spawn(move || handle_connection(stream, tx, bus));
                }
                Err(e) => {
                    tracing::warn!(err = %e, "IPC accept error");
                }
            }
        }
    });

    cmd_rx
}

fn handle_connection(
    stream: std::os::unix::net::UnixStream,
    cmd_tx: mpsc::Sender<IpcCommand>,
    event_bus: Arc<EventBus>,
) {
    let reader = BufReader::new(&stream);
    let mut writer = &stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::err(format!("invalid JSON: {}", e));
                let _ = write_response(&mut writer, &resp);
                continue;
            }
        };

        // Handle subscribe: keep connection open, stream events
        if request.command == "subscribe" {
            handle_subscribe(&stream, &request.args, &event_bus);
            return; // Connection dedicated to streaming
        }

        // Send to main thread and wait for response
        let (resp_tx, resp_rx) = mpsc::channel();
        let cmd = IpcCommand {
            request,
            response_tx: resp_tx,
        };

        if cmd_tx.send(cmd).is_err() {
            let resp = Response::err("daemon shutting down");
            let _ = write_response(&mut writer, &resp);
            break;
        }

        // Wait for response from main thread (with timeout)
        match resp_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(resp) => {
                let _ = write_response(&mut writer, &resp);
            }
            Err(_) => {
                let resp = Response::err("command timed out");
                let _ = write_response(&mut writer, &resp);
            }
        }
    }
}

fn handle_subscribe(
    stream: &std::os::unix::net::UnixStream,
    filters: &[String],
    event_bus: &EventBus,
) {
    let mut writer = stream;
    let rx = event_bus.subscribe();
    let all = filters.is_empty() || filters.iter().any(|f| f == "*");

    // Acknowledge subscription
    let _ = write_response(&mut writer, &Response::ok_empty());

    tracing::info!(filters = ?filters, "IPC client subscribed to events");

    // Stream events until client disconnects
    while let Ok(event) = rx.recv() {
        if !all && !filters.iter().any(|f| f == event.event_type()) {
            continue;
        }
        let Ok(json) = serde_json::to_string(&event) else {
            continue;
        };
        if writer.write_all(json.as_bytes()).is_err()
            || writer.write_all(b"\n").is_err()
            || writer.flush().is_err()
        {
            break;
        }
    }
    tracing::debug!("IPC subscriber disconnected");
}

fn write_response(writer: &mut impl Write, response: &Response) -> std::io::Result<()> {
    let json = serde_json::to_string(response)?;
    writer.write_all(json.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
