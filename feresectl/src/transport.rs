use std::error::Error;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ferese_ipc::{Request, Response, VERSION, read_frame, write_frame};
use serde_json::Value;

const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
// The screenshot coordinator has 30 seconds to complete or reject a capture.
// Leave room for its terminal response instead of applying the query budget.
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(35);

fn message(command: &str, args: Value) -> Request {
    Request {
        version: VERSION,
        id: 1,
        kind: "command".into(),
        command: command.into(),
        args,
    }
}

fn response_value(response: Response) -> Result<Value, Box<dyn Error>> {
    if response.version != VERSION || response.id != 1 {
        return Err("Unexpected IPC response".into());
    }
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => Err(format!("{}: {}", error.code, error.message).into()),
        _ => Err("Invalid IPC response outcome".into()),
    }
}

// Activity publishing supplies a Read+Write transport with its own deadline
// spanning connect, request and response. Keep its shorter budget intact.
pub(super) fn request(
    stream: &mut (impl Read + Write),
    command: &str,
    args: Value,
) -> Result<Value, Box<dyn Error>> {
    write_frame(stream, &message(command, args))?;
    response_value(read_frame(stream)?)
}

pub(super) fn request_socket(
    stream: &UnixStream,
    command: &str,
    args: Value,
) -> Result<Value, Box<dyn Error>> {
    let budget = if matches!(command, "screenshot" | "screenshot-window") {
        SCREENSHOT_TIMEOUT
    } else {
        FRAME_TIMEOUT
    };
    request_socket_with_budget(stream, command, args, budget)
}

fn request_socket_with_budget(
    stream: &UnixStream,
    command: &str,
    args: Value,
    budget: Duration,
) -> Result<Value, Box<dyn Error>> {
    stream.set_read_timeout(Some(budget))?;
    stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
    let response = match ferese_ipc::exchange_frames(stream, &message(command, args), budget) {
        Ok(response) => response,
        Err(error) => {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(error.into());
        }
    };
    response_value(response)
}

pub(super) fn next_event(stream: &UnixStream) -> Result<ferese_ipc::events::Event, Box<dyn Error>> {
    next_event_with_budget(stream, FRAME_TIMEOUT)
}

fn next_event_with_budget(
    stream: &UnixStream,
    budget: Duration,
) -> Result<ferese_ipc::events::Event, Box<dyn Error>> {
    let event: ferese_ipc::events::Event = match ferese_ipc::read_socket_frame(stream, budget) {
        Ok(event) => event,
        Err(error) => {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(error.into());
        }
    };
    event.validate()?;
    Ok(event)
}

#[cfg(test)]
mod tests;
