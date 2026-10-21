use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
static NEXT_TOKEN: AtomicU32 = AtomicU32::new(1);
pub(super) struct Thumbnails {
    token: u32,
    pub images: HashMap<u64, image::Handle>,
    pending: HashSet<u64>,
    updated: Option<Instant>,
    visible: [Option<u64>; 3],
}
impl Default for Thumbnails {
    fn default() -> Self {
        Self {
            token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            images: HashMap::new(),
            pending: HashSet::new(),
            updated: None,
            visible: [None; 3],
        }
    }
}
impl Thumbnails {
    pub fn refresh(&mut self, rows: &[Row], selected: usize, control: &ShellControl, now: Instant) {
        if self
            .updated
            .is_some_and(|updated| now.duration_since(updated) < Duration::from_millis(500))
        {
            return;
        }
        self.visible = std::array::from_fn(|offset| {
            rows.get(selected.saturating_sub(2) + offset)
                .and_then(|row| match row.target {
                    Target::Window(id) => Some(id),
                    _ => None,
                })
        });
        self.images
            .retain(|id, _| self.visible.contains(&Some(*id)));
        for id in self.visible.into_iter().flatten() {
            if self.pending.len() < 3 && self.pending.insert(id) {
                control.thumbnail(self.token, id);
            }
        }
        self.updated = Some(now);
    }
}
impl FereseShell {
    pub(crate) fn launcher_thumbnail(
        &mut self,
        token: u32,
        window: u64,
        image: Option<image::Handle>,
    ) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.closing
            || launcher.thumbnails.token != token
            || !launcher.thumbnails.pending.remove(&window)
        {
            return;
        }
        if let Some(image) = image {
            launcher.thumbnails.images.insert(window, image);
        } else {
            launcher.thumbnails.images.remove(&window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_replies_must_belong_to_the_open_invocation() {
        let mut shell = crate::tests::shell_with_measured_panel();
        let _ = shell.toggle_launcher("test".into());
        let launcher = shell.launcher.as_mut().unwrap();
        let token = launcher.thumbnails.token;
        launcher.thumbnails.pending.insert(1);
        let image = image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]);
        shell.launcher_thumbnail(token.wrapping_add(1), 1, Some(image.clone()));
        assert!(
            shell
                .launcher
                .as_ref()
                .unwrap()
                .thumbnails
                .images
                .is_empty()
        );
        shell.launcher_thumbnail(token, 1, Some(image.clone()));
        assert_eq!(shell.launcher.as_ref().unwrap().thumbnails.images.len(), 1);
        let _ = shell.destroy_launcher();
        let _ = shell.toggle_launcher("test".into());
        shell.launcher_thumbnail(token, 1, Some(image));
        assert!(
            shell
                .launcher
                .as_ref()
                .unwrap()
                .thumbnails
                .images
                .is_empty()
        );
    }
}
