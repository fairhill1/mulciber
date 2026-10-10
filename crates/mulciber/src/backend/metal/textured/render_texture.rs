//! Render textures: an `RGBA16Float` color texture sampled like any upload, with depth and
//! multisample storage of its own, rendered by offscreen passes each in an encoder of its own on
//! the frame's command buffer, after the shadow prepass and before the scene.
use super::{
    GraphicsError, LOAD_ACTION_CLEAR, Object, PIXEL_FORMAT_DEPTH32_FLOAT,
    PIXEL_FORMAT_RGBA16_FLOAT, ResourceId, STORE_ACTION_DONT_CARE,
    STORE_ACTION_MULTISAMPLE_RESOLVE, STORE_ACTION_STORE, SampledTextureFormat,
    TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ, TextureResource, TexturedSession,
    create_target_texture, create_upload_sampler, material_instances_len, material_storage_len,
    objc, ptr, required, transient_geometry_len,
};
use crate::OffscreenPass;

/// What a render texture owns beyond its sampled color.
pub(super) struct RenderAttachments {
    depth: Object,
    /// Multisample color resolved into the sampled color; null at one sample.
    multisample: Object,
    /// Samples per pixel the storage was built for; rendering at another count is refused.
    pub(super) sample_count: usize,
    /// Whether any offscreen pass has rendered into the texture; sampling before that is
    /// rejected.
    pub(super) rendered: bool,
}

impl RenderAttachments {
    pub(super) fn release(self) {
        unsafe {
            if !self.multisample.is_null() {
                objc::void(self.multisample, c"release");
            }
            objc::void(self.depth, c"release");
        }
    }
}

