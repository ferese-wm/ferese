mod backend;
mod bridge;
mod capture;
mod consent;
mod desktop;
mod eis;
mod inhibit;
mod input_capture;
mod lockdown;
mod parent;
mod permissions;
mod picker;
mod recording;
mod recording_region;
mod restore;
mod settings;
mod shortcuts;
mod stream;
mod workspace;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [flag, name, cursor] if flag == "--stream" => stream::run(
            name.clone(),
            cursor == "embedded",
            None,
            false,
            workspace::Quality::default(),
        ),
        [flag, id, cursor, options @ ..] if flag == "--stream-workspace" => {
            let id: u64 = id.parse()?;
            let (include_shell, quality) = workspace_options(options)?;
            stream::run(
                format!("workspace:{id}"),
                cursor == "embedded",
                None,
                include_shell,
                quality,
            )
        }
        [flag, id, cursor] if flag == "--stream-window" => {
            let id: u64 = id.parse()?;
            stream::run(
                format!("window:{id}"),
                cursor == "embedded",
                None,
                false,
                workspace::Quality::default(),
            )
        }
        [flag, name, cursor, generation] if flag == "--stream" => stream::run(
            name.clone(),
            cursor == "embedded",
            Some(generation.parse()?),
            false,
            workspace::Quality::default(),
        ),
        [flag] if flag == "--sources" => {
            let capture = capture::Capture::connect(&std::sync::atomic::AtomicBool::new(false))?;
            println!("{}", serde_json::to_string(&capture.sources())?);
            Ok(())
        }
        [flag] if flag == "--picker" => picker::run(),
        [flag] if flag == "--consent" => consent::run(),
        [] => tokio::runtime::Runtime::new()?.block_on(backend::run()),
        _ => Err("Usage: xdg-desktop-portal-ferese [--sources]".into()),
    }
}

fn workspace_options(args: &[String]) -> Result<(bool, workspace::Quality), String> {
    let mut remaining = args;
    let mut include_shell = false;
    let mut quality = None;
    while !remaining.is_empty() {
        remaining = match remaining {
            [flag, rest @ ..] if flag == "--include-shell" && !include_shell => {
                include_shell = true;
                rest
            }
            [flag, value, rest @ ..] if flag == "--quality" && quality.is_none() => {
                quality = Some(value.parse()?);
                rest
            }
            _ => return Err("Invalid workspace capture options".into()),
        };
    }
    Ok((include_shell, quality.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_quality_and_shell_options_compose_without_ambiguous_defaults() {
        for (args, expected) in [
            (vec![], (false, workspace::Quality::Native)),
            (vec!["--include-shell"], (true, workspace::Quality::Native)),
            (
                vec!["--quality", "native", "--include-shell"],
                (true, workspace::Quality::Native),
            ),
            (
                vec!["--include-shell", "--quality", "high"],
                (true, workspace::Quality::High),
            ),
        ] {
            assert_eq!(
                workspace_options(&args.into_iter().map(String::from).collect::<Vec<_>>()).unwrap(),
                expected
            );
        }
        for args in [
            vec!["--quality"],
            vec!["--quality", "4k"],
            vec!["--include-shell", "--include-shell"],
            vec!["--quality", "native", "--quality", "balanced"],
        ] {
            assert!(
                workspace_options(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
    }
}
