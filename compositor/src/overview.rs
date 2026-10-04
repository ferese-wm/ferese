use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use ab_glyph::{Font, FontArc, FontVec, ScaleFont};
use ferese_animation::{AnimatedRect, AnimatedValue, CrossingPolicy, SpringConfig};
use ferese_core::{OutputId, WorkspaceId};
use ferese_layout::{Direction, Rect, WindowId};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Transform};

use crate::Ferese;

const OVERVIEW_MARGIN: f64 = 48.0;
const OVERVIEW_GAP: f64 = 40.0;
// Applied after the configured animation speed, in both live and predicted motion.
pub(crate) const OVERVIEW_MOTION_SPEED: f64 = 0.6;
const WORKSPACE_CARD_GAP: f64 = 8.0;
const MAX_PREVIEW_SCALE: f64 = 0.82;

// A single worker loads fonts. The bounded wake channel and latest
// request slot coalesce rapid config reloads without spawning more loaders.
#[derive(Debug)]
struct FontRequests {
    latest: Arc<Mutex<String>>,
    wake: mpsc::SyncSender<()>,
}

impl FontRequests {
    fn request(&self, family: &str) {
        *self.latest.lock().unwrap() = family.to_owned();
        let _ = self.wake.try_send(());
    }
}

pub(crate) fn init_font_loader(
    event_loop: &mut smithay::reexports::calloop::EventLoop<'static, Ferese>,
    state: &mut Ferese,
) -> Result<(), Box<dyn std::error::Error>> {
    use smithay::reexports::calloop::channel;
    let (results, source) = channel::sync_channel::<(String, Option<FontArc>)>(1);
    event_loop.handle().insert_source(source, |event, _, state| {
        if let channel::Event::Msg((family, font)) = event
            && state.overview.complete_font_load(&family, font)
            && state.overview.is_presenting()
        {
            crate::backends::direct::render_all(state);
        }
    })?;
    let latest = Arc::new(Mutex::new(state.overview.font_family.clone()));
    let worker_latest = latest.clone();
    let (wake, requests) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("ferese-overview-font".into())
        .spawn(move || {
            while requests.recv().is_ok() {
                let family = worker_latest.lock().unwrap().clone();
                // Rescan on a font change so newly installed fonts remain usable.
                let mut database = fontdb::Database::new();
                database.load_system_fonts();
                let families = [fontdb::Family::Name(&family), fontdb::Family::SansSerif];
                let font = database
                    .query(&fontdb::Query {
                        families: &families,
                        ..Default::default()
                    })
                    .and_then(|id| {
                        database.with_face_data(id, |bytes, index| {
                            FontVec::try_from_vec_and_index(bytes.to_vec(), index)
                                .ok()
                                .map(FontArc::new)
                        })
                    })
                    .flatten();
                if results.send((family, font)).is_err() {
                    break;
                }
            }
        })?;
    let requests = FontRequests { latest, wake };
    requests.request(&state.overview.font_family);
    state.overview.font_requests = Some(requests);
    Ok(())
}

type LabelBuffer = (MemoryRenderBuffer, smithay::utils::Size<i32, smithay::utils::Buffer>);
type LabelStyle = (u64, [u32; 4]);
type OverviewStateLabel = HashMap<String, HashMap<LabelStyle, LabelBuffer>>;

#[derive(Debug)]
struct WindowLabel {
    revision: u64,
    limit: usize,
    style: LabelStyle,
    buffer: LabelBuffer,
}

#[derive(Debug)]
pub(crate) struct OverviewState {
    motion: OverviewMotion,
    selected: Option<WindowId>,
    selection_workspace: Option<WorkspaceId>,
    strip_offsets: HashMap<OutputId, usize>,
    font_family: String,
    font_requests: Option<FontRequests>,
    font: Option<FontArc>,
    labels: OverviewStateLabel,
    window_labels: HashMap<WindowId, WindowLabel>,
    label_count: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct OverviewMotion {
    active: bool,
    presentations: HashMap<WindowId, AnimatedRect>,
    opacity: AnimatedValue,
}

impl Default for OverviewState {
    fn default() -> Self {
        Self {
            motion: OverviewMotion::default(),
            selected: None,
            selection_workspace: None,
            strip_offsets: HashMap::new(),
            font_family: "sans-serif".into(),
            font_requests: None,
            font: None,
            labels: HashMap::new(),
            window_labels: HashMap::new(),
            label_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceCard {
    pub workspace: WorkspaceId,
    pub index: u32,
    pub rect: Rect,
    pub selected: bool,
    pub windows: Vec<(WindowId, Rect)>,
}

pub(crate) fn workspace_strip(bounds: Rect) -> Rect {
    let inset = 24.0_f64.min(bounds.width * 0.05);
    Rect::new(
        bounds.x + inset,
        bounds.y + 16.0,
        (bounds.width - inset * 2.0).max(1.0),
        (bounds.height * 0.24).clamp(40.0, 132.0),
    )
}

fn window_area(bounds: Rect) -> Rect {
    let strip = workspace_strip(bounds);
    let top = strip.y + strip.height + 16.0;
    Rect::new(bounds.x, top, bounds.width, (bounds.y + bounds.height - top).max(1.0))
}

fn contains(rect: Rect, point: Point<f64, Logical>) -> bool {
    point.x >= rect.x && point.x < rect.x + rect.width && point.y >= rect.y && point.y < rect.y + rect.height
}

fn strip_capacity(strip: Rect) -> usize {
    (((strip.width - 24.0) + WORKSPACE_CARD_GAP) / (160.0 + WORKSPACE_CARD_GAP))
        .floor()
        .max(1.0) as usize
}

fn centered_row_start(strip: Rect, count: usize, card_width: f64) -> f64 {
    let width = count as f64 * card_width + count.saturating_sub(1) as f64 * WORKSPACE_CARD_GAP;
    strip.x + (strip.width - width) * 0.5
}

impl OverviewState {
    pub(crate) fn needs_tick(&self) -> bool {
        self.motion.opacity.current != self.motion.opacity.target
            || self.motion.opacity.velocity != 0.0
            || self
                .motion
                .presentations
                .values()
                .any(|rect| rect.current != rect.target || rect.velocity != ferese_animation::RectVelocity::default())
    }

    pub(crate) fn is_animating(&self, spring: SpringConfig) -> bool {
        self.motion.is_animating(spring)
    }

    pub(crate) fn sample(&self, delta: Duration, spring: SpringConfig) -> OverviewMotion {
        let mut motion = self.motion.clone();

        if !delta.is_zero() {
            motion.advance(delta, spring, true);
        }

        motion
    }

    pub(crate) fn set_font_family(&mut self, family: String) {
        if self.font_family != family {
            self.font_family = family;
            // Keep the current labels visible until the replacement is ready.
            if let Some(requests) = &self.font_requests {
                requests.request(&self.font_family);
            }
        }
    }

    pub(crate) fn with_font_family(font_family: String) -> Self {
        Self {
            font_family,
            ..Self::default()
        }
    }

    fn complete_font_load(&mut self, family: &str, font: Option<FontArc>) -> bool {
        if family != self.font_family {
            return false;
        }
        self.font = font;
        self.labels.clear();
        self.window_labels.clear();
        self.label_count = 0;
        true
    }

    fn label(
        &mut self,
        text: &str,
        scale: f64,
        color: [f32; 4],
    ) -> Option<(MemoryRenderBuffer, smithay::utils::Size<i32, smithay::utils::Buffer>)> {
        let key = (scale.to_bits(), color.map(f32::to_bits));
        if let Some(buffer) = self.labels.get(text).and_then(|styles| styles.get(&key)) {
            return Some(buffer.clone());
        }
        let font = self.font.as_ref()?;
        let scaled = font.as_scaled((13.0 * scale) as f32);
        let width = (text
            .chars()
            .map(|ch| scaled.h_advance(font.glyph_id(ch)))
            .sum::<f32>()
            .ceil() as i32
            + 4)
        .max(1);
        let height = (18.0 * scale).ceil() as i32;
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        let mut pen = 2.0;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            let glyph = id.with_scale_and_position(scaled.scale(), ab_glyph::point(pen, scaled.ascent() + 1.0));
            pen += scaled.h_advance(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|x, y, coverage| {
                    let x = x as i32 + bounds.min.x as i32;
                    let y = y as i32 + bounds.min.y as i32;
                    if x >= 0 && x < width && y >= 0 && y < height {
                        let offset = ((y * width + x) * 4) as usize;
                        let alpha = coverage * color[3];
                        pixels[offset..offset + 4].copy_from_slice(&[
                            (color[0] * alpha * 255.0).round() as u8,
                            (color[1] * alpha * 255.0).round() as u8,
                            (color[2] * alpha * 255.0).round() as u8,
                            (alpha * 255.0).round() as u8,
                        ]);
                    }
                });
            }
        }
        let buffer =
            MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (width, height), 1, Transform::Normal, None);
        if self.label_count >= 128 {
            self.labels.clear();
            self.label_count = 0;
        }
        let label = (buffer, (width, height).into());
        self.labels
            .entry(text.to_owned())
            .or_default()
            .insert(key, label.clone());
        self.label_count += 1;
        Some(label)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.motion.active
    }

