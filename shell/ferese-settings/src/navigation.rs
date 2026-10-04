use cosmic::iced::advanced::widget::Id;
use cosmic::iced::advanced::widget::operation::{Operation, Scrollable};
use cosmic::iced::{Rectangle, Vector};

pub(super) fn gallery_list(appearance: Option<ferese_config::theme::Appearance>, custom: bool) -> &'static str {
    match (appearance, custom) {
        (Some(ferese_config::theme::Appearance::Light), false) => "theme-light",
        (Some(ferese_config::theme::Appearance::Dark), false) => "theme-dark",
        (None, false) => "theme",
        (Some(ferese_config::theme::Appearance::Light), true) => "theme-custom-light",
        (Some(ferese_config::theme::Appearance::Dark), true) => "theme-custom-dark",
        (None, true) => "theme-custom",
    }
}

pub(super) fn gallery_row_id(list: &str, row: usize) -> Id {
    Id::new(format!("{list}-row-{row}"))
}

pub(super) struct RevealRow {
    target: Id,
    bounds: Option<Rectangle>,
}

impl RevealRow {
    pub(super) fn new(list: &str, row: usize) -> Self {
        Self {
            target: gallery_row_id(list, row),
            bounds: None,
        }
    }
}

impl<M> Operation<M> for RevealRow {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<M>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.bounds = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        viewport: Rectangle,
        _: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        if id != Some(&Id::new("settings-content")) {
            return;
        }

        if let Some(target) = self.bounds {
            let visible = target.y - translation.y;
            let delta = if visible < viewport.y {
                visible - viewport.y
            } else if visible + target.height > viewport.y + viewport.height {
                visible + target.height - viewport.y - viewport.height
            } else {
                return;
            };
            state.scroll_to(cosmic::iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some((translation.y + delta).max(0.)),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::widget::operation::scrollable::{AbsoluteOffset, RelativeOffset};

    #[derive(Default)]
    struct Scroll(Option<AbsoluteOffset<Option<f32>>>);

    impl Scrollable for Scroll {
        fn snap_to(&mut self, _: RelativeOffset<Option<f32>>) {
            panic!("not a snap");
        }
        fn scroll_to(&mut self, offset: AbsoluteOffset<Option<f32>>) {
            self.0 = Some(offset);
        }
        fn scroll_by(&mut self, _: AbsoluteOffset, _: Rectangle, _: Rectangle) {
            panic!("not a relative scroll");
        }
    }

    #[test]
    fn wrapping_and_home_end_reveal_only_as_much_as_needed() {
        let viewport = Rectangle {
            x: 0.,
            y: 40.,
            width: 600.,
            height: 500.,
        };
        for (top, height, offset, expected) in [
            (1000., 200., 0., Some(660.)),
            (40., 200., 500., Some(0.)),
            (100., 200., 0., None),
        ] {
            let mut operation = RevealRow::new("theme", 10);
            let target = operation.target.clone();
            Operation::<()>::container(
                &mut operation,
                Some(&target),
                Rectangle {
                    y: top,
                    height,
                    ..viewport
                },
            );
            let mut state = Scroll::default();
            Operation::<()>::scrollable(
                &mut operation,
                Some(&Id::new("settings-content")),
                viewport,
                viewport,
                Vector::new(0., offset),
                &mut state,
            );
            assert_eq!(state.0.and_then(|offset| offset.y), expected);
        }
    }
}
