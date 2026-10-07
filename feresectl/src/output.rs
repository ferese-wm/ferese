use std::error::Error;
use std::io::{self, Write};

use serde_json::Value;

pub fn print(value: &Value, json: bool) -> Result<(), Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    if json {
        writeln!(stdout, "{}", serde_json::to_string_pretty(value)?)?;
    } else {
        write_plain(&mut stdout, value, 0)?;
    }
    stdout.flush()?;
    Ok(())
}

fn write_plain(writer: &mut impl Write, value: &Value, indent: usize) -> io::Result<()> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                write!(writer, "{:indent$}{key}:", "")?;
                if value.is_object() || value.is_array() {
                    writeln!(writer)?;
                    write_plain(writer, value, indent + 2)?;
                } else {
                    writeln!(writer, " {}", scalar(value))?;
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 && item.is_object() {
                    writeln!(writer)?;
                }
                write_plain(writer, item, indent)?;
            }
        }
        _ => writeln!(writer, "{:indent$}{}", "", scalar(value))?,
    }
    Ok(())
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "none".into(),
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

/// Return false when a downstream consumer closes stdout.
pub fn write_json_line(writer: &mut impl Write, value: &impl serde::Serialize) -> Result<bool, Box<dyn Error>> {
    let line = serde_json::to_vec(value)?;
    match writer
        .write_all(&line)
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
    {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_output_has_readable_fields_without_json_quotes() {
        let mut bytes = Vec::new();
        write_plain(
            &mut bytes,
            &serde_json::json!([{"id": 7, "title": "Terminal", "focused": true}]),
            0,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "focused: true\nid: 7\ntitle: Terminal\n"
        );
    }
    #[test]
    fn event_output_is_one_json_value_per_line() {
        let mut bytes = Vec::new();
        for value in [
            serde_json::json!({"title": "a\nb"}),
            serde_json::json!({"locked": true}),
        ] {
            assert!(write_json_line(&mut bytes, &value).unwrap());
        }
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }
}
