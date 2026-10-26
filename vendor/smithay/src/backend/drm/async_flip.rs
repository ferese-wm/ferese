//! Retry a rejected asynchronous request before advancing frame ownership.
use std::io::{self, ErrorKind};

pub(super) fn submit(allow_tearing: bool, mut operation: impl FnMut(bool) -> io::Result<()>) -> io::Result<()> {
    let result = operation(allow_tearing);
    if allow_tearing
        && matches!(&result, Err(error) if matches!(error.kind(), ErrorKind::InvalidInput | ErrorKind::Unsupported))
    {
        return operation(false);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_async_retries_once_without_tearing_before_ownership_changes() {
        for error in [ErrorKind::InvalidInput, ErrorKind::Unsupported] {
            let mut calls = Vec::new();
            submit(true, |asynchronous| {
                calls.push(asynchronous);
                if asynchronous { Err(error.into()) } else { Ok(()) }
            })
            .unwrap();
            assert_eq!(calls, [true, false]);
        }
    }

    #[test]
    fn success_permission_loss_and_ordinary_failures_never_duplicate_submission() {
        for requested in [false, true] {
            for error in [None, Some(ErrorKind::PermissionDenied), Some(ErrorKind::ResourceBusy), Some(ErrorKind::NotFound)] {
                let mut calls = Vec::new();
                let result = submit(requested, |asynchronous| {
                    calls.push(asynchronous);
                    error.map_or(Ok(()), |error| Err(error.into()))
                });
                assert_eq!(result.is_err(), error.is_some());
                assert_eq!(calls, [requested]);
            }
        }
    }
}
