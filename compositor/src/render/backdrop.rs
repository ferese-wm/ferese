use super::{
    AnimatedWindowRenderElement, Buffer, CommitCounter, Element, Id, Ordering, Physical, Rectangle,
    RenderScale, Transform,
};

#[derive(Clone, Debug, Default)]
pub(super) struct Backdrop {
    pub commit: CommitCounter,
    sources: Vec<Source>,
    region: Option<Rectangle<i32, Physical>>,
}

#[derive(Clone, Debug)]
struct Source {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    src: Rectangle<f64, Buffer>,
    transform: Transform,
    visible: Vec<Rectangle<i32, Physical>>,
}

impl Backdrop {
    fn update<'a, E: Element + 'a>(
        &mut self,
        elements: impl IntoIterator<Item = &'a E>,
        region: Rectangle<i32, Physical>,
        scale: RenderScale<f64>,
    ) -> bool {
        let mut uncovered = vec![region];
        let mut sources = Vec::new();
        let mut changed = self.region != Some(region);

        for element in elements {
            let geometry = element.geometry(scale);
            let visible = uncovered
                .iter()
                .filter_map(|rect| rect.intersection(geometry))
                .collect::<Vec<_>>();
            if visible.is_empty() || element.alpha() == 0.0 {
                continue;
            }

            let previous = self
                .sources
                .iter()
                .find(|source| &source.id == element.id());
            changed |= previous.is_none_or(|source| {
                source.geometry != geometry
                    || source.alpha != element.alpha()
                    || source.src != element.src()
                    || source.transform != element.transform()
                    || source.visible != visible
            });
            if let Some(previous) = previous {
                changed |= element
                    .damage_since(scale, Some(previous.commit))
                    .iter()
                    .any(|damage| {
                        let damage = Rectangle::new(damage.loc + geometry.loc, damage.size);
                        visible.iter().any(|rect| rect.overlaps(damage))
                    });
            }

            sources.push(Source {
                id: element.id().clone(),
                commit: element.current_commit(),
                geometry,
                alpha: element.alpha(),
                src: element.src(),
                transform: element.transform(),
                visible,
            });
            if element.alpha() == 1.0 {
                for opaque in element.opaque_regions(scale).iter() {
                    let opaque = Rectangle::new(opaque.loc + geometry.loc, opaque.size);
                    uncovered = uncovered
                        .into_iter()
                        .flat_map(|rect| rect.subtract_rect(opaque))
                        .collect();
                }
            }
        }

        // Membership and stacking changes matter even without buffer damage.
        changed |= !self
            .sources
            .iter()
            .map(|source| &source.id)
            .eq(sources.iter().map(|source| &source.id));
        self.sources = sources;
        self.region = Some(region);
        if changed {
            self.commit.increment();
        }

        changed
    }
}

fn update_shared<E: Element>(
    shared: &std::sync::Mutex<Backdrop>,
    elements: &[E],
    owner: &Id,
    region: Rectangle<i32, Physical>,
    scale: RenderScale<f64>,
) -> bool {
    // Element queries can read this same backdrop's commit. Snapshot the state,
    // release its lock, then inspect sources. Rendering is the sole writer.
    let mut backdrop = shared
        .lock()
        .expect("surface rendering state must remain consistent across callbacks")
        .clone();
    let sources = elements.iter().filter(|element| element.id() != owner);
    let changed = backdrop.update(sources, region, scale);
    *shared
        .lock()
        .expect("surface rendering state must remain consistent across callbacks") = backdrop;
    changed
}