    pub(crate) fn is_presenting(&self) -> bool {
        self.motion.is_presenting()
    }

    #[cfg(test)]
    pub(crate) fn opacity(&self) -> f32 {
        self.motion.opacity()
    }

    fn select_workspace_windows(&mut self, windows: &[WindowId], preferred: Option<WindowId>) {
        let visible = |id: &WindowId| windows.contains(id) && self.motion.presentations.contains_key(id);
        self.selected = self
            .selected
            .filter(visible)
            .or_else(|| preferred.filter(visible))
            .or_else(|| windows.iter().copied().filter(visible).min_by_key(|id| id.0));
    }

    pub(crate) fn selection_state(&self) -> (Option<WindowId>, Option<WorkspaceId>) {
        (self.selected, self.selection_workspace)
    }

    pub(crate) fn selected(&self) -> Option<WindowId> {
        self.selected
    }

    pub(crate) fn has_window_preview(&self, id: WindowId) -> bool {
        self.motion.presentations.contains_key(&id)
    }

    pub(crate) fn select_window(&mut self, id: WindowId) -> bool {
        if !self.motion.presentations.contains_key(&id) {
            return false;
        }
        self.selection_workspace = None;
        self.selected = Some(id);
        true
    }

    pub(crate) fn select_direction(&mut self, direction: Direction) -> bool {
        let Some(current) = self
            .selected
            .and_then(|id| self.motion.presentations.get(&id))
            .map(|presentation| presentation.current)
        else {
            return false;
        };
        let current_center = rect_center(current);
        let next = self
            .motion
            .presentations
            .iter()
            .filter(|(id, _)| Some(**id) != self.selected)
            .filter_map(|(id, presentation)| {
                let center = rect_center(presentation.current);
                directional_distance(direction, current_center, center).map(|distance| (*id, distance))
            })
            .min_by(|(left_id, left), (right_id, right)| left.total_cmp(right).then_with(|| left_id.0.cmp(&right_id.0)))
            .map(|(id, _)| id);
        let Some(next) = next else {
            return false;
        };

        self.selection_workspace = None;
        self.selected = Some(next);
        true
    }

    #[cfg(test)]
    pub(crate) fn presented_rect(&self, id: WindowId, normal: Rect) -> Rect {
        self.motion.presented_rect(id, normal)
    }

    pub(crate) fn motion(&self) -> &OverviewMotion {
        &self.motion
    }

    fn seed_presentations(&mut self, windows: impl IntoIterator<Item = crate::presentation::WindowPresentation>) {
        for window in windows {
            self.motion.presentations.entry(window.id).or_insert_with(|| {
                // Opening scales the final presentation after overview. Remove
                // it here so the same opening motion is not applied twice.
                let scale = 0.97 + 0.03 * window.opacity.current;
                let mut bounds = window.bounds;
                bounds.velocity = crate::presentation::scaled_visual_velocity(
                    bounds.current,
                    bounds.velocity,
                    1.0 / scale,
                    -0.03 * window.opacity.velocity / (scale * scale),
                );
                bounds.current = crate::presentation::scaled_visual_rect(bounds.current, 1.0 / scale);
                bounds.velocity.x /= OVERVIEW_MOTION_SPEED;
                bounds.velocity.y /= OVERVIEW_MOTION_SPEED;
                bounds.velocity.width /= OVERVIEW_MOTION_SPEED;
                bounds.velocity.height /= OVERVIEW_MOTION_SPEED;
                bounds
            });
        }
    }

    fn enter(
        &mut self,
        targets: HashMap<WindowId, (Rect, Rect)>,
        selected: Option<WindowId>,
        animations_enabled: bool,
    ) {
        self.selection_workspace = None;
        self.motion.active = true;
        self.motion.opacity.retarget_preserving_motion(1.0);
        if !animations_enabled {
            self.motion.opacity.snap();
        }
        self.strip_offsets.clear();
        self.selected = selected
            .filter(|id| targets.contains_key(id))
            .or_else(|| targets.keys().copied().min_by_key(|id| id.0));
        self.retarget(targets, animations_enabled);
    }

