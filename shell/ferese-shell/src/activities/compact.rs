use std::collections::BTreeMap;

use cosmic::font::Font;
use cosmic::iced::advanced::text::{self, Paragraph};
use ferese_activities::{Activity, Id, Selection};

pub(super) const PADDING: f32 = 14.;
pub(super) const ICON: f32 = ferese_theme::panel::ICON_SLOT_WIDTH as f32;
pub(super) const TEXT_GAP: f32 = 6.;
pub(super) const ITEM_GAP: f32 = 8.;

#[derive(Default)]
pub(super) struct Compact {
    font: Option<Font>,
    titles: BTreeMap<Id, (String, f32)>,
    badge: Option<(usize, f32)>,
    text_scale: f32,
    pub width: f32,
}

impl Compact {
    pub fn update(
        &mut self,
        entries: &[&Activity],
        selection: &Selection,
        font: Font,
        available: f32,
    ) {
        if self.font != Some(font) {
            self.font = Some(font);
            self.titles.clear();
            self.badge = None;
        }
        self.titles.retain(|id, _| selection.visible.contains(id));
        let entry_iter = entries
            .iter()
            .filter(|entry| selection.visible.contains(&entry.id));
        for entry in entry_iter {
            let title = &entry.data.title;
            if self
                .titles
                .get(&entry.id)
                .is_none_or(|(cached, _)| cached != title)
            {
                self.titles.insert(
                    entry.id.clone(),
                    (
                        title.clone(),
                        text_width(title, f32::from(ferese_theme::panel::TEXT_SIZE), font)
                            .ceil()
                            .clamp(48., 100.),
                    ),
                );
            }
        }
        let hidden = selection.hidden;
        if self.badge.is_none_or(|(count, _)| count != hidden) {
            self.badge = Some((
                hidden,
                if hidden > 0 {
                    text_width(&format!("+{hidden}"), 11., font).ceil()
                } else {
                    0.
                },
            ));
        }
        let count = self.titles.len();
        let fixed = 2. * PADDING
            + count as f32 * (ICON + TEXT_GAP)
            + count.saturating_sub(1) as f32 * (2. * ITEM_GAP + 1.)
            + if hidden > 0 {
                ITEM_GAP
                    + self
                        .badge
                        .expect("hidden activity count is nonzero only when its badge was measured")
                        .1
            } else {
                0.
            };
        let text: f32 = self.titles.values().map(|(_, width)| width).sum();
        self.text_scale = if text > 0. {
            ((available - fixed).max(0.) / text).min(1.)
        } else {
            1.
        };
        self.width = (fixed + text * self.text_scale).ceil();
    }

    pub fn text_width(&self, id: &Id) -> f32 {
        self.titles
            .get(id)
            .map_or(48., |(_, width)| width * self.text_scale)
    }
}

// Use the same font and shaping as the displayed labels; measure only when a
// title, font or badge count changes. Progress and countdown ticks do not resize.
fn text_width(content: &str, size: f32, font: Font) -> f32 {
    <cosmic::Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content,
        bounds: cosmic::iced::Size::INFINITE,
        size: size.into(),
        line_height: text::LineHeight::Relative(1.1),
        font,
        align_x: Default::default(),
        align_y: cosmic::iced::alignment::Vertical::Top,
        shaping: Default::default(),
        wrapping: text::Wrapping::None,
        ellipsize: Default::default(),
    })
    .min_width()
}
