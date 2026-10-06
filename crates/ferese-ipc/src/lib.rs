use std::error::Error;
use std::io::{Read, Write};
use std::{fmt, io};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod media;
pub mod theme;
pub mod xwayland;

pub const VERSION: u32 = 1;
pub const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResponseError {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            version: VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: u64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            id,
            result: None,
            error: Some(ResponseError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Oversized(usize),
    InvalidJson(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Oversized(size) => write!(formatter, "IPC payload is {size} bytes; maximum is {MAX_PAYLOAD_SIZE}"),
            Self::InvalidJson(error) => write!(formatter, "invalid IPC JSON: {error}"),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidJson(error) => Some(error),
            Self::Oversized(_) => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> Result<T, FrameError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size > MAX_PAYLOAD_SIZE {
        return Err(FrameError::Oversized(size));
    }

    let mut payload = vec![0; size];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(FrameError::InvalidJson)
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value).map_err(FrameError::InvalidJson)?;
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(FrameError::Oversized(payload.len()));
    }

    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct ChunkedReader<R> {
        inner: R,
        chunk_size: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let limit = buffer.len().min(self.chunk_size);
            self.inner.read(&mut buffer[..limit])
        }
    }

    fn request(id: u64, command: &str) -> Request {
        Request {
            version: VERSION,
            id,
            kind: "command".to_owned(),
            command: command.to_owned(),
            args: serde_json::json!({}),
        }
    }

    #[test]
    fn framed_json_round_trips() {
        let mut request = request(42, "focus");
        request.args = serde_json::json!({ "direction": "left" });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).unwrap();
        let decoded: Request = read_frame(&mut bytes.as_slice()).unwrap();

        assert_eq!(decoded.version, VERSION);
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.command, "focus");
        assert_eq!(decoded.args["direction"], "left");
    }

    #[test]
    fn reads_fragmented_frames_and_preserves_frame_boundaries() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request(1, "get-workspaces")).unwrap();
        write_frame(&mut bytes, &request(2, "get-outputs")).unwrap();
        let mut reader = ChunkedReader {
            inner: Cursor::new(bytes),
            chunk_size: 3,
        };

        let first: Request = read_frame(&mut reader).unwrap();
        let second: Request = read_frame(&mut reader).unwrap();

        assert_eq!((first.id, first.command.as_str()), (1, "get-workspaces"));
        assert_eq!((second.id, second.command.as_str()), (2, "get-outputs"));
    }

    #[test]
    fn rejects_invalid_and_truncated_payloads_without_panicking() {
        let invalid = b"not-json";
        let mut invalid_frame = Vec::from((invalid.len() as u32).to_be_bytes());
        invalid_frame.extend_from_slice(invalid);
        assert!(matches!(
            read_frame::<Request>(&mut invalid_frame.as_slice()),
            Err(FrameError::InvalidJson(_))
        ));

        let mut truncated = Vec::from(32_u32.to_be_bytes());
        truncated.extend_from_slice(b"short");
        assert!(matches!(
            read_frame::<Request>(&mut truncated.as_slice()),
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn rejects_oversized_frames_before_allocating_payload() {
        let size = (MAX_PAYLOAD_SIZE as u32 + 1).to_be_bytes();
        let error = read_frame::<Request>(&mut size.as_slice()).unwrap_err();

        assert!(matches!(error, FrameError::Oversized(_)));
    }
}
