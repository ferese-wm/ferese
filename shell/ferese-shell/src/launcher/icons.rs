use cosmic::widget::icon;
use std::path::Path;

pub(super) fn resolve(name: &str, fallback: &'static str) -> icon::Handle {
    if Path::new(name).is_absolute() {
        icon::from_path(name.into())
    } else {
        icon::from_name(name.to_owned())
            .prefer_svg(true)
            .size(32)
            .fallback(Some(icon::IconFallback::Names(vec![fallback.into()])))
            .handle()
    }
}

#[derive(Default)]
pub(super) struct FileIcons(std::collections::HashMap<String, icon::Handle>);
impl FileIcons {
    pub(super) fn file(&mut self, path: &Path) -> icon::Handle {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        if let Some(handle) = self.0.get(mime.essence_str()) {
            return handle.clone();
        }
        let name = mime.essence_str().replace('/', "-");
        let generic = format!("{}-x-generic", mime.type_());
        let handle = icon::from_name(name)
            .prefer_svg(true)
            .size(32)
            .fallback(Some(icon::IconFallback::Names(vec![
                generic.into(),
                "text-x-generic".into(),
            ])))
            .handle();
        // The cache belongs to one invocation and has a fixed upper bound.
        if self.0.len() < 64 {
            self.0.insert(mime.essence_str().into(), handle.clone());
        }
        handle
    }
}
