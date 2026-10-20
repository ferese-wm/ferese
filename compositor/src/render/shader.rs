//! Static uniform names stay borrowed; cached frame elements share their state.
//! Copy on write preserves the ID and commit seen by an already sampled frame.
use std::sync::Arc;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

#[derive(Clone, Debug)]
pub(crate) struct SharedPixelShaderElement {
    shader: GlesPixelProgram,
    state: Arc<ShaderState>,
}

#[derive(Clone, Debug)]
struct ShaderState {
    id: Id,
    commit: CommitCounter,
    area: Rectangle<i32, Logical>,
    opaque: Vec<Rectangle<i32, Logical>>,
    alpha: f32,
    uniforms: Vec<Uniform<'static>>,
    kind: Kind,
}

impl SharedPixelShaderElement {
    pub(crate) fn new(
        shader: GlesPixelProgram,
        area: Rectangle<i32, Logical>,
        opaque: Option<Vec<Rectangle<i32, Logical>>>,
        alpha: f32,
        uniforms: Vec<Uniform<'static>>,
        kind: Kind,
    ) -> Self {
        Self {
            shader,
            state: Arc::new(ShaderState {
                id: Id::new(),
                commit: CommitCounter::default(),
                area,
                opaque: opaque.unwrap_or_default(),
                alpha,
                uniforms,
                kind,
            }),
        }
    }

    pub(crate) fn resize(&mut self, area: Rectangle<i32, Logical>, opaque: Option<Vec<Rectangle<i32, Logical>>>) {
        let opaque = opaque.unwrap_or_default();

        if self.state.area != area || self.state.opaque != opaque {
            let state = Arc::make_mut(&mut self.state);
            state.area = area;
            state.opaque = opaque;
            state.commit.increment();
        }
    }

    pub(crate) fn update_uniforms(&mut self, uniforms: Vec<Uniform<'static>>) {
        let state = Arc::make_mut(&mut self.state);
        state.uniforms = uniforms;
        state.commit.increment();
    }
}

impl Element for SharedPixelShaderElement {
    fn id(&self) -> &Id {
        &self.state.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.state.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.state.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.state.area.to_physical_precise_round(scale)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.state
            .opaque
            .iter()
            .map(|region| region.to_physical_precise_round(scale))
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.state.alpha
    }

    fn kind(&self) -> Kind {
        self.state.kind
    }
}

impl RenderElement<GlesRenderer> for SharedPixelShaderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_pixel_shader_to(
            &self.shader,
            src,
            dst,
            self.state.area.size.to_buffer(1, Transform::Normal),
            Some(damage),
            self.state.alpha,
            &self.state.uniforms,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::gles::element::PixelShaderElement;
    use smithay::backend::renderer::gles::{GlesTexture, UniformName, UniformType};
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};

    use super::*;

    fn pixels(renderer: &mut GlesRenderer, element: &impl RenderElement<GlesRenderer>) -> Vec<u8> {
        let size = (32, 32).into();
        let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size).unwrap();
        let rect = Rectangle::from_size((32, 32).into());
        let mut target = renderer.bind(&mut texture).unwrap();

        {
            let mut frame = renderer
                .render(&mut target, (32, 32).into(), Transform::Normal)
                .unwrap();
            frame.clear(Color32F::TRANSPARENT, &[rect]).unwrap();
            element
                .draw(&mut frame, element.src(), rect, &[rect], &[], None)
                .unwrap();
            let _ = frame.finish().unwrap();
        }

        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
            .unwrap();
        renderer.map_texture(&mapping).unwrap().to_vec()
    }

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn shared_shader_matches_smithay_and_keeps_old_frames_immutable() {
        let device = EGLDevice::enumerate().unwrap().last().expect("an EGL device");
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let mut renderer = unsafe { GlesRenderer::new(EGLContext::new(&display).unwrap()).unwrap() };
        let program = renderer
            .compile_custom_pixel_shader(
                super::super::corner_shader(super::super::ROUNDED_SOLID_SHADER),
                &[
                    UniformName::new("clip_rect", UniformType::_4f),
                    UniformName::new("radius", UniformType::_1f),
                    UniformName::new("color", UniformType::_4f),
                ],
            )
            .unwrap();
        let area = Rectangle::new((2, 3).into(), (32, 32).into());
        let uniforms = |color| {
            vec![
                Uniform::new("clip_rect", [0.0_f32, 0.0, 32.0, 32.0]),
                Uniform::new("radius", 8.0_f32),
                Uniform::new("color", color),
            ]
        };
        let red = [0.5_f32, 0.0, 0.0, 0.5];
        let green = [0.0_f32, 0.5, 0.0, 0.5];
        let opaque = Some(vec![Rectangle::from_size((20, 20).into())]);
        let baseline = PixelShaderElement::new(
            program.clone(),
            area,
            opaque.clone(),
            0.5,
            uniforms(red),
            Kind::Unspecified,
        );
        let mut shared = SharedPixelShaderElement::new(program, area, opaque, 0.5, uniforms(red), Kind::Unspecified);
        let old = shared.clone();
        let old_commit = old.current_commit();

        assert!(Arc::ptr_eq(&shared.state, &old.state));
        assert!(
            shared
                .state
                .uniforms
                .iter()
                .all(|uniform| matches!(uniform.name, Cow::Borrowed(_)))
        );

        for scale in [1.0, 1.25, 2.0] {
            assert_eq!(shared.geometry(scale.into()), baseline.geometry(scale.into()));
            assert_eq!(
                &*shared.opaque_regions(scale.into()),
                &*baseline.opaque_regions(scale.into())
            );
        }

        assert_eq!(pixels(&mut renderer, &shared), pixels(&mut renderer, &baseline));
        shared.resize(area, Some(vec![Rectangle::from_size((20, 20).into())]));
        assert!(Arc::ptr_eq(&shared.state, &old.state));
        shared.update_uniforms(uniforms(green));
        assert!(!Arc::ptr_eq(&shared.state, &old.state));
        assert_eq!(shared.id(), old.id());
        assert_eq!(old.current_commit(), old_commit);
        assert_ne!(shared.current_commit(), old_commit);
        assert_eq!(pixels(&mut renderer, &old), pixels(&mut renderer, &baseline));
        assert_ne!(pixels(&mut renderer, &shared), pixels(&mut renderer, &baseline));

        let retained = shared.clone();
        shared.resize(Rectangle::from_size((16, 16).into()), None);
        assert_eq!(retained.geometry(1.0.into()), area.to_physical_precise_round(1.0));
        assert_eq!(shared.geometry(1.0.into()).size, (16, 16).into());
        assert!(shared.opaque_regions(1.0.into()).is_empty());
        assert_eq!(shared.alpha(), baseline.alpha());
        assert_eq!(shared.kind(), baseline.kind());
    }
}