pub(super) fn update(elements: &[AnimatedWindowRenderElement], scale: f64) {
    // Resolve lower blurs first so their damage propagates to higher materials.
    for (index, element) in elements.iter().enumerate().rev() {
        if let AnimatedWindowRenderElement::Blur(blur) = element {
            let changed = update_shared(
                &blur.backdrop,
                &elements[index + 1..],
                blur.id(),
                blur.geometry(scale.into()),
                scale.into(),
            );
            if changed {
                blur.capture_dirty.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(test)]
    use smithay::backend::renderer::utils::DamageSet;
    #[cfg(test)]
    use smithay::backend::renderer::utils::OpaqueRegions;
    use std::sync::Arc;

    use super::*;

    struct TestElement {
        id: Id,
        commit: CommitCounter,
        rect: Rectangle<i32, Physical>,
        damage: Vec<Rectangle<i32, Physical>>,
        probe: Option<Arc<std::sync::Mutex<Backdrop>>>,
        opaque: bool,
        alpha: f32,
    }

    impl TestElement {
        fn new(rect: Rectangle<i32, Physical>) -> Self {
            Self {
                id: Id::new(),
                commit: CommitCounter::default(),
                rect,
                damage: vec![],
                probe: None,
                opaque: false,
                alpha: 1.0,
            }
        }

        fn repaint(&mut self, damage: Vec<Rectangle<i32, Physical>>) {
            self.commit.increment();
            self.damage = damage;
        }
    }

    impl Element for TestElement {
        fn id(&self) -> &Id {
            &self.id
        }
        fn current_commit(&self) -> CommitCounter {
            if let Some(backdrop) = &self.probe {
                let _guard = backdrop
                    .try_lock()
                    .expect("backdrop lock held during an element query");
            }
            self.commit
        }
        fn src(&self) -> Rectangle<f64, Buffer> {
            Rectangle::from_size((100.0, 100.0).into())
        }
        fn geometry(&self, _: RenderScale<f64>) -> Rectangle<i32, Physical> {
            self.rect
        }
        fn damage_since(
            &self,
            _: RenderScale<f64>,
            commit: Option<CommitCounter>,
        ) -> DamageSet<i32, Physical> {
            if commit == Some(self.commit) {
                DamageSet::default()
            } else {
                DamageSet::from_slice(&self.damage)
            }
        }
        fn opaque_regions(&self, _: RenderScale<f64>) -> OpaqueRegions<i32, Physical> {
            if self.opaque {
                OpaqueRegions::from_slice(&[Rectangle::from_size(self.rect.size)])
            } else {
                OpaqueRegions::default()
            }
        }
        fn alpha(&self) -> f32 {
            self.alpha
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn blurred_popup_open_update_close_releases_backdrop_before_element_queries() {
        let region = rect(0, 0, 100, 100);
        let backdrop = Arc::new(std::sync::Mutex::new(Backdrop::default()));
        let owner = Id::new();
        let mut panel = TestElement::new(region);
        panel.probe = Some(backdrop.clone());
        let mut popup_alias = TestElement::new(region);
        popup_alias.id = owner.clone();
        let mut elements = [popup_alias, panel];

        // Open: ignore a repeated instance of the cached popup material, and
        // allow dependencies to query backdrop state without a nested lock.
        assert!(update_shared(
            &backdrop,
            &elements,
            &owner,
            region,
            1.0.into()
        ));
        assert_eq!(backdrop.lock().unwrap().sources.len(), 1);
        assert!(!update_shared(
            &backdrop,
            &elements,
            &owner,
            region,
            1.0.into()
        ));

        // Update both content and bounds while the popup remains visible.
        elements[1].repaint(vec![rect(10, 10, 5, 5)]);
        assert!(update_shared(
            &backdrop,
            &elements,
            &owner,
            region,
            1.0.into()
        ));
        assert!(update_shared(
            &backdrop,
            &elements,
            &owner,
            rect(5, 5, 90, 90),
            1.0.into()
        ));

        // Close and reopen: dependencies disappear, then are tracked again.
        assert!(update_shared::<TestElement>(
            &backdrop,
            &[],
            &owner,
            region,
            1.0.into()
        ));
        assert!(backdrop.lock().unwrap().sources.is_empty());
        assert!(update_shared(
            &backdrop,
            &elements,
            &owner,
            region,
            1.0.into()
        ));
        assert!(!update_shared(
            &backdrop,
            &elements,
            &owner,
            region,
            1.0.into()
        ));
    }

    #[test]
    fn only_damage_intersecting_the_sample_invalidates_its_output() {
        let mut first = Backdrop::default();
        let mut second = Backdrop::default();
        let region = rect(20, 20, 30, 30);
        let mut a = TestElement::new(rect(0, 0, 100, 100));
        let b = TestElement::new(rect(0, 0, 100, 100));
        assert!(first.update(&[&a], region, 1.0.into()));
        assert!(second.update(&[&b], region, 1.0.into()));

        a.repaint(vec![]);
        assert!(!first.update(&[&a], region, 1.0.into()));
        a.repaint(vec![rect(60, 60, 10, 10)]);
        assert!(!first.update(&[&a], region, 1.0.into()));
        a.repaint(vec![rect(25, 25, 1, 1)]);
        assert!(first.update(&[&a], region, 1.0.into()));
        assert!(!second.update(&[&b], region, 1.0.into()));
        assert!(!first.update(&[&a], region, 1.0.into()));
    }

    #[test]
    fn opaque_occlusion_hides_damage_but_fading_and_moving_reveal_it() {
        let mut backdrop = Backdrop::default();
        let region = rect(0, 0, 100, 100);
        let mut front = TestElement::new(rect(0, 0, 50, 100));
        front.opaque = true;
        let mut back = TestElement::new(region);
        assert!(backdrop.update(&[&front, &back], region, 1.0.into()));

        back.repaint(vec![rect(10, 10, 10, 10)]);
        assert!(!backdrop.update(&[&front, &back], region, 1.0.into()));
        front.alpha = 0.5;
        assert!(backdrop.update(&[&front, &back], region, 1.0.into()));
        back.repaint(vec![rect(10, 10, 10, 10)]);
        assert!(backdrop.update(&[&front, &back], region, 1.0.into()));
        front.rect.loc.x = 20;
        assert!(backdrop.update(&[&front, &back], region, 1.0.into()));
        assert!(!backdrop.update(&[&front, &back], region, 1.0.into()));
    }

    #[test]
    fn membership_and_stacking_changes_invalidate_without_buffer_damage() {
        let mut backdrop = Backdrop::default();
        let region = rect(0, 0, 100, 100);
        let a = TestElement::new(region);
        let b = TestElement::new(region);
        assert!(backdrop.update(&[&a, &b], region, 1.0.into()));
        assert!(backdrop.update(&[&b, &a], region, 1.0.into()));
        assert!(backdrop.update(&[&a], region, 1.0.into()));
        assert!(backdrop.update(&[&a, &b], region, 1.0.into()));
        assert!(!backdrop.update(&[&a, &b], region, 1.0.into()));
    }
}
