use std::time::Instant;

use ferese_panel::morph::Mode;

use super::{Peek, PreviousContent, Runtime, content};
use crate::status_ui::Menu;

/// A transient owns its expiry token and the content it interrupted.
#[derive(Clone, Copy, Default)]
pub(super) enum Transient {
    #[default]
    None,
    Toast {
        notice: u32,
        token: Instant,
    },
    Workspaces {
        token: Instant,
        resume: (Peek, f64),
    },
}

impl Transient {
    pub(super) fn toast(self) -> Option<u32> {
        match self {
            Self::Toast { notice, .. } => Some(notice),
            _ => None,
        }
    }

    pub(super) fn workspace_token(self) -> Option<Instant> {
        match self {
            Self::Workspaces { token, .. } => Some(token),
            _ => None,
        }
    }
}

pub(super) enum Event {
    Hover {
        entered: bool,
        enabled: bool,
        peek: (Peek, f64),
        priority: Option<Peek>,
    },
    Open(Menu),
    Close,
    Activity {
        priority: Option<Peek>,
        peek: (Peek, f64),
    },
    Toast {
        notice: u32,
        token: Instant,
    },
    ToastExpired(Instant),
    Workspaces(Instant),
    WorkspacesExpired(Instant),
    /// Recompute the allocation after a size change without changing content.
    Resize,
    #[cfg(test)]
    Present(Mode, Option<Menu>),
}

impl Runtime {
    fn restore_transient(&mut self) {
        if let Transient::Workspaces { resume, .. } = self.transient {
            (self.peek, self.peek_width) = resume;
        }
        self.transient = Transient::None;
    }

    /// All semantic state changes enter here. Retargeting below handles only geometry.
    pub(super) fn transition(&mut self, event: Event, available: [f64; 2]) -> bool {
        let before = self.sample(Instant::now());
        let outgoing = PreviousContent {
            menu: self.menu,
            peek: self.peek,
            toast: self.transient.toast(),
            size: self.hub.target_size(),
        };
        let mut replace_content = false;
        let current = self.hub.mode();
        let (mode, menu) = match event {
            Event::Hover {
                entered,
                enabled,
                peek,
                priority,
            } => {
                self.hovered = entered;
                match current {
                    Mode::Idle if entered && enabled => {
                        (self.peek, self.peek_width) = peek;
                        self.priority = priority;
                        (Mode::Peek, None)
                    }
                    Mode::Peek if !entered => {
                        self.restore_transient();
                        (Mode::Idle, None)
                    }
                    _ => return false,
                }
            }
            Event::Open(menu) => {
                self.restore_transient();
                self.last_menu = menu;
                self.workspace_hover = None;
                self.workspace_selected = None;
                (Mode::Expanded(content(menu)), Some(menu))
            }
            Event::Close => {
                self.restore_transient();
                (Mode::Idle, None)
            }
            Event::Activity { priority, peek } => {
                let changed = self.priority != priority;
                let resized =
                    priority.is_some() && current == Mode::Peek && self.peek_width != peek.1;
                self.priority = priority;
                if (!changed && !resized) || current.expanded() {
                    return false;
                }
                if current != Mode::Peek && !(current == Mode::Toast && priority.is_some()) {
                    return false;
                }
                // Preserve the outgoing content for its fade, but discard its expiry.
                self.transient = Transient::None;
                (self.peek, self.peek_width) = peek;
                if changed && current == Mode::Peek {
                    replace_content = true;
                }
                (if self.hovered { Mode::Peek } else { Mode::Idle }, None)
            }
            Event::Toast { notice, token } => {
                if current.expanded() || self.priority.is_some() {
                    return false;
                }
                self.restore_transient();
                self.transient = Transient::Toast { notice, token };
                (Mode::Toast, None)
            }
            Event::ToastExpired(token) => {
                if !matches!(self.transient, Transient::Toast { token: live, .. } if live == token)
                {
                    return false;
                }
                self.restore_transient();
                (Mode::Idle, None)
            }
            Event::Workspaces(token) => {
                if !matches!(current, Mode::Idle | Mode::Peek) || self.priority.is_some() {
                    return false;
                }
                let resume = match self.transient {
                    Transient::Workspaces { resume, .. } => resume,
                    _ => {
                        let resume = (self.peek, self.peek_width);
                        self.peek = Peek::Workspaces;
                        self.peek_width = 260.;
                        if current == Mode::Peek {
                            replace_content = true;
                        }
                        resume
                    }
                };
                self.transient = Transient::Workspaces { token, resume };
                (Mode::Peek, None)
            }
            Event::WorkspacesExpired(token) => {
                if self.transient.workspace_token() != Some(token) {
                    return false;
                }
                self.restore_transient();
                if self.hovered {
                    replace_content = true;
                }
                (if self.hovered { Mode::Peek } else { Mode::Idle }, None)
            }
            Event::Resize => (current, self.menu),
            #[cfg(test)]
            Event::Present(mode, menu) => {
                self.restore_transient();
                (mode, menu)
            }
        };
        replace_content |= current == mode && self.menu != menu;
        let changing_content = current != mode || replace_content;
        if changing_content && (replace_content || before.opacity >= before.previous_opacity) {
            self.previous = outgoing;
        }
        self.retarget(mode, available, menu);
        if replace_content {
            self.hub.replace_content();
        }
        true
    }
}
