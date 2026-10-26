use super::*;
use ferese_ipc::events::{Change, Event};
use serde_json::json;
use std::io::{self, Cursor};

struct MemoryTransport {
    reply: Cursor<Vec<u8>>,
    sent: Vec<u8>,
}

impl Read for MemoryTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reply.read(bytes)
    }
}

impl Write for MemoryTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sent.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn generic_requests_preserve_null_and_validate_response_identity_and_outcome() {
    for case in 0..7 {
        let mut response = Response::success(1, Value::Null);
        match case {
            0 => (),
            1 => response.result = Some(json!({})),
            2 => response = Response::error(1, "denied", "Denied"),
            3 => response.id += 1,
            4 => response.version += 1,
            5 => response.result = None,
            _ => response.error = Response::error(1, "failed", "Failed").error,
        }
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).unwrap();
        let mut stream = MemoryTransport {
            reply: Cursor::new(bytes),
            sent: Vec::new(),
        };
        let result = request(&mut stream, "get-focused-window", json!({}));
        assert_eq!(result.is_ok(), case < 2, "case {case}");
        if case == 0 {
            assert_eq!(result.unwrap(), Value::Null);
        } else if case == 2 {
            assert_eq!(result.unwrap_err().to_string(), "denied: Denied");
        }
        let sent: Request = read_frame(&mut stream.sent.as_slice()).unwrap();
        assert_eq!(sent.command, "get-focused-window");
        assert_eq!((sent.id, sent.version), (1, VERSION));
    }
}

#[test]
fn socket_requests_expire_silent_and_trickled_replies() {
    for trickle in [false, true] {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
        let peer = std::thread::spawn(move || {
            let request: Request = read_frame(&mut server).unwrap();
            if !trickle {
                assert_eq!(server.read(&mut [0]).unwrap(), 0);
                return;
            }
            let mut frame = Vec::new();
            write_frame(&mut frame, &Response::success(request.id, json!({}))).unwrap();
            for byte in frame {
                if server.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        assert!(
            request_socket_with_budget(
                &client,
                "get-outputs",
                json!({}),
                Duration::from_millis(90),
            )
            .is_err(),
            "trickle={trickle}"
        );
        drop(client);
        peer.join().unwrap();
    }
}

#[test]
fn socket_request_accepts_a_legitimate_null_result() {
    let (client, mut server) = UnixStream::pair().unwrap();
    server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
    let peer = std::thread::spawn(move || {
        let request: Request = read_frame(&mut server).unwrap();
        write_frame(&mut server, &Response::success(request.id, Value::Null)).unwrap();
    });
    assert_eq!(
        request_socket(&client, "get-focused-window", json!({})).unwrap(),
        Value::Null
    );
    peer.join().unwrap();
}

#[test]
fn screenshots_allow_a_reply_after_the_ordinary_query_budget() {
    let (client, mut server) = UnixStream::pair().unwrap();
    server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
    let peer = std::thread::spawn(move || {
        let request: Request = read_frame(&mut server).unwrap();
        std::thread::sleep(FRAME_TIMEOUT + Duration::from_millis(250));
        write_frame(
            &mut server,
            &Response::success(request.id, json!({"path":"/tmp/capture.png"})),
        )
        .unwrap();
    });
    assert_eq!(
        request_socket(&client, "screenshot", json!({})).unwrap(),
        json!({"path":"/tmp/capture.png"})
    );
    peer.join().unwrap();
}

#[test]
fn event_subscription_may_idle_but_a_started_event_must_finish() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let budget = Duration::from_millis(90);
    let peer = std::thread::spawn(move || {
        let mut event = Event {
            version: ferese_ipc::events::VERSION,
            generation: 1,
            last: true,
            change: Change::FocusChanged { focus: Value::Null },
        };
        std::thread::sleep(budget * 3);
        write_frame(&mut server, &event).unwrap();
        event.generation += 1;
        let mut frame = Vec::new();
        write_frame(&mut frame, &event).unwrap();
        for byte in frame {
            if server.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    assert_eq!(
        next_event_with_budget(&client, budget).unwrap().generation,
        1
    );
    assert_eq!(client.read_timeout().unwrap(), None);
    assert!(next_event_with_budget(&client, budget).is_err());
    assert_eq!(client.read_timeout().unwrap(), None);
    drop(client);
    peer.join().unwrap();
}
