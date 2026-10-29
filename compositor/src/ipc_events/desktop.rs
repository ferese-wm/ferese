//! Retain desktop payloads while their borrowed logical fields are unchanged.
use std::sync::Arc;

use ferese_core::WorkspaceView;
use ferese_ipc::events::Window;
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowView<'window> {
    pub id: u64,
    pub app_id: &'window str,
    pub title: &'window str,
    pub focused: bool,
    pub workspace: u64,
    pub floating: bool,
    pub fullscreen: bool,
}

impl<'window> From<&'window Window> for WindowView<'window> {
    fn from(window: &'window Window) -> Self {
        Self {
            id: window.id,
            app_id: &window.app_id,
            title: &window.title,
            focused: window.focused,
            workspace: window.workspace,
            floating: window.floating,
            fullscreen: window.fullscreen,
        }
    }
}

impl WindowView<'_> {
    fn owned(self) -> Window {
        Window {
            id: self.id,
            app_id: self.app_id.to_owned(),
            title: self.title.to_owned(),
            focused: self.focused,
            workspace: self.workspace,
            floating: self.floating,
            fullscreen: self.fullscreen,
        }
    }
}

#[derive(Default)]
pub(crate) struct Windows {
    value: Arc<Vec<Window>>,
}

impl Windows {
    pub(crate) fn get_or_update<'window, I: Iterator<Item = WindowView<'window>>>(
        &mut self,
        windows: impl Fn() -> I,
    ) -> Arc<Vec<Window>> {
        let mut current = windows();
        let unchanged = self.value.iter().all(|previous| {
            current
                .next()
                .is_some_and(|window| window == WindowView::from(previous))
        }) && current.next().is_none();
        if !unchanged {
            self.value = Arc::new(windows().map(WindowView::owned).collect());
        }
        self.value.clone()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub view: WorkspaceView,
    pub layout: &'static str,
    pub focused_window: Option<u64>,
    pub fullscreen_window: Option<u64>,
}

impl Workspace {
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.view.id.0,
            "name": self.view.index.to_string(),
            "index": self.view.index,
            "output": self.view.output.0,
            "window_count": self.view.window_count,
            "visible": self.view.visible,
            "focused": self.view.focused,
            "active": self.view.focused,
            "layout": self.layout,
            "focused_window": self.focused_window,
            "fullscreen_window": self.fullscreen_window,
        })
    }
}

#[derive(Default)]
pub(crate) struct Workspaces {
    views: Vec<Workspace>,
    value: Option<Arc<Value>>,
}

impl Workspaces {
    pub(crate) fn get_or_update<I: Iterator<Item = Workspace>>(
        &mut self,
        workspaces: impl Fn() -> I,
    ) -> Arc<Value> {
        if self.value.is_none() || !workspaces().eq(self.views.iter().cloned()) {
            self.views = workspaces().collect();
            self.value = Some(Arc::new(Value::Array(
                self.views.iter().map(Workspace::json).collect(),
            )));
        }
        self.value
            .as_ref()
            .expect("workspace payload was populated")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferese_core::{OutputId, WorkspaceId};

    fn window() -> Window {
        Window {
            id: 1,
            app_id: "editor".into(),
            title: "Document".into(),
            focused: false,
            workspace: 1,
            floating: false,
            fullscreen: false,
        }
    }

    #[test]
    fn windows_reuse_storage_but_keep_every_change_and_previous_snapshot() {
        let mut cache = Windows::default();
        let initial = window();
        let before = cache.get_or_update(|| std::iter::once(WindowView::from(&initial)));
        for _ in 0..100 {
            let next = cache.get_or_update(|| std::iter::once(WindowView::from(&initial)));
            assert!(Arc::ptr_eq(&before, &next));
        }
        for change in [
            |w: &mut Window| w.id += 1,
            |w: &mut Window| w.app_id.push_str("-new"),
            |w: &mut Window| w.title.push_str(" — changed"),
            |w: &mut Window| w.focused = true,
            |w: &mut Window| w.workspace += 1,
            |w: &mut Window| w.floating = true,
            |w: &mut Window| w.fullscreen = true,
        ] {
            let unchanged = cache.get_or_update(|| std::iter::once(WindowView::from(&initial)));
            let mut changed = initial.clone();
            change(&mut changed);
            let next = cache.get_or_update(|| std::iter::once(WindowView::from(&changed)));
            assert!(!Arc::ptr_eq(&unchanged, &next));
            assert_eq!(*next, [changed]);
            assert_eq!(before.as_slice(), std::slice::from_ref(&initial));
        }
        let mut second = initial.clone();
        second.id = 2;
        let both = cache.get_or_update(|| [&initial, &second].into_iter().map(WindowView::from));
        let reordered =
            cache.get_or_update(|| [&second, &initial].into_iter().map(WindowView::from));
        assert!(!Arc::ptr_eq(&both, &reordered));
        assert_eq!(reordered[0].id, 2);
        let hidden = cache.get_or_update(std::iter::empty);
        assert!(hidden.is_empty());
        assert_eq!(both.len(), 2);
        let restored =
            cache.get_or_update(|| [&initial, &second].into_iter().map(WindowView::from));
        assert_eq!(restored, both);
    }

    fn workspace() -> Workspace {
        Workspace {
            view: WorkspaceView {
                id: WorkspaceId(1),
                output: OutputId(1),
                index: 1,
                window_count: 2,
                visible: true,
                focused: true,
            },
            layout: "scrolling",
            focused_window: Some(1),
            fullscreen_window: None,
        }
    }

    #[test]
    fn workspace_cache_covers_layout_ownership_order_membership_focus_and_fullscreen() {
        let mut cache = Workspaces::default();
        let initial = workspace();
        let before = cache.get_or_update(|| std::iter::once(initial.clone()));
        assert_eq!(
            *before,
            json!([{
                "id": 1, "name": "1", "index": 1, "output": 1, "window_count": 2,
                "visible": true, "focused": true, "active": true, "layout": "scrolling",
                "focused_window": 1, "fullscreen_window": null
            }])
        );
        for _ in 0..100 {
            assert!(Arc::ptr_eq(
                &before,
                &cache.get_or_update(|| std::iter::once(initial.clone()))
            ));
        }
        for change in [
            |w: &mut Workspace| w.view.id = WorkspaceId(2),
            |w: &mut Workspace| w.view.output = OutputId(2),
            |w: &mut Workspace| w.view.index = 2,
            |w: &mut Workspace| w.view.window_count = 3,
            |w: &mut Workspace| w.view.visible = false,
            |w: &mut Workspace| w.view.focused = false,
            |w: &mut Workspace| w.layout = "tree",
            |w: &mut Workspace| w.focused_window = Some(2),
            |w: &mut Workspace| w.fullscreen_window = Some(1),
        ] {
            let unchanged = cache.get_or_update(|| std::iter::once(initial.clone()));
            let mut changed = initial.clone();
            change(&mut changed);
            let next = cache.get_or_update(|| std::iter::once(changed.clone()));
            assert!(!Arc::ptr_eq(&unchanged, &next));
            assert_ne!(before, next);
        }
        let hidden = cache.get_or_update(std::iter::empty);
        assert_eq!(*hidden, json!([]));
        assert_eq!(before[0]["window_count"], 2);
        assert_eq!(
            cache.get_or_update(|| std::iter::once(initial.clone())),
            before
        );
    }
}
