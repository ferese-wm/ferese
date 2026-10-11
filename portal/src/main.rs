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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [flag, name, cursor] if flag == "--stream" => stream::run(name.clone(), cursor == "embedded", None),
        [flag, id, cursor] if flag == "--stream-window" => {
            let id: u64 = id.parse()?;
            stream::run(format!("window:{id}"), cursor == "embedded", None)
        }
        [flag, name, cursor, generation] if flag == "--stream" => {
            stream::run(name.clone(), cursor == "embedded", Some(generation.parse()?))
        }
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