    fn retarget(&mut self, targets: HashMap<WindowId, (Rect, Rect)>, animations_enabled: bool) {
        self.motion.presentations.retain(|id, _| targets.contains_key(id));

        for (id, (normal, target)) in targets {
            let presentation = self
                .motion
                .presentations
                .entry(id)
                .or_insert_with(|| AnimatedRect::new(normal));

            presentation.set_target(target);
            if !animations_enabled {
                presentation.snap();
            }
        }
        if self
            .selected
            .is_none_or(|id| !self.motion.presentations.contains_key(&id))
        {
            self.selected = self.motion.presentations.keys().copied().min_by_key(|id| id.0);
        }
    }

    fn exit(&mut self, normal: HashMap<WindowId, Rect>, animations_enabled: bool) {
        self.motion.active = false;
        self.motion.opacity.retarget_preserving_motion(0.0);
        if !animations_enabled {
            self.motion.opacity.snap();
        }
        self.selected = None;
        self.motion.presentations.retain(|id, presentation| {
            let Some(target) = normal.get(id).copied() else {
                return false;
            };

            presentation.set_target(target);
            if !animations_enabled {
                presentation.snap();
            }
            true
        });

        if !animations_enabled {
            self.motion.presentations.clear();
        }
    }

    pub(crate) fn advance(&mut self, delta: Duration, spring: SpringConfig, animations_enabled: bool) -> bool {
        self.motion.advance(delta, spring, animations_enabled)
    }
}

impl Ferese {
    pub(crate) fn hover_overview_window(&mut self, point: Point<f64, Logical>) {
        if !self.overview.is_active() {
            return;
        }
        let selected = self
            .window_under_visual(point)
            .and_then(|w| self.windows.ids().get(&w).copied());

        if selected.is_some() && self.overview.selected != selected {
            let previous = self.overview.selection_state();
            self.overview.selection_workspace = None;
            self.overview.selected = selected;
            self.redraw_overview_selection(previous);
        }
    }