impl TexturedSession<'_> {
    pub(crate) fn create_render_texture(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<ResourceId, GraphicsError> {
        // The same exact-format request as RGBA16Float uploads: Metal 3 guarantees filtering.
        if !unsafe { objc::bool_usize(self.surface.device, c"supportsFamily:", 5001) } {
            return Err(GraphicsError::with_kind(
                crate::GraphicsErrorKind::Unsupported,
                "render textures require Metal 3 RGBA16Float linear filtering",
            ));
        }
        let (columns, rows) = (width as usize, height as usize);
        let device = self.surface.device;
        let sample_count = self.sample_count;
        let mut owned: [Object; 4] = [ptr::null_mut(); 4];
        let release = |owned: &[Object]| {
            for &object in owned.iter().filter(|object| !object.is_null()) {
                unsafe { objc::void(object, c"release") };
            }
        };
        let created = (|| {
            owned[0] = create_target_texture(
                device,
                PIXEL_FORMAT_RGBA16_FLOAT,
                columns,
                rows,
                1,
                TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
            )?;
            // Depth and multisample color live only inside the pass: memoryless when
            // multisampled, as for the presentable targets.
            owned[1] = create_target_texture(
                device,
                PIXEL_FORMAT_DEPTH32_FLOAT,
                columns,
                rows,
                sample_count,
                TEXTURE_USAGE_RENDER_TARGET,
            )?;
            if sample_count > 1 {
                owned[2] = create_target_texture(
                    device,
                    PIXEL_FORMAT_RGBA16_FLOAT,
                    columns,
                    rows,
                    sample_count,
                    TEXTURE_USAGE_RENDER_TARGET,
                )?;
            }
            owned[3] = create_upload_sampler(device)?;
            Ok::<(), GraphicsError>(())
        })();
        if let Err(failure) = created {
            release(&owned);
            return Err(failure);
        }
        let [texture, depth, multisample, sampler] = owned;
        self.textures
            .insert(TextureResource {
                texture,
                sampler,
                extent: [width, height],
                format: SampledTextureFormat::Float16,
                mip_levels: 1,
                slices: 1,
                generates_mips: false,
                pending: None,
                render: Some(RenderAttachments {
                    depth,
                    multisample,
                    sample_count,
                    rendered: false,
                }),
            })
            .inspect_err(|_| release(&owned))
    }

    /// Whether an offscreen pass has rendered into the texture.
    pub(crate) fn render_texture_rendered(&self, id: ResourceId) -> Result<bool, GraphicsError> {
        Ok(self
            .textures
            .get(id)?
            .render
            .as_ref()
            .is_some_and(|render| render.rendered))
    }

    /// Checks each pass's target against the session's sample count, before anything is staged.
    pub(super) fn check_offscreen_targets(
        &self,
        offscreen: &[OffscreenPass<'_>],
    ) -> Result<(), GraphicsError> {
        for pass in offscreen {
            let render = self
                .textures
                .get(pass.target.texture().id())?
                .render
                .as_ref()
                .ok_or_else(|| {
                    GraphicsError::new("offscreen pass target is not a render texture")
                })?;
            self.check_sample_count(render.sample_count, "render texture")?;
        }
        Ok(())
    }

    /// Marks each pass's target rendered once the frame that renders it is prepared.
    pub(super) fn mark_offscreen_rendered(
        &mut self,
        offscreen: &[OffscreenPass<'_>],
    ) -> Result<(), GraphicsError> {
        for pass in offscreen {
            let index = self.textures.index_of(pass.target.texture().id())?;
            if let Some(render) = self.textures[index].render.as_mut() {
                render.rendered = true;
            }
        }
        Ok(())
    }

    /// Encodes each offscreen pass in its own encoder, clearing its target's color and depth,
    /// resolving multisample color into the sampled texture. `base` holds the uniform slot and
    /// the storage, transient-geometry and instance offsets of the first pass's first record;
    /// the passes' records were staged in pass order from there.
    pub(super) unsafe fn encode_offscreen_passes(
        &self,
        command: Object,
        offscreen: &[OffscreenPass<'_>],
        depth_clears: &[f32],
        base: [usize; 4],
    ) -> Result<(), GraphicsError> {
        let [mut uniform, mut storage, mut transient, mut instances] = base;
        for (pass, &depth_clear) in offscreen.iter().zip(depth_clears) {
            let texture = self.textures.get(pass.target.texture().id())?;
            let render = texture.render.as_ref().ok_or_else(|| {
                GraphicsError::new("offscreen pass target is not a render texture")
            })?;
            unsafe {
                let descriptor = required(
                    objc::object(
                        objc::class(c"MTLRenderPassDescriptor"),
                        c"renderPassDescriptor",
                    ),
                    "Metal offscreen render-pass descriptor",
                )?;
                let attachments = required(
                    objc::object(descriptor, c"colorAttachments"),
                    "offscreen color attachments",
                )?;
                let color = required(
                    objc::object_usize(attachments, c"objectAtIndexedSubscript:", 0),
                    "offscreen color attachment zero",
                )?;
                if render.multisample.is_null() {
                    objc::void_object(color, c"setTexture:", texture.texture);
                    objc::void_usize(color, c"setStoreAction:", STORE_ACTION_STORE);
                } else {
                    objc::void_object(color, c"setTexture:", render.multisample);
                    objc::void_object(color, c"setResolveTexture:", texture.texture);
                    objc::void_usize(color, c"setStoreAction:", STORE_ACTION_MULTISAMPLE_RESOLVE);
                }
                objc::void_usize(color, c"setLoadAction:", LOAD_ACTION_CLEAR);
                let [red, green, blue, alpha] = pass.clear.components();
                objc::void_clear_color(
                    color,
                    c"setClearColor:",
                    objc::ClearColor {
                        red: f64::from(red),
                        green: f64::from(green),
                        blue: f64::from(blue),
                        alpha: f64::from(alpha),
                    },
                );
                let depth = required(
                    objc::object(descriptor, c"depthAttachment"),
                    "offscreen depth attachment",
                )?;
                objc::void_object(depth, c"setTexture:", render.depth);
                objc::void_usize(depth, c"setLoadAction:", LOAD_ACTION_CLEAR);
                objc::void_usize(depth, c"setStoreAction:", STORE_ACTION_DONT_CARE);
                objc::void_f64(depth, c"setClearDepth:", f64::from(depth_clear));
                let encoder = required(
                    objc::object_object(
                        command,
                        c"renderCommandEncoderWithDescriptor:",
                        descriptor,
                    ),
                    "Metal offscreen render encoder",
                )?;
                self.encode_material_records(
                    encoder,
                    pass.records,
                    uniform,
                    storage,
                    transient,
                    instances,
                    false,
                    ptr::null_mut(),
                )?;
                objc::void(encoder, c"endEncoding");
            }
            uniform += pass.records.len();
            storage += material_storage_len(pass.records);
            transient += transient_geometry_len(pass.records);
            instances += material_instances_len(pass.records);
        }
        Ok(())
    }
}
