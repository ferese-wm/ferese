//! Native recording client. The portal retains capture authorization; encoding
//! and file I/O live in this separate process, never in the shell render loop.
mod recorder;
mod recording;

fn main() {
    if let Err(error) = tokio::runtime::Runtime::new()
        .and_then(|runtime| runtime.block_on(recorder::run()).map_err(std::io::Error::other))
    {
        recorder::event("error", Some(&error.to_string()), None);
        std::process::exit(1);
    }
}