    pub(crate) fn redraw_overview_selection(&mut self, previous: (Option<WindowId>, Option<WorkspaceId>)) {
        let outputs = self
            .space
            .outputs()
            .filter(|output| {
                [previous.0, self.overview.selected]
                    .into_iter()
                    .flatten()
                    .any(|id| self.window_belongs_to_output(id, output))
                    || [previous.1, self.overview.selection_workspace]
                        .into_iter()
                        .flatten()
                        .any(|workspace| {
                            self.output_workspaces.output_for_workspace(workspace) == self.output_id(output)
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        crate::backends::direct::render_on(self, &outputs);
    }

    pub(crate) fn retain_overview_labels(&mut self) {
        self.overview
            .window_labels
            .retain(|id, _| self.windows.window(*id).is_some());
    }

    pub(crate) fn overview_window_label(&mut self, id: WindowId, scale: f64, width: f64) -> Option<LabelBuffer> {
        let record = self.windows.record(id)?;
        let limit = ((width - 16.0) / 8.0).clamp(1.0, 48.0) as usize;
        let color = self.theme_settings.text_primary_color.0;
        let style = (scale.to_bits(), color.map(f32::to_bits));

        if let Some(label) = self.overview.window_labels.get(&id)
            && label.revision == record.metadata_revision
            && label.limit == limit
            && label.style == style
        {
            return Some(label.buffer.clone());
        }

        let title = if !record.title.trim().is_empty() {
            &record.title
        } else if !record.app_id.is_empty() {
            &record.app_id
        } else {
            "Window"
        };
        let mut chars = title.chars().filter(|c| !c.is_control());
        let mut text: String = chars.by_ref().take(limit).collect();

        if chars.next().is_some() {
            text.pop();
            text.push('…');
        }

        let revision = record.metadata_revision;
        let buffer = self.overview.label(&text, scale, color)?;
        self.overview.window_labels.insert(
            id,
            WindowLabel {
                revision,
                limit,
                style,
                buffer: buffer.clone(),
            },
        );
        Some(buffer)
    }

    pub(crate) fn overview_workspace_label(
        &mut self,
        index: u32,
        scale: f64,
    ) -> Option<(MemoryRenderBuffer, smithay::utils::Size<i32, smithay::utils::Buffer>)> {
        self.overview
            .label(&index.to_string(), scale, self.theme_settings.text_primary_color.0)
    }

    pub(crate) fn set_overview_active(&mut self, active: bool) {
        if !active {
            self.focus_cycle = None;
        }
        if self.overview.is_active() == active {
            return;
        }
        // Account for idle time before starting a new transition, not after.
        self.advance_animations(std::time::Instant::now());

        if active {
            let presentations = self
                .windows
                .ordered_ids()
                .filter_map(|id| self.current_window_presentation(id))
                .collect::<Vec<_>>();
            self.overview.seed_presentations(presentations);
            if self.cancel_workspace_slides() {
                self.relayout();
            }
            let targets = self.overview_targets();
            self.overview
                .enter(targets, self.focused_window, self.animations_enabled());
        } else {
            let normal = self.normal_window_rects();
            self.overview.exit(normal, self.animations_enabled());
        }

        self.notify_overview_state();
        crate::backends::direct::render_all(self);
    }

    pub(crate) fn toggle_overview(&mut self) {
        self.set_overview_active(!self.overview.is_active());
    }

    pub(crate) fn focus_overview_direction(&mut self, direction: Direction) -> bool {
        if !self.overview.is_active() {
            return false;
        }

        let previous = self.overview.selection_state();
        if self.overview.select_direction(direction) {
            self.redraw_overview_selection(previous);
        }

        true
    }

    pub(crate) fn select_overview_window(&mut self, id: WindowId) -> bool {
        if !self.overview.is_active() || !self.overview.motion.presentations.contains_key(&id) {
            return false;
        }

        if !self.activate_managed_window(id) {
            return false;
        }

        self.set_overview_active(false);
        true
    }

    pub(crate) fn retarget_overview(&mut self) {
        if !self.overview.is_active() {
            if self.overview.is_presenting() {
                let normal = self.normal_window_rects();
                self.overview.motion.presentations.retain(|id, presentation| {
                    if let Some(target) = normal.get(id) {
                        presentation.set_target(*target);
                        true
                    } else {
                        false
                    }
                });
            }
            return;
        }

        let targets = self.overview_targets();
        self.overview.retarget(targets, self.animations_enabled());
        if let Some(cycle) = self.focus_cycle.as_mut() {
            let available = self.overview.motion.presentations.keys().copied().collect::<Vec<_>>();
            if let Some(id) = cycle.reconcile(&available) {
                self.overview.select_window(id);
            }
            return;
        }
        if self.overview.selection_workspace.is_some() {
            let id = self.workspaces.active_id();
            self.overview.selection_workspace = Some(id);
            let windows = self
                .workspaces
                .workspace(id)
                .map(|workspace| {
                    workspace
                        .layout
                        .window_ids()
                        .chain(workspace.floating.iter().copied())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.overview.select_workspace_windows(&windows, self.focused_window);
        }
    }

    pub(crate) fn presented_window_rect(&self, id: WindowId) -> Option<Rect> {
        Some(self.current_window_presentation(id)?.bounds.current)
    }

    pub(crate) fn inverse_presented_window_point(&self, id: WindowId, x: f64, y: f64) -> Option<(f64, f64)> {
        let geometry = self.windows.geometry(&id)?;
        if !self.overview.is_presenting() && self.windows.record(id)?.opening.is_none() {
            let presented = self.presented_window_rect(id)?;
            return Some((x - presented.x, y - presented.y));
        }
        let source = if self.overview.is_presenting() {
            geometry.client.committed_size?
        } else {
            ferese_animation::ClientSize::from_rect(geometry.visual.current)
        };
        let presented = self.presented_window_rect(id)?;

        if source.width <= 0 || source.height <= 0 || presented.width <= 0.0 || presented.height <= 0.0 {
            return None;
        }

        let scale_x = presented.width / f64::from(source.width);
        let scale_y = presented.height / f64::from(source.height);

        Some(((x - presented.x) / scale_x, (y - presented.y) / scale_y))
    }

    pub(crate) fn overview_selected(&self, id: WindowId) -> bool {
        self.overview.is_active() && self.overview.selected() == Some(id)
    }

    pub(crate) fn overview_workspace_cards(&self, output: &Output) -> Vec<WorkspaceCard> {
        self.overview_workspace_cards_for_frame(output, None)
    }

    pub(crate) fn overview_workspace_cards_for_frame(
        &self,
        output: &Output,
        frame: Option<&crate::state::FrameScene>,
    ) -> Vec<WorkspaceCard> {
        let Some(bounds) = self.output_bounds_for(output) else {
            return Vec::new();
        };
        let Some(output_id) = self.output_id(output) else {
            return Vec::new();
        };
        let selected = self.output_workspaces.active_workspace(output_id);
        let strip = workspace_strip(bounds);
        let capacity = strip_capacity(strip);
        let workspaces = self
            .output_workspaces
            .workspace_views_for_output(&self.workspaces, output_id)
            .collect::<Vec<_>>();
        let selected_index = workspaces
            .iter()
            .position(|workspace| Some(workspace.id) == selected)
            .unwrap_or(0);
        let max_offset = workspaces.len().saturating_sub(capacity);
        let offset = self
            .overview
            .strip_offsets
            .get(&output_id)
            .copied()
            .unwrap_or_else(|| selected_index.saturating_sub(capacity - 1))
            .min(max_offset);
        let card_width = ((strip.width - 24.0 - WORKSPACE_CARD_GAP * capacity.saturating_sub(1) as f64)
            / capacity as f64)
            .clamp(1.0, 160.0);
        let visible_count = workspaces.len().saturating_sub(offset).min(capacity);
        let start_x = centered_row_start(strip, visible_count, card_width);
        workspaces
            .into_iter()
            .skip(offset)
            .take(capacity)
            .enumerate()
            .map(|(index, view)| {
                let workspace = self.workspaces.workspace(view.id).expect("workspace view exists");

                let rect = Rect::new(
                    start_x + index as f64 * (card_width + WORKSPACE_CARD_GAP),
                    strip.y + 12.0,
                    card_width,
                    (strip.height - 24.0).max(1.0),
                );
                let windows = workspace
                    .layout
                    .window_ids()
                    .chain(workspace.floating.iter().copied())
                    .take(4)
                    .filter_map(|id| {
                        let source = frame
                            .and_then(|frame| frame.windows.get(&id).map(|window| &window.geometry))
                            .or_else(|| self.windows.geometry(&id))?
                            .visual
                            .current;
                        Some((id, source))
                    })
                    .collect::<Vec<_>>();
                let preview_bounds = Rect::new(
                    rect.x + 6.0,
                    rect.y + 6.0,
                    (rect.width - 12.0).max(1.0),
                    (rect.height - 26.0).max(1.0),
                );
                let windows = preview_layout(preview_bounds, &windows, 4.0, 4.0);
                WorkspaceCard {
                    workspace: workspace.id,
                    index: view.index,
                    rect,
                    selected: Some(workspace.id) == selected,
                    windows,
                }
            })
            .collect()
    }

    pub(crate) fn overview_strip_at(&self, point: Point<f64, Logical>) -> Option<Output> {
        if !self.overview.is_active() {
            return None;
        }
        let output = self.space.output_under(point).next()?;
        self.output_bounds_for(output)
            .is_some_and(|bounds| contains(workspace_strip(bounds), point))
            .then(|| output.clone())
    }

    pub(crate) fn click_overview_workspace(&mut self, point: Point<f64, Logical>) -> bool {
        let Some(output) = self.overview_strip_at(point) else {
            return false;
        };

        let card = self
            .overview_workspace_cards(&output)
            .into_iter()
            .find(|card| contains(card.rect, point));

        if let Some(card) = card {
            let Some(output_id) = self.output_id(&output) else {
                return true;
            };

            // Consume idle time before creating the new slide. Subsequent
            // clicks then retarget the motion currently visible on this output.
            self.advance_animations(std::time::Instant::now());
            let previous = self.output_workspaces.active_workspace(output_id);
            if let Ok(
                ferese_core::WorkspaceSwitch::Activated(owner) | ferese_core::WorkspaceSwitch::FocusedExisting(owner),
            ) = self.output_workspaces.switch_workspace(output_id, card.workspace)
            {
                self.update_workspace_slide(output_id, owner, previous, card.workspace, None);
                // Existing monitor ownership wins; never steal another output's workspace.
                self.restore_output_focus();
                self.overview.strip_offsets.clear();
                self.overview.selection_workspace = Some(card.workspace);
                self.relayout();
                self.restore_keyboard_focus();
            }
        }
        crate::backends::direct::render_all(self);
        true
    }

    pub(crate) fn scroll_overview_strip(&mut self, point: Point<f64, Logical>, delta: f64) -> bool {
        let Some(output) = self.overview_strip_at(point) else {
            return false;
        };
        let Some(output_id) = self.output_id(&output) else {
            return true;
        };
        let Some(bounds) = self.output_bounds_for(&output) else {
            return true;
        };
        let capacity = strip_capacity(workspace_strip(bounds));
        let cards = self.overview_workspace_cards(&output);
        let ordered = self.output_workspaces.ordered_workspaces(&self.workspaces, output_id);
        let current = cards
            .first()
            .and_then(|card| ordered.iter().position(|id| *id == card.workspace))
            .unwrap_or(0);
        let next = if delta > 0.0 {
            current.saturating_add(1)
        } else if delta < 0.0 {
            current.saturating_sub(1)
        } else {
            current
        };
        self.overview
            .strip_offsets
            .insert(output_id, next.min(ordered.len().saturating_sub(capacity)));
        crate::backends::direct::render_on(self, &[output]);
        true
    }

    fn overview_targets(&self) -> HashMap<WindowId, (Rect, Rect)> {
        let mut targets = HashMap::new();

        for output in self.space.outputs() {
            let Some(output_id) = self.output_id(output) else {
                continue;
            };
            let Some(workspace) = self.output_workspaces.active_workspace(output_id) else {
                continue;
            };
            let Some(bounds) = self.output_bounds_for(output) else {
                continue;
            };
            if let Some(cycle) = &self.focus_cycle {
                let windows = cycle
                    .windows()
                    .iter()
                    .filter_map(|id| {
                        if self.focus_preview_output(*id) != Some(output_id) {
                            return None;
                        }
                        Some((*id, self.windows.geometry(id)?.visual.current))
                    })
                    .collect::<Vec<_>>();
                for (id, target) in overview_layout(window_area(bounds), &windows) {
                    targets.insert(id, (self.windows.geometry(&id).unwrap().visual.current, target));
                }
                continue;
            }
            let mut visible = vec![workspace];
            let sliding = self.workspace_slide_workspaces(output_id).collect::<Vec<_>>();
            visible.extend(sliding.iter().copied().filter(|id| *id != workspace));
            for workspace in visible {
                let Some(workspace) = self.workspaces.workspace(workspace) else {
                    continue;
                };
                let windows = workspace
                    .layout
                    .window_ids()
                    .chain(workspace.floating.iter().copied())
                    .filter_map(|id| {
                        let normal = self.windows.geometry(&id)?.visual.current;
                        Some((id, normal))
                    })
                    .collect::<Vec<_>>();

                for (id, target) in overview_layout(window_area(bounds), &windows) {
                    let normal = self.windows.geometry(&id).unwrap().visual.current;
                    // Incoming grids start at their overview size. Translation
                    // belongs to the workspace slide, not a second desktop zoom.
                    let source = if !sliding.is_empty() { target } else { normal };
                    targets.insert(id, (source, target));
                }
            }
        }

        targets
    }

    fn normal_window_rects(&self) -> HashMap<WindowId, Rect> {
        self.windows
            .geometries()
            .map(|(id, geometry)| (*id, geometry.visual.current))
            .collect()
    }
}

fn overview_layout(bounds: Rect, windows: &[(WindowId, Rect)]) -> HashMap<WindowId, Rect> {
    preview_layout(bounds, windows, OVERVIEW_MARGIN, OVERVIEW_GAP)
        .into_iter()
        .collect()
}

// Preserve input order for rendering: the damage tracker includes each
// instance's z index, even when neighboring previews do not overlap.
fn preview_layout(bounds: Rect, windows: &[(WindowId, Rect)], margin: f64, gap: f64) -> Vec<(WindowId, Rect)> {
    if windows.is_empty() {
        return Vec::new();
    }

    let margin = margin.min(bounds.width * 0.1).min(bounds.height * 0.1);
    let content_width = (bounds.width - margin * 2.0).max(1.0);
    let content_height = (bounds.height - margin * 2.0).max(1.0);
    // Choose the grid from the actual window shapes. Three tall windows should
    // share one row instead of becoming tiny previews spread over two rows.
    let cell_size = |columns: usize| {
        let rows = windows.len().div_ceil(columns);
        (
            ((content_width - gap * columns.saturating_sub(1) as f64) / columns as f64).clamp(1.0, 480.0),
            ((content_height - gap * rows.saturating_sub(1) as f64) / rows as f64).clamp(1.0, 420.0),
        )
    };
    let score = |columns: usize| {
        let (w, h) = cell_size(columns);
        windows
            .iter()
            .map(|(_, source)| {
                let scale = (w / source.width.max(1.0))
                    .min(h / source.height.max(1.0))
                    .min(MAX_PREVIEW_SCALE);
                source.width * source.height * scale * scale
            })
            .sum::<f64>()
    };
    let columns = (1..=windows.len())
        .max_by(|a, b| {
            let (sa, sb) = (score(*a), score(*b));
            if (sa - sb).abs() < 1.0 {
                windows.len().div_ceil(*b).cmp(&windows.len().div_ceil(*a))
            } else {
                sa.total_cmp(&sb)
            }
        })
        .unwrap_or(1);
    let rows = windows.len().div_ceil(columns);
    let (cell_width, cell_height) = cell_size(columns);
    let grid_height = cell_height * rows as f64 + gap * rows.saturating_sub(1) as f64;
    let origin_y = bounds.y + (bounds.height - grid_height) * 0.5;

    windows
        .iter()
        .enumerate()
        .map(|(index, (id, source))| {
            let column = index % columns;
            let row = index / columns;
            let scale = (cell_width / source.width.max(1.0))
                .min(cell_height / source.height.max(1.0))
                .min(MAX_PREVIEW_SCALE);
            let width = (source.width * scale).max(1.0);
            let height = (source.height * scale).max(1.0);
            let row_count = (windows.len() - row * columns).min(columns);
            let row_width = cell_width * row_count as f64 + gap * row_count.saturating_sub(1) as f64;
            let cell_x = bounds.x + (bounds.width - row_width) * 0.5 + column as f64 * (cell_width + gap);
            let cell_y = origin_y + row as f64 * (cell_height + gap);
            let target = Rect::new(
                cell_x + (cell_width - width) / 2.0,
                cell_y + (cell_height - height) / 2.0,
                width,
                height,
            );

            (*id, target)
        })
        .collect()
}

fn rect_center(rect: Rect) -> (f64, f64) {
    (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn directional_distance(direction: Direction, current: (f64, f64), candidate: (f64, f64)) -> Option<f64> {
    let delta_x = candidate.0 - current.0;
    let delta_y = candidate.1 - current.1;
    let (primary, secondary) = match direction {
        Direction::Left if delta_x < 0.0 => (-delta_x, delta_y.abs()),
        Direction::Right if delta_x > 0.0 => (delta_x, delta_y.abs()),
        Direction::Up if delta_y < 0.0 => (-delta_y, delta_x.abs()),
        Direction::Down if delta_y > 0.0 => (delta_y, delta_x.abs()),
        _ => return None,
    };

    Some(primary + secondary * 0.35)
}

impl Default for OverviewMotion {
    fn default() -> Self {
        Self {
            active: false,
            presentations: HashMap::new(),
            opacity: AnimatedValue::new(0.0),
        }
    }
}

impl OverviewMotion {
    pub(crate) fn is_animating(&self, spring: SpringConfig) -> bool {
        (self.opacity.current - self.opacity.target).abs() > 0.001
            || self.opacity.velocity.abs() > 0.005
            || self.presentations.values().any(|rect| !rect.is_settled(spring))
    }

    pub(crate) fn is_presenting(&self) -> bool {
        self.active || !self.presentations.is_empty() || self.opacity.current > 0.001
    }

    pub(crate) fn opacity(&self) -> f32 {
        self.opacity.current.clamp(0.0, 1.0) as f32
    }

    pub(crate) fn presented_bounds(&self, id: WindowId, normal: AnimatedRect) -> AnimatedRect {
        let Some(mut bounds) = self.presentations.get(&id).copied() else {
            return normal;
        };
        bounds.velocity.x *= OVERVIEW_MOTION_SPEED;
        bounds.velocity.y *= OVERVIEW_MOTION_SPEED;
        bounds.velocity.width *= OVERVIEW_MOTION_SPEED;
        bounds.velocity.height *= OVERVIEW_MOTION_SPEED;
        bounds
    }

    #[cfg(test)]
    pub(crate) fn presented_rect(&self, id: WindowId, normal: Rect) -> Rect {
        self.presentations.get(&id).map_or(normal, |p| p.current)
    }

    pub(crate) fn advance(&mut self, delta: Duration, spring: SpringConfig, animations_enabled: bool) -> bool {
        let delta = delta.mul_f64(OVERVIEW_MOTION_SPEED);

        let mut active_animation = false;

        for presentation in self.presentations.values_mut() {
            if animations_enabled {
                active_animation |= presentation.advance(delta, spring);
            } else {
                presentation.snap();
            }
        }

        if !self.active && !active_animation {
            self.presentations.clear();
        }

        if animations_enabled {
            // Geometry's 0.1-pixel tolerance is far too coarse for opacity:
            // it would abruptly drop the last ten percent of the fade.
            active_animation |= self.opacity.advance_with_policy(
                delta,
                SpringConfig {
                    position_tolerance: 0.001,
                    velocity_tolerance: 0.005,
                    ..spring
                },
                CrossingPolicy::NoCrossing,
            );
        } else {
            self.opacity.snap();
        }

        active_animation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_entry_inherits_visible_velocity_and_does_not_double_opening_scale() {
        use crate::presentation::{WindowPresentation, scaled_visual_rect, scaled_visual_velocity};
        use ferese_animation::RectVelocity;
        let normal = Rect::new(20.25, -3.5, 640.0, 480.0);
        let velocity = RectVelocity {
            x: 300.0,
            y: -50.0,
            width: 25.0,
            height: 8.0,
        };
        let opacity = AnimatedValue {
            current: 0.5,
            target: 1.0,
            velocity: 0.8,
        };
        let scale = 0.97 + 0.03 * opacity.current;
        let id = WindowId(1);
        let presentation = WindowPresentation {
            id,
            bounds: AnimatedRect {
                current: scaled_visual_rect(normal, scale),
                target: normal,
                velocity: scaled_visual_velocity(normal, velocity, scale, 0.03 * opacity.velocity),
            },
            opacity,
            emphasis: AnimatedValue::new(1.0),
            shadow: AnimatedValue::new(1.0),
            scale_content: false,
            native_size: None,
        };
        let mut overview = OverviewState::default();
        overview.seed_presentations([presentation]);
        overview.enter(
            HashMap::from([(id, (normal, Rect::new(200.0, 100.0, 320.0, 240.0)))]),
            Some(id),
            true,
        );
        let sampled = overview.motion.presented_bounds(id, AnimatedRect::new(normal));
        assert!((sampled.current.x - normal.x).abs() < 1e-10);
        assert!((sampled.current.width - normal.width).abs() < 1e-10);
        assert!((sampled.velocity.x - velocity.x).abs() < 1e-10);
        assert!((sampled.velocity.width - velocity.width).abs() < 1e-10);
        // Re-entering during dismissal must keep the current spring, not seed
        // it from a stale desktop sample.
        overview.exit(HashMap::from([(id, normal)]), true);
        let before = overview.motion.presentations[&id];
        overview.seed_presentations([presentation]);
        assert_eq!(overview.motion.presentations[&id], before);
    }

    #[test]
    fn tiny_overview_target_changes_still_get_a_settling_tick() {
        let mut overview = OverviewState::default();
        assert!(!overview.needs_tick());
        overview.motion.opacity.set_target(0.0001);
        assert!(!overview.is_animating(SpringConfig::default()));
        assert!(overview.needs_tick());
    }

    fn test_font() -> FontArc {
        FontArc::try_from_slice(include_bytes!("../../assets/fonts/Comfortaa-Regular.otf")).unwrap()
    }

    #[test]
    fn font_reload_keeps_labels_until_current_result_arrives() {
        let mut overview = OverviewState::with_font_family("first".into());
        assert!(overview.complete_font_load("first", Some(test_font())));
        assert!(overview.label("1", 1.0, [1.0; 4]).is_some());
        assert_eq!(overview.labels.len(), 1);
        overview.set_font_family("second".into());
        assert!(overview.font.is_some());
        assert_eq!(overview.labels.len(), 1);
        assert!(!overview.complete_font_load("first", None));
        assert!(overview.font.is_some());
        assert_eq!(overview.labels.len(), 1);
        assert!(overview.complete_font_load("second", Some(test_font())));
        assert!(overview.labels.is_empty());
        assert!(overview.label("1", 1.0, [1.0; 4]).is_some());
    }

    #[test]
    fn rapid_font_requests_are_bounded_and_keep_latest_family() {
        let (wake, receiver) = mpsc::sync_channel(1);
        let requests = FontRequests {
            latest: Arc::new(Mutex::new(String::new())),
            wake,
        };
        for family in ["first", "second", "third"] {
            requests.request(family);
        }
        receiver.try_recv().unwrap();
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(*requests.latest.lock().unwrap(), "third");
        drop(requests);
        assert!(matches!(receiver.recv(), Err(mpsc::RecvError)));
    }

    #[test]
    fn labels_do_not_load_fonts_synchronously() {
        let mut overview = OverviewState::default();
        assert!(overview.label("1", 1.0, [1.0; 4]).is_none());
        assert!(overview.font.is_none());
        assert!(overview.labels.is_empty());
    }

    #[test]
    fn workspace_cards_are_centered_as_a_group_on_each_output() {
        for origin in [0.0, 1600.0] {
            let strip = Rect::new(origin + 24.0, 64.0, 1552.0, 132.0);
            for count in [1, 3, 8] {
                let start = centered_row_start(strip, count, 160.0);
                let end = start + count as f64 * 160.0 + count.saturating_sub(1) as f64 * WORKSPACE_CARD_GAP;
                assert!((start - strip.x - (strip.x + strip.width - end)).abs() < 0.001);
            }
        }
    }

    #[test]
    fn only_strip_is_tinted_and_windows_start_below_it() {
        for bounds in [
            Rect::new(0.0, 48.0, 1600.0, 952.0),
            Rect::new(1600.0, 48.0, 2400.0, 1302.0),
        ] {
            let strip = workspace_strip(bounds);
            let area = window_area(bounds);
            assert!(strip.height < bounds.height / 2.0);
            assert!(area.y > strip.y + strip.height);
            let windows = [(WindowId(1), Rect::new(0.0, 0.0, 800.0, 900.0))];
            assert!(overview_layout(area, &windows).values().all(|rect| rect.y >= area.y));
        }
    }

    #[test]
    fn empty_workspace_fades_without_an_abrupt_last_ten_percent() {
        let mut overview = OverviewState::default();
        overview.enter(HashMap::new(), None, false);
        overview.exit(HashMap::new(), true);
        assert!(overview.is_presenting());
        assert_eq!(overview.opacity(), 1.0);
        let mut previous = 1.0;
        for _ in 0..100 {
            overview.advance(Duration::from_millis(16), SpringConfig::default(), true);
            let current = overview.opacity();
            assert!(current <= previous);
            if current == 0.0 {
                assert!(previous < 0.01);
            }
            previous = current;
        }
        assert_eq!(overview.opacity(), 0.0);
        assert!(!overview.is_presenting());
    }

    #[test]
    fn sampling_overview_exit_does_not_finish_the_live_transition() {
        let mut overview = OverviewState::default();
        let id = WindowId(1);
        let normal = Rect::new(0., 0., 800., 600.);
        let thumbnail = Rect::new(100., 100., 200., 150.);
        overview.enter(HashMap::from([(id, (normal, thumbnail))]), Some(id), false);
        overview.exit(HashMap::from([(id, normal)]), true);
        overview.advance(Duration::from_millis(280), SpringConfig::default(), true);
        let before = overview.presented_rect(id, normal);
        let opacity = overview.opacity();

        let finished = overview.sample(Duration::from_secs(2), SpringConfig::default());
        let earlier = overview.sample(Duration::from_millis(4), SpringConfig::default());

        assert!(!finished.is_presenting());
        assert_eq!(finished.presented_rect(id, normal), normal);
        assert!(earlier.is_presenting());
        assert!(earlier.opacity() < opacity);
        assert!(overview.is_presenting());
        assert_eq!(overview.presented_rect(id, normal), before);
        assert_eq!(overview.opacity(), opacity);
        assert_eq!(
            overview
                .sample(Duration::ZERO, SpringConfig::default())
                .presented_rect(id, normal),
            before
        );
    }

    #[test]
    fn reversing_overview_preserves_current_opacity() {
        let mut overview = OverviewState::default();
        overview.enter(HashMap::new(), None, true);
        overview.advance(Duration::from_millis(48), SpringConfig::default(), true);
        let opacity = overview.opacity();
        assert!(opacity > 0.0 && opacity < 1.0);
        overview.exit(HashMap::new(), true);
        assert_eq!(overview.opacity(), opacity);
        overview.advance(Duration::from_millis(16), SpringConfig::default(), true);
        let opacity = overview.opacity();
        overview.enter(HashMap::new(), None, true);
        assert_eq!(overview.opacity(), opacity);
    }

    #[test]
    fn overview_entry_uses_sixty_percent_speed_for_live_and_predicted_geometry() {
        let id = WindowId(1);
        let normal = Rect::new(0.0, 0.0, 800.0, 600.0);
        let thumbnail = Rect::new(100.0, 100.0, 200.0, 150.0);
        let spring = SpringConfig::default();
        let mut overview = OverviewState::default();
        overview.enter(HashMap::from([(id, (normal, thumbnail))]), Some(id), true);

        let predicted = overview.sample(Duration::from_millis(100), spring);
        assert_eq!(overview.presented_rect(id, normal), normal);

        let mut reference = AnimatedRect::new(normal);
        reference.set_target(thumbnail);
        reference.advance_with_policy(Duration::from_millis(60), spring, CrossingPolicy::NoCrossing);
        assert_eq!(predicted.presented_rect(id, normal), reference.current);

        overview.advance(Duration::from_millis(100), spring, true);
        assert_eq!(
            overview.presented_rect(id, normal),
            predicted.presented_rect(id, normal)
        );
        assert_eq!(overview.opacity(), predicted.opacity());
    }

    #[test]
    fn reduced_motion_snaps_overview_chrome() {
        let mut overview = OverviewState::default();
        let id = WindowId(1);
        let normal = Rect::new(0.0, 0.0, 800.0, 600.0);
        let thumbnail = Rect::new(100.0, 100.0, 200.0, 150.0);
        overview.enter(HashMap::from([(id, (normal, thumbnail))]), Some(id), false);
        assert_eq!(overview.opacity(), 1.0);
        assert_eq!(overview.presented_rect(id, normal), thumbnail);

        overview.exit(HashMap::from([(id, normal)]), false);
        assert_eq!(overview.opacity(), 0.0);
        assert_eq!(overview.presented_rect(id, normal), normal);
        assert!(!overview.is_presenting());
    }

    #[test]
    fn thumbnail_grid_preserves_aspect_and_does_not_overlap_labels() {
        let bounds = Rect::new(30.0, 50.0, 148.0, 76.0);
        let windows = (1..=4)
            .map(|id| (WindowId(id), Rect::new(0.0, 0.0, 1000.0, 800.0)))
            .collect::<Vec<_>>();
        let layout = preview_layout(bounds, &windows, 4.0, 4.0);
        assert_eq!(
            layout.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            windows.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        );

        for (_, rect) in layout {
            assert!((rect.width / rect.height - 1.25).abs() < 0.001);
            assert!(rect.y + rect.height <= bounds.y + bounds.height);
            assert!(rect.x + rect.width <= bounds.x + bounds.width);
        }
    }

    #[test]
    fn overview_grid_fits_windows_inside_the_output() {
        let bounds = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let windows = (1..=5)
            .map(|id| (WindowId(id), Rect::new(0.0, 0.0, 960.0, 1000.0)))
            .collect::<Vec<_>>();
        let layout = overview_layout(bounds, &windows);

        assert_eq!(layout.len(), windows.len());
        assert!(layout.values().all(|rect| {
            rect.x >= bounds.x
                && rect.y >= bounds.y
                && rect.x + rect.width <= bounds.x + bounds.width
                && rect.y + rect.height <= bounds.y + bounds.height
        }));
    }

    #[test]
    fn tall_windows_share_a_row_when_their_shapes_fit() {
        let bounds = Rect::new(0.0, 0.0, 1192.0, 1100.0);
        let windows = (1..=3)
            .map(|id| (WindowId(id), Rect::new(0.0, 0.0, 586.0, 1250.0)))
            .collect::<Vec<_>>();
        let grid = overview_layout(bounds, &windows);
        let first = grid[&WindowId(1)];
        for rect in grid.values() {
            assert_eq!(rect.y, first.y);
            assert!(rect.height >= 400.0);
            assert!(rect.x >= 0.0 && rect.x + rect.width <= bounds.width);
        }
    }

    #[test]
    fn overview_grid_preserves_preview_aspect_ratio() {
        let source = Rect::new(0.0, 0.0, 1200.0, 800.0);
        let layout = overview_layout(Rect::new(0.0, 0.0, 1920.0, 1080.0), &[(WindowId(1), source)]);
        let preview = layout[&WindowId(1)];

        assert!((preview.width / preview.height - source.width / source.height).abs() < 0.001);
    }

    #[test]
    fn mru_selection_can_cross_workspace_selection_and_rejects_missing_previews() {
        let first = WindowId(1);
        let second = WindowId(2);
        let normal = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut overview = OverviewState::default();
        overview.enter(
            HashMap::from([(first, (normal, normal)), (second, (normal, normal))]),
            Some(first),
            false,
        );
        overview.selection_workspace = Some(WorkspaceId(1));
        assert!(overview.select_window(second));
        assert_eq!(overview.selected(), Some(second));
        assert_eq!(overview.selection_workspace, None);
        assert!(!overview.select_window(WindowId(99)));
        assert_eq!(overview.selected(), Some(second));
    }

    #[test]
    fn exiting_overview_retargets_without_resetting_motion() {
        let id = WindowId(1);
        let initial = Rect::new(-400.0, 0.0, 800.0, 600.0);
        let preview = Rect::new(100.0, 100.0, 400.0, 300.0);
        let moved = Rect::new(200.0, 0.0, 800.0, 600.0);
        let mut overview = OverviewState::default();
        overview.enter(HashMap::from([(id, (initial, preview))]), Some(id), false);
        overview.exit(HashMap::from([(id, initial)]), true);
        overview.advance(Duration::from_millis(60), SpringConfig::default(), true);
        let before = overview.motion.presentations[&id];
        overview.exit(HashMap::from([(id, moved)]), true);
        let after = overview.motion.presentations[&id];
        assert_eq!(before.current, after.current);
        assert_eq!(before.velocity, after.velocity);
        for _ in 0..300 {
            overview.advance(Duration::from_millis(8), SpringConfig::default(), true);
        }
        assert!(!overview.is_presenting());
        assert_eq!(overview.presented_rect(id, moved), moved);
    }

    #[test]
    fn reversing_exit_preserves_the_visible_window_position() {
        let id = WindowId(1);
        let normal = Rect::new(0.0, 0.0, 800.0, 600.0);
        let preview = Rect::new(100.0, 100.0, 400.0, 300.0);
        let targets = HashMap::from([(id, (normal, preview))]);
        let mut overview = OverviewState::default();
        overview.enter(targets.clone(), Some(id), false);
        overview.exit(HashMap::from([(id, normal)]), true);
        overview.advance(Duration::from_millis(60), SpringConfig::default(), true);
        let shown = overview.presented_rect(id, normal);
        let velocity = overview.motion.presentations[&id].velocity;
        overview.enter(targets, Some(id), true);
        assert_eq!(overview.motion.presentations[&id].velocity, velocity);
        assert_eq!(overview.presented_rect(id, normal), shown);
    }

    #[test]
    fn workspace_selection_replaces_other_output_selection_and_keeps_empty_selection() {
        let old = WindowId(1);
        let new = WindowId(2);
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let targets = HashMap::from([(old, (rect, rect)), (new, (rect, rect))]);
        let mut overview = OverviewState::default();
        overview.enter(targets.clone(), Some(old), false);

        overview.retarget(targets.clone(), false);
        overview.select_workspace_windows(&[new], Some(new));
        assert!(overview.is_active());
        assert_eq!(overview.selected(), Some(new));

        overview.retarget(targets.clone(), false);
        overview.select_workspace_windows(&[], None);
        assert_eq!(overview.selected(), None);
        assert!(overview.is_active());

        overview.retarget(targets, false);
        overview.select_workspace_windows(&[new], None);
        assert_eq!(overview.selected(), Some(new));
    }

    #[test]
    fn leaving_overview_keeps_presentations_until_they_settle() {
        let id = WindowId(1);
        let normal = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let target = Rect::new(200.0, 150.0, 500.0, 400.0);
        let mut overview = OverviewState::default();

        overview.enter(HashMap::from([(id, (normal, target))]), Some(id), false);
        overview.exit(HashMap::from([(id, normal)]), true);

        assert_eq!(overview.presented_rect(id, normal), target);
        assert!(!overview.is_active());
        assert!(overview.advance(Duration::from_millis(16), SpringConfig::default(), true));
    }

    #[test]
    fn directional_selection_uses_presented_geometry() {
        let left = WindowId(1);
        let right = WindowId(2);
        let left_rect = Rect::new(0.0, 0.0, 400.0, 400.0);
        let right_rect = Rect::new(500.0, 0.0, 400.0, 400.0);
        let mut overview = OverviewState::default();

        overview.enter(
            HashMap::from([(left, (left_rect, left_rect)), (right, (right_rect, right_rect))]),
            Some(left),
            false,
        );

        assert!(overview.select_direction(Direction::Right));
        assert_eq!(overview.selected(), Some(right));
        assert!(!overview.select_direction(Direction::Right));
    }
}
