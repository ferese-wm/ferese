use super::*;
use ferese_ipc::{Request, Response, read_frame, write_frame};
use serde_json::json;
use std::io::{Read, Write};

fn pair() -> (Bridge, UnixStream) {
    pair_with_timeout(Duration::from_secs(1))
}

fn pair_with_timeout(timeout: Duration) -> (Bridge, UnixStream) {
    let (client, server) = UnixStream::pair().unwrap();
    server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    (Bridge::from_stream(client, timeout).unwrap(), server)
}

#[tokio::test]
async fn valid_errors_preserve_connection_and_request_ids_are_unique() {
    let (bridge, mut server) = pair();
    let peer = std::thread::spawn(move || {
        let first: Request = read_frame(&mut server).unwrap();
        write_frame(&mut server, &Response::error(first.id, "denied", "Denied")).unwrap();
        let second: Request = read_frame(&mut server).unwrap();
        assert_eq!(second.id, first.id + 1);
        write_frame(&mut server, &Response::success(second.id, json!({"enabled":false}))).unwrap();
        let third: Request = read_frame(&mut server).unwrap();
        assert_eq!(third.id, second.id + 1);
        write_frame(&mut server, &Response::success(third.id, Json::Null)).unwrap();
    });
    assert_eq!(bridge.call("enable", json!({})).await.unwrap_err(), "Denied");
    assert!(!bridge.is_closed());
    assert_eq!(
        bridge.call("disable", json!({})).await.unwrap(),
        json!({"enabled":false})
    );
    assert!(!bridge.is_closed());
    assert_eq!(bridge.call("empty-result", json!({})).await.unwrap(), Json::Null);
    peer.join().unwrap();
}

#[tokio::test]
async fn timeout_closes_stream_before_a_late_reply_can_be_reused() {
    let (bridge, mut server) = pair_with_timeout(Duration::from_millis(40));
    let (release, blocked) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let request: Request = read_frame(&mut server).unwrap();
        blocked.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(write_frame(&mut server, &Response::success(request.id, json!({"enabled":true}))).is_err());
        assert_eq!(server.read(&mut [0u8; 1]).unwrap(), 0);
    });
    assert!(bridge.call("enable", json!({})).await.is_err());
    assert!(bridge.is_closed());
    tokio::time::timeout(Duration::from_secs(1), bridge.terminated())
        .await
        .unwrap();
    assert!(bridge.call("disable", json!({})).await.unwrap_err().contains("closed"));
    release.send(()).unwrap();
    peer.join().unwrap();
}

#[tokio::test]
async fn partial_reply_timeout_cannot_corrupt_the_next_frame() {
    let (bridge, mut server) = pair_with_timeout(Duration::from_millis(40));
    let (release, blocked) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let _: Request = read_frame(&mut server).unwrap();
        server.write_all(&20u32.to_be_bytes()).unwrap();
        server.write_all(b"{\"id\":").unwrap();
        blocked.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(server.read(&mut [0u8; 1]).unwrap(), 0);
    });
    assert!(bridge.call("enable", json!({})).await.is_err());
    assert!(bridge.is_closed());
    assert!(bridge.call("disable", json!({})).await.is_err());
    release.send(()).unwrap();
    peer.join().unwrap();
}

#[tokio::test]
async fn malformed_or_mismatched_replies_poison_the_bridge() {
    for case in 0..6 {
        let (bridge, mut server) = pair();
        let peer = std::thread::spawn(move || {
            let request: Request = read_frame(&mut server).unwrap();
            let mut response = Response::success(request.id, json!({}));
            match case {
                0 => response.id += 1,
                1 => response.version += 1,
                2 => response.result = None,
                3 => response.error = Response::error(request.id, "failed", "Failed").error,
                4 => {
                    server.write_all(&1u32.to_be_bytes()).unwrap();
                    server.write_all(b"{").unwrap();
                    return;
                }
                _ => {
                    server.write_all(&u32::MAX.to_be_bytes()).unwrap();
                    return;
                }
            }
            write_frame(&mut server, &response).unwrap();
        });
        assert!(bridge.call("enable", json!({})).await.is_err(), "case {case}");
        assert!(bridge.is_closed(), "case {case}");
        assert!(bridge.call("disable", json!({})).await.is_err());
        peer.join().unwrap();
    }
}

#[tokio::test]
async fn closing_any_clone_interrupts_a_blocked_watch() {
    let (bridge, mut server) = pair();
    let (started, received) = tokio::sync::oneshot::channel();
    let peer = std::thread::spawn(move || {
        let request: Request = read_frame(&mut server).unwrap();
        assert_eq!(request.command, "input-capture-watch");
        started.send(()).unwrap();
        assert_eq!(server.read(&mut [0u8; 1]).unwrap(), 0);
    });
    let watcher = bridge.clone();
    let task = tokio::spawn(async move { watcher.capture_watch(7).await });
    received.await.unwrap();
    bridge.close();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(bridge.call("enable", json!({})).await.is_err());
    peer.join().unwrap();
}

#[tokio::test]
async fn peer_watch_keeps_descriptor_flags_and_response_bytes_untouched() {
    use std::os::fd::AsRawFd;

    let (bridge, mut server) = pair();
    let flags = unsafe { libc::fcntl(bridge.0.shutdown.as_raw_fd(), libc::F_GETFL) };
    server.write_all(b"x").unwrap();
    tokio::time::timeout(Duration::from_secs(1), bridge.closed())
        .await
        .unwrap();
    assert_eq!(
        unsafe { libc::fcntl(bridge.0.shutdown.as_raw_fd(), libc::F_GETFL) },
        flags
    );
    let mut byte = [0u8; 1];
    bridge
        .0
        .connection
        .lock()
        .unwrap()
        .stream
        .read_exact(&mut byte)
        .unwrap();
    assert_eq!(byte, *b"x");
}
