mod capture;
mod cube_texture;
mod hdr;
mod ktx2;
mod sampled_texture;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use sampled_texture::checked_staging_size;
mod scene_depth;
pub use capture::FrameCapture;
pub(crate) use capture::{CaptureByteOrder, capture_byte_len, frame_capture_from_native};
pub use hdr::BLOOM_SMALLEST;
pub(crate) use hdr::{BLOOM_LEVELS, bloom_extents};
use hdr::{validate_bloom_filter_interface, validate_hdr_pair};
pub use ktx2::{Ktx2Texture, ktx2_vk_format};
use scene_depth::validate_scene_depth_order;

use core::cell::RefCell;
use std::format;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::vec::Vec;

use mulciber_platform::{SurfaceTarget, WindowMetrics};

use crate::backend;
use crate::resource::{DestroyRequest, DropQueue, ResourceId, ResourceKind, ResourceLease};
use crate::shader;
use crate::{
    ClearColor, FrameAcquire, FrameDisposition, GraphicsError, GraphicsErrorKind, ShaderArtifact,
    SurfaceExtent, SurfaceInfo,
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
/// Maximum lazy resource handles reclaimed at one completed-frame boundary. A mesh and all of its
/// immutable index parts are one handle and one parent allocation/reclamation unit.
const LAZY_RECLAIM_BUDGET: usize = 8;

/// Multisample count supported by the first textured rendering slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleCount {
    /// One sample per pixel.
    One,
    /// Two samples per pixel.
    Two,
    /// Four samples per pixel.
    Four,
}

impl SampleCount {
    /// Samples per pixel as the number the native APIs take.
    #[must_use]
    pub const fn samples(self) -> u32 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }

    /// The count for a native sample number, which is one of the three or nothing.
    #[must_use]
    pub const fn from_samples(samples: u32) -> Option<Self> {
        match samples {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            4 => Some(Self::Four),
            _ => None,
        }
    }

    /// Whether the scene is rendered into a multisample target that is resolved.
    #[must_use]
    pub const fn is_multisampled(self) -> bool {
        !matches!(self, Self::One)
    }
}

/// Preferences used while selecting a surface-compatible graphics device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceRequest {
    /// Preferred sample count. A multisample count the device cannot render falls back
    /// observably to one sample per pixel.
    pub preferred_sample_count: SampleCount,
}

impl Default for DeviceRequest {
    fn default() -> Self {
        Self {
            preferred_sample_count: SampleCount::Four,
        }
    }
}

/// Granularity of GPU duration diagnostics exposed by the selected backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingSupport {
    /// The selected queue cannot produce GPU duration timestamps.
    Unsupported,
    /// Only the complete submitted command-buffer duration is available.
    Frame,
    /// The backend can measure the fixed rendering regions inside the submitted frame.
    Regions,
}

/// Native choices made while opening graphics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSelection {
    backend: &'static str,
    sample_count: SampleCount,
    gpu_timing: GpuTimingSupport,
}

impl DeviceSelection {
    /// Native backend selected for this compilation target.
    #[must_use]
    pub const fn backend(&self) -> &'static str {
        self.backend
    }

    /// Sample count chosen when the session opened, including a visible fallback from a
    /// multisample count to one. [`Device::set_sample_count`] changes the count in use later
    /// and returns the new one; this selection is not updated.
    #[must_use]
    pub const fn sample_count(&self) -> SampleCount {
        self.sample_count
    }

    /// GPU duration timing supported by the selected queue, independently of whether collection
    /// was enabled in [`DeviceRequest`].
    #[must_use]
    pub const fn gpu_timing_support(&self) -> GpuTimingSupport {
        self.gpu_timing
    }
}

struct Shared<'window> {
    id: u64,
    inner: Rc<RefCell<Option<backend::TexturedSession<'window>>>>,
    drops: Rc<DropQueue>,
}

impl Clone for Shared<'_> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            inner: Rc::clone(&self.inner),
            drops: Rc::clone(&self.drops),
        }
    }
}

/// The distinct device, queue, and surface owners opened for one native graphics session.
pub struct OpenedGraphics<'window> {
    /// Resource creation owner.
    pub device: Device<'window>,
    /// Submission owner.
    pub queue: Queue<'window>,
    /// Presentation owner.
    pub surface: Surface<'window>,
    /// Observable native selection.
    pub selection: DeviceSelection,
}

impl<'window> OpenedGraphics<'window> {
    /// Opens the target-selected native backend and produces distinct logical owners.
    ///
    /// # Errors
    ///
    /// Returns an error when explicitly enabled validation is unavailable, no surface-compatible device exists, or
    /// native device and presentation setup fails.
    pub fn open(
        target: SurfaceTarget<'window>,
        metrics: WindowMetrics,
        request: DeviceRequest,
    ) -> Result<Self, GraphicsError> {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(GraphicsError::internal(
                "graphics session identity space is exhausted",
            ));
        }
        let (session, sample_count) = backend::TexturedSession::new(target, metrics, request)?;
        let gpu_timing = session.gpu_timing_support();
        let shared = Shared {
            id,
            inner: Rc::new(RefCell::new(Some(session))),
            drops: Rc::new(DropQueue::default()),
        };
        Ok(Self {
            device: Device {
                shared: shared.clone(),
            },
            queue: Queue {
                shared: shared.clone(),
            },
            surface: Surface { shared },
            selection: DeviceSelection {
                backend: backend::BACKEND_NAME,
                sample_count,
                gpu_timing,
            },
        })
    }

    /// Drains GPU and presentation ownership and destroys the native session.
    ///
    /// All acquired frames must already be presented or abandoned. Resource handles may remain;
    /// they become inert identifiers after shutdown.
    ///
    /// # Errors
    ///
    /// Returns a deferred frame error or native completion, validation, or destruction failure.
    pub fn shutdown(self) -> Result<(), GraphicsError> {
        let Self {
            device,
            queue,
            surface,
            selection: _,
        } = self;
        if Rc::strong_count(&surface.shared.inner) != 3 {
            return Err(GraphicsError::lifecycle(
                "cannot shut graphics down while an acquired frame is live",
            ));
        }
        drop(device);
        drop(queue);
        let mut session = surface
            .shared
            .inner
            .borrow_mut()
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("graphics session is already shut down"))?;
        let pending = surface.shared.drops.take_bounded(usize::MAX);
        session.reclaim_resources(&pending)?;
        session.shutdown()
    }
}

/// Resource creation owner for one native session.
pub struct Device<'window> {
    shared: Shared<'window>,
}

impl Device<'_> {
    /// Uploads fixed-layout vertices and one default `u16` index part.
    ///
    /// Meshes that need 32-bit indices declare their layout and pass [`MeshIndices::U32`]
    /// through [`Device::create_mesh_with_layout`].
    ///
    /// # Errors
    ///
    /// Returns an error for empty geometry, an out-of-range index, or native allocation/upload
    /// failure.
    pub fn create_mesh(&self, vertices: &[Vertex], indices: &[u16]) -> Result<Mesh, GraphicsError> {
        self.create_mesh_with_parts(vertices, &[MeshIndices::U16(indices)])
    }

    /// Uploads fixed-layout vertices once and one or more immutable indexed parts.
    ///
    /// Every part references the same vertex storage. Part zero is the default selected by
    /// existing whole-mesh draw paths; use [`Mesh::part`] with a material or shadow record to
    /// select another part.
    ///
    /// # Errors
    ///
    /// Returns an error for empty vertices, no parts, an empty part, an out-of-range index, too
    /// many parts, or native allocation/upload failure. Diagnostics identify the invalid part.
    pub fn create_mesh_with_parts(
        &self,
        vertices: &[Vertex],
        parts: &[MeshIndices<'_>],
    ) -> Result<Mesh, GraphicsError> {
        if vertices.is_empty() {
            return Err(GraphicsError::invalid_request(
                "mesh vertex data must be non-empty",
            ));
        }
        let part_count = validate_mesh_parts(vertices.len(), parts)?;
        let id = session_mut(&self.shared)?.create_mesh(vertices, parts)?;
        Ok(Mesh {
            lease: self.lease(id, ResourceKind::Mesh),
            layout: VertexLayout::VERTEX.to_owned_layout(),
            part_count,
        })
    }

    /// Uploads indexed geometry from raw vertex bytes against a declared layout.
    ///
    /// The layout is retained with the mesh: a material draw whose mesh and pipeline layouts
    /// differ is rejected at submission.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, vertex bytes that are empty or not a multiple of
    /// the stride, empty indices, an out-of-range index, or native allocation/upload failure.
    pub fn create_mesh_with_layout(
        &self,
        layout: VertexLayout<'_>,
        vertices: &[u8],
        indices: MeshIndices<'_>,
    ) -> Result<Mesh, GraphicsError> {
        self.create_mesh_with_layout_and_parts(layout, vertices, &[indices])
    }

    /// Uploads raw vertex bytes once and one or more immutable indexed parts against a declared
    /// layout.
    ///
    /// Each part may independently use 16- or 32-bit indices. The layout and graphics-session
    /// identity belong to the parent mesh and therefore apply to every borrowed [`MeshPart`]. Part
    /// zero is the default selected by [`GeometrySource::Mesh`] and the compatibility draw APIs.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, vertex bytes that are empty or not a multiple of
    /// the stride, no parts, an empty part, an out-of-range index, too many parts, or native
    /// allocation/upload failure. Diagnostics identify the invalid part.
    pub fn create_mesh_with_layout_and_parts(
        &self,
        layout: VertexLayout<'_>,
        vertices: &[u8],
        parts: &[MeshIndices<'_>],
    ) -> Result<Mesh, GraphicsError> {
        let owned = validate_vertex_layout(layout)?;
        let stride = usize::try_from(layout.stride).map_err(|_| {
            GraphicsError::invalid_request("vertex layout stride exceeds this target")
        })?;
        if vertices.is_empty() || !vertices.len().is_multiple_of(stride) {
            return Err(GraphicsError::invalid_request(
                "mesh vertex bytes must be a non-zero multiple of the layout stride",
            ));
        }
        let vertex_count = vertices.len() / stride;
        let part_count = validate_mesh_parts(vertex_count, parts)?;
        let id = session_mut(&self.shared)?.create_mesh_from_bytes(vertices, parts)?;
        Ok(Mesh {
            lease: self.lease(id, ResourceKind::Mesh),
            layout: owned,
            part_count,
        })
    }

    /// Uploads a tightly packed RGBA8 sRGB texture.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a mismatched byte count, overflow, or native upload
    /// failure.
    pub fn create_rgba8_srgb_texture(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture(width, height, texels, SampledTextureFormat::Srgb)
    }

    /// Uploads a tightly packed RGBA8 sRGB texture with an application-supplied mip chain.
    ///
    /// `levels[0]` holds the base image; each following level halves both dimensions (flooring at
    /// one texel), and the chain must run to its final 1×1 level. The application owns mip
    /// content, including its downsampling filter and color-space handling.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a chain that does not run from the base level to
    /// 1×1, a level whose byte count does not match its dimensions, overflow, or native upload
    /// failure.
    pub fn create_rgba8_srgb_texture_with_mips(
        &self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture_with_mips(width, height, levels, SampledTextureFormat::Srgb)
    }

    /// Uploads a tightly packed RGBA8 UNORM texture without sRGB transfer-function decoding.
    ///
    /// This format is suitable for linearly interpreted data such as tangent-space normal maps.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a mismatched byte count, overflow, or native upload
    /// failure.
    pub fn create_rgba8_unorm_texture(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture(width, height, texels, SampledTextureFormat::Unorm)
    }

    /// Uploads a tightly packed RGBA8 UNORM texture with an application-supplied mip chain.
    ///
    /// `levels[0]` holds the base image; each following level halves both dimensions (flooring at
    /// one texel), and the chain must run to its final 1×1 level. The application owns mip
    /// content, including its downsampling filter. Sampling does not apply the sRGB transfer
    /// function, making this format suitable for linearly interpreted data such as normal maps.
    ///
    /// # Errors
    ///
    /// Returns an error for empty dimensions, a chain that does not run from the base level to
    /// 1×1, a level whose byte count does not match its dimensions, overflow, or native upload
    /// failure.
    pub fn create_rgba8_unorm_texture_with_mips(
        &self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture_with_mips(width, height, levels, SampledTextureFormat::Unorm)
    }

    /// Uploads one level of an already block-compressed texture.
    ///
    /// `blocks` holds the base level's 4×4 blocks in row-major order, tightly packed, exactly
    /// as an encoder writes them. Mulciber never encodes or decodes: it uploads the blocks as
    /// they are and the GPU samples them directly, so the payload is a quarter of its RGBA8
    /// equivalent in video memory as well as on disk. Dimensions need not be multiples of four;
    /// a partial edge block still carries sixteen bytes.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the adapter cannot sample the encoding, and `InvalidRequest`
    /// for empty dimensions, a byte count that is not the level's block count times the block
    /// size, or overflow.
    pub fn create_block_compressed_texture(
        &self,
        compression: BlockCompression,
        width: u32,
        height: u32,
        blocks: &[u8],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture(width, height, blocks, compression.sampled())
    }

    /// Uploads a block-compressed texture with an application-supplied mip chain.
    ///
    /// `levels[0]` holds the base level's blocks; each following level halves both dimensions
    /// (flooring at one texel) and holds the blocks covering that extent, so the 2×2 and 1×1
    /// tail levels are each one sixteen-byte block. The chain must run to 1×1. The application
    /// owns mip content: it filters the RGBA8 chain first and encodes every level, because a
    /// block cannot be downsampled without decoding it.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the adapter cannot sample the encoding, and `InvalidRequest`
    /// for empty dimensions, a chain that does not run from the base level to 1×1, a level
    /// whose byte count does not match its block count, or overflow.
    pub fn create_block_compressed_texture_with_mips(
        &self,
        compression: BlockCompression,
        width: u32,
        height: u32,
        levels: &[&[u8]],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture_with_mips(width, height, levels, compression.sampled())
    }

    /// Uploads a block-compressed texture from a parsed KTX 2.0 file: its base level alone, or the
    /// complete chain it stores, as [`create_block_compressed_texture`] and
    /// [`create_block_compressed_texture_with_mips`] would. The blocks go to the GPU as they are
    /// stored; nothing is decoded or transcoded.
    ///
    /// [`create_block_compressed_texture`]: Self::create_block_compressed_texture
    /// [`create_block_compressed_texture_with_mips`]: Self::create_block_compressed_texture_with_mips
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the adapter cannot sample the file's encoding, and the errors of
    /// the block-compressed uploads otherwise.
    pub fn create_ktx2_texture(&self, texture: &Ktx2Texture<'_>) -> Result<Texture, GraphicsError> {
        let levels = texture.levels();
        if let [base] = levels {
            return self.create_block_compressed_texture(
                texture.compression(),
                texture.width(),
                texture.height(),
                base,
            );
        }
        self.create_block_compressed_texture_with_mips(
            texture.compression(),
            texture.width(),
            texture.height(),
            levels,
        )
    }

    /// Uploads linear `RGBA16Float` data from row-major RGBA f32 texels (eight GPU bytes/texel).
    ///
    /// Conversion rounds to IEEE binary16, nearest with ties to even. Signed zero and half
    /// subnormals are preserved in uploaded storage; values below half precision underflow to
    /// signed zero. GPU sampling/arithmetic may flush subnormals on some hardware. No color
    /// transform or 0–1 clamp is applied. NaN, infinity, and magnitudes above 65504 are rejected.
    /// The texture has only level zero; explicit LOD sampling clamps to that level.
    /// Bind using `MaterialBinding::Texture` and WGSL `texture_2d<f32>` in either shader stage.
    ///
    /// # Errors
    /// Returns an error for zero dimensions, mismatched texel counts, size overflow, invalid
    /// components, allocation/upload failure, or unsupported linearly filterable native storage.
    pub fn create_rgba16_float_texture(
        &self,
        width: u32,
        height: u32,
        texels: &[[f32; 4]],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float(width, height, &[texels], false)
    }

    /// Uploads a complete application-authored `RGBA16Float` mip chain.
    ///
    /// Level zero has the supplied dimensions; subsequent levels halve each axis, flooring at
    /// one, through 1×1. No mip generation or color transform is performed. Component conversion
    /// and errors follow [`Self::create_rgba16_float_texture`]. Use a material sampler with
    /// linear mip filtering to interpolate explicit fractional LODs.
    ///
    /// # Errors
    /// Also rejects incomplete/extra mip levels and mismatched per-level texel counts.
    pub fn create_rgba16_float_texture_with_mips(
        &self,
        width: u32,
        height: u32,
        levels: &[&[[f32; 4]]],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float(width, height, levels, true)
    }

    /// Uploads linear `RGBA16Float` data from IEEE binary16 bit patterns, four per texel in
    /// RGBA order, as the `half` crate's `f16::to_bits` gives them.
    ///
    /// The bits upload unchanged, with no conversion pass, so data already in half precision
    /// (an HDR image decoded to `f16`, say) costs one copy. Infinity and NaN (an all-ones
    /// exponent) are rejected, as [`Self::create_rgba16_float_texture`] rejects them. Otherwise
    /// it behaves as that function.
    ///
    /// # Errors
    /// Returns an error for zero dimensions, mismatched texel counts, size overflow, a non-finite
    /// component, allocation/upload failure, or unsupported linearly filterable native storage.
    pub fn create_rgba16_float_texture_from_bits(
        &self,
        width: u32,
        height: u32,
        texels: &[[u16; 4]],
    ) -> Result<Texture, GraphicsError> {
        let packed = sampled_texture::pack_half_levels(width, height, &[texels], false)?;
        self.upload_packed_float(width, height, &packed, false)
    }

    /// Uploads a complete application-authored `RGBA16Float` mip chain of binary16 bit patterns,
    /// laid out as [`Self::create_rgba16_float_texture_with_mips`] takes it.
    ///
    /// # Errors
    /// Reports the errors of [`Self::create_rgba16_float_texture_from_bits`], plus incomplete or
    /// extra mip levels and mismatched per-level texel counts.
    pub fn create_rgba16_float_texture_with_mips_from_bits(
        &self,
        width: u32,
        height: u32,
        levels: &[&[[u16; 4]]],
    ) -> Result<Texture, GraphicsError> {
        let packed = sampled_texture::pack_half_levels(width, height, levels, true)?;
        self.upload_packed_float(width, height, &packed, false)
    }

    /// Uploads level 0 of an `RGBA16Float` texture and generates the rest of its full mip chain
    /// on the GPU.
    ///
    /// Each level is a linear-filtered half-size copy of the one above (a 2×2 box filter for
    /// even extents, as Vulkan blits and Metal's `generateMipmapsForTexture:` compute it),
    /// through 1×1. Values and errors follow [`Self::create_rgba16_float_texture`]; the texture
    /// can be replaced with [`Self::update_rgba16_float_texture_with_generated_mips`] or
    /// [`Self::update_rgba16_float_texture_with_mips`]. Sample it with a material sampler whose
    /// filter interpolates between mips.
    ///
    /// # Errors
    /// Reports the errors of [`Self::create_rgba16_float_texture`], and `Unsupported` when the
    /// adapter cannot blit the format with linear filtering.
    pub fn create_rgba16_float_texture_with_generated_mips(
        &self,
        width: u32,
        height: u32,
        texels: &[[f32; 4]],
    ) -> Result<Texture, GraphicsError> {
        let packed = sampled_texture::pack_float_levels(width, height, &[texels], false)?;
        self.upload_packed_float(width, height, &packed, true)
    }

    /// [`Self::create_rgba16_float_texture_with_generated_mips`] from binary16 bit patterns, as
    /// [`Self::create_rgba16_float_texture_from_bits`] takes them.
    ///
    /// # Errors
    /// Reports the errors of [`Self::create_rgba16_float_texture_from_bits`], and `Unsupported`
    /// when the adapter cannot blit the format with linear filtering.
    pub fn create_rgba16_float_texture_with_generated_mips_from_bits(
        &self,
        width: u32,
        height: u32,
        texels: &[[u16; 4]],
    ) -> Result<Texture, GraphicsError> {
        let packed = sampled_texture::pack_half_levels(width, height, &[texels], false)?;
        self.upload_packed_float(width, height, &packed, true)
    }

    /// Uploads a tightly packed RGBA8 sRGB base level and generates the rest of its full mip
    /// chain on the GPU.
    ///
    /// Filtering happens on linear values: the GPU decodes sRGB before averaging and encodes the
    /// result, as an sRGB blit or Metal mip generation does.
    ///
    /// # Errors
    /// Returns an error for empty dimensions, a mismatched byte count, overflow, `Unsupported`
    /// when the adapter cannot blit the format with linear filtering, or native upload failure.
    pub fn create_rgba8_srgb_texture_with_generated_mips(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture_with_generated_mips(
            width,
            height,
            texels,
            SampledTextureFormat::Srgb,
        )
    }

    /// Uploads a tightly packed RGBA8 UNORM base level and generates the rest of its full mip
    /// chain on the GPU, averaging the stored values directly.
    ///
    /// # Errors
    /// Returns an error for empty dimensions, a mismatched byte count, overflow, `Unsupported`
    /// when the adapter cannot blit the format with linear filtering, or native upload failure.
    pub fn create_rgba8_unorm_texture_with_generated_mips(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
    ) -> Result<Texture, GraphicsError> {
        self.create_rgba8_texture_with_generated_mips(
            width,
            height,
            texels,
            SampledTextureFormat::Unorm,
        )
    }

    fn create_rgba8_texture_with_generated_mips(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
        format: SampledTextureFormat,
    ) -> Result<Texture, GraphicsError> {
        validate_mip_level(format, width, height, 0, texels)?;
        let id = session_mut(&self.shared)?
            .create_texture_with_generated_mips(width, height, texels, format)?;
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::D2,
        })
    }

    /// Uploads a tightly packed RGBA8 sRGB cube texture of six `size`×`size` faces.
    ///
    /// `faces` is in the standard cube layer order +X, -X, +Y, -Y, +Z, -Z, each face's texels
    /// row-major as for [`Self::create_rgba8_srgb_texture`]. The texture has only level zero
    /// and binds through [`MaterialBinding::CubeTexture`] and WGSL `texture_cube<f32>`.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, a face whose byte count does not match it (naming
    /// the face), overflow, or native upload failure.
    pub fn create_rgba8_srgb_cube_texture(
        &self,
        size: u32,
        faces: [&[u8]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(
            size,
            single_level_faces(&faces),
            SampledTextureFormat::Srgb,
            false,
        )
    }

    /// Uploads an RGBA8 sRGB cube texture with an application-supplied mip chain per face.
    ///
    /// Each of the six faces (+X, -X, +Y, -Y, +Z, -Z) supplies the complete chain from its
    /// `size`×`size` base to 1×1, halving each level as
    /// [`Self::create_rgba8_srgb_texture_with_mips`] does. The application owns mip content,
    /// including any filtering across face edges.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, a face whose chain does not run from the base level
    /// to 1×1, a level whose byte count does not match its extent (naming the face and level),
    /// overflow, or native upload failure.
    pub fn create_rgba8_srgb_cube_texture_with_mips(
        &self,
        size: u32,
        faces: [&[&[u8]]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(size, faces, SampledTextureFormat::Srgb, true)
    }

    /// Uploads a tightly packed RGBA8 UNORM cube texture of six `size`×`size` faces, sampled
    /// without sRGB transfer-function decoding.
    ///
    /// Face order and binding follow [`Self::create_rgba8_srgb_cube_texture`].
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, a face whose byte count does not match it (naming
    /// the face), overflow, or native upload failure.
    pub fn create_rgba8_unorm_cube_texture(
        &self,
        size: u32,
        faces: [&[u8]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(
            size,
            single_level_faces(&faces),
            SampledTextureFormat::Unorm,
            false,
        )
    }

    /// Uploads an RGBA8 UNORM cube texture with an application-supplied mip chain per face.
    ///
    /// Face order and chain rules follow [`Self::create_rgba8_srgb_cube_texture_with_mips`].
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, a face whose chain does not run from the base level
    /// to 1×1, a level whose byte count does not match its extent (naming the face and level),
    /// overflow, or native upload failure.
    pub fn create_rgba8_unorm_cube_texture_with_mips(
        &self,
        size: u32,
        faces: [&[&[u8]]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(size, faces, SampledTextureFormat::Unorm, true)
    }

    /// Uploads one level of an already block-compressed cube texture.
    ///
    /// Each of the six faces (+X, -X, +Y, -Y, +Z, -Z) holds its `size`×`size` level as
    /// [`Self::create_block_compressed_texture`] takes a 2D level: whole 4×4 blocks, row-major
    /// and tightly packed, never encoded or decoded by Mulciber.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the adapter cannot sample the encoding, and `InvalidRequest`
    /// for a zero extent, a face whose byte count is not its block count times the block size
    /// (naming the face), or overflow.
    pub fn create_block_compressed_cube_texture(
        &self,
        compression: BlockCompression,
        size: u32,
        faces: [&[u8]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(
            size,
            single_level_faces(&faces),
            compression.sampled(),
            false,
        )
    }

    /// Uploads a block-compressed cube texture with an application-supplied mip chain per face.
    ///
    /// Each face supplies the complete chain to 1×1 as
    /// [`Self::create_block_compressed_texture_with_mips`] takes a 2D chain, so every face's
    /// 2×2 and 1×1 tail levels are each one block.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the adapter cannot sample the encoding, and `InvalidRequest`
    /// for a zero extent, a face whose chain does not run from the base level to 1×1, a level
    /// whose byte count does not match its block count (naming the face and level), or
    /// overflow.
    pub fn create_block_compressed_cube_texture_with_mips(
        &self,
        compression: BlockCompression,
        size: u32,
        faces: [&[&[u8]]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.create_cube_texture(size, faces, compression.sampled(), true)
    }

    /// Uploads a linear `RGBA16Float` cube texture of six `size`×`size` faces from row-major
    /// RGBA f32 texels.
    ///
    /// Face order follows [`Self::create_rgba8_srgb_cube_texture`]; component conversion and
    /// limits follow [`Self::create_rgba16_float_texture`]. The texture has only level zero and
    /// cannot be replaced with [`Self::update_rgba16_float_texture`].
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, a face whose texel count does not match it, invalid
    /// components (each naming the face), size overflow, allocation/upload failure, or
    /// unsupported linearly filterable native storage.
    pub fn create_rgba16_float_cube_texture(
        &self,
        size: u32,
        faces: [&[[f32; 4]]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float_cube(size, single_level_faces(&faces), false)
    }

    /// Uploads a linear `RGBA16Float` cube texture with an application-authored mip chain per
    /// face.
    ///
    /// Each face (+X, -X, +Y, -Y, +Z, -Z) supplies the complete chain from `size`×`size` to
    /// 1×1, as [`Self::create_rgba16_float_texture_with_mips`] takes a 2D chain.
    ///
    /// # Errors
    ///
    /// Also rejects incomplete/extra mip levels and mismatched per-level texel counts, naming
    /// the face.
    pub fn create_rgba16_float_cube_texture_with_mips(
        &self,
        size: u32,
        faces: [&[&[[f32; 4]]]; 6],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float_cube(size, faces, true)
    }

    fn upload_rgba16_float_cube(
        &self,
        size: u32,
        faces: [&[&[[f32; 4]]]; 6],
        complete: bool,
    ) -> Result<Texture, GraphicsError> {
        let mut packed: [Vec<Vec<u8>>; 6] = Default::default();
        for ((name, levels), packed) in cube_texture::FACE_NAMES.iter().zip(faces).zip(&mut packed)
        {
            *packed = sampled_texture::pack_float_levels(size, size, levels, complete)
                .map_err(|error| cube_texture::name_face(name, &error))?;
        }
        let slices: [Vec<&[u8]>; 6] = packed
            .each_ref()
            .map(|levels| levels.iter().map(Vec::as_slice).collect());
        self.create_cube_texture(
            size,
            slices.each_ref().map(Vec::as_slice),
            SampledTextureFormat::Float16,
            complete,
        )
    }

    fn create_cube_texture(
        &self,
        size: u32,
        faces: [&[&[u8]]; 6],
        format: SampledTextureFormat,
        complete: bool,
    ) -> Result<Texture, GraphicsError> {
        cube_texture::validate_faces(format, size, &faces, complete)?;
        let id = session_mut(&self.shared)?.create_cube_texture(size, &faces, format)?;
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::Cube,
        })
    }

    /// Uploads a linear `RGBA16Float` cube texture array: one or more cubes of one `size`, each
    /// with an application-authored mip chain per face, sampled as a whole through
    /// [`MaterialBinding::CubeTextureArray`] and WGSL `texture_cube_array<f32>`.
    ///
    /// `layers[i]` is cube `i`, which the shader selects with the array index of
    /// `textureSampleLevel(map, sampler, direction, i, lod)`. Each cube's six faces are in the
    /// order and orientation of [`Self::create_rgba16_float_cube_texture`], and every face supplies
    /// the complete chain from `size`×`size` to 1×1, as
    /// [`Self::create_rgba16_float_cube_texture_with_mips`] takes it. Component conversion and
    /// limits follow [`Self::create_rgba16_float_texture`]. The texture cannot be replaced with
    /// the float texture updates.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` when the device cannot sample cube arrays (Vulkan's
    /// `imageCubeArray` feature, Metal's Metal 3 family), cannot allocate `6 × layers.len()`
    /// array layers at this extent, or has no linearly filterable `RGBA16Float` storage; and
    /// `InvalidRequest` for no layers, a zero extent, or a face whose chain is incomplete, whose
    /// texel count does not match its extent or whose components are invalid, naming the layer
    /// and face, or for staging-size overflow.
    pub fn create_rgba16_float_cube_array_texture_with_mips(
        &self,
        size: u32,
        layers: &[[&[&[[f32; 4]]]; 6]],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float_cube_array(size, layers, |levels| {
            sampled_texture::pack_float_levels(size, size, levels, true)
        })
    }

    /// [`Self::create_rgba16_float_cube_array_texture_with_mips`] from binary16 bit patterns, as
    /// [`Self::create_rgba16_float_texture_from_bits`] takes them.
    ///
    /// # Errors
    ///
    /// Reports the errors of [`Self::create_rgba16_float_cube_array_texture_with_mips`], with
    /// non-finite bits as the invalid components.
    pub fn create_rgba16_float_cube_array_texture_with_mips_from_bits(
        &self,
        size: u32,
        layers: &[[&[&[[u16; 4]]]; 6]],
    ) -> Result<Texture, GraphicsError> {
        self.upload_rgba16_float_cube_array(size, layers, |levels| {
            sampled_texture::pack_half_levels(size, size, levels, true)
        })
    }

    fn upload_rgba16_float_cube_array<T>(
        &self,
        size: u32,
        layers: &[[&[&[T]]; 6]],
        pack: impl Fn(&[&[T]]) -> Result<Vec<Vec<u8>>, GraphicsError>,
    ) -> Result<Texture, GraphicsError> {
        cube_texture::require_layers(size, layers.len())?;
        // Refuse before packing on a device that could never sample the result.
        session_ref(&self.shared)?.require_cube_arrays()?;
        let packed = cube_texture::pack_layers(layers, pack)?;
        let slices: Vec<[Vec<&[u8]>; 6]> = packed
            .iter()
            .map(|cube| {
                cube.each_ref()
                    .map(|levels| levels.iter().map(Vec::as_slice).collect())
            })
            .collect();
        let cubes: Vec<[&[&[u8]]; 6]> = slices
            .iter()
            .map(|cube| cube.each_ref().map(Vec::as_slice))
            .collect();
        cube_texture::validate_layers(SampledTextureFormat::Float16, size, &cubes, true)?;
        let id = session_mut(&self.shared)?.create_cube_array_texture(
            size,
            &cubes,
            SampledTextureFormat::Float16,
        )?;
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::CubeArray,
        })
    }

    /// Queues a full replacement of a single-level `RGBA16Float` 2D texture.
    ///
    /// The texture must belong to this device and retain its original dimensions.
    /// Values follow `create_rgba16_float_texture`. The bytes are copied now;
    /// the next textured/material scene submission uploads them before any draws.
    /// Earlier submitted frames keep their original contents. Multiple writes
    /// before that submission coalesce to the last write. No device-idle wait or
    /// texture/descriptor recreation is required. Dropping a texture before the
    /// next scene cancels its pending replacement.
    ///
    /// # Errors
    /// Returns an error for a foreign or stale texture, incompatible format or
    /// dimensions, a texture with a mip chain (use
    /// [`Self::update_rgba16_float_texture_with_mips`]), invalid texels, or an
    /// unavailable graphics session.
    pub fn update_rgba16_float_texture(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        texels: &[[f32; 4]],
    ) -> Result<(), GraphicsError> {
        self.update_rgba16_float(texture, width, height, &[texels], false)
    }

    /// Queues a full replacement of every level of an `RGBA16Float` texture created by
    /// [`Self::create_rgba16_float_texture_with_mips`].
    ///
    /// `levels` is the complete chain from `width`×`height` to 1×1, laid out as
    /// [`Self::create_rgba16_float_texture_with_mips`] takes it, and must match the texture's
    /// dimensions and mip count. No level is generated or kept from the previous contents.
    /// Values, queuing, coalescing and cancellation follow
    /// [`Self::update_rgba16_float_texture`]: every level of the last write is uploaded before
    /// any draw of the next textured/material scene submission, and earlier submitted frames
    /// keep their original contents.
    ///
    /// # Errors
    /// Returns an error for a foreign or stale texture, incompatible format, dimensions or mip
    /// count (including a single-level texture larger than 1×1), an incomplete/extra chain or
    /// mismatched per-level texel count, invalid texels, or an unavailable graphics session.
    pub fn update_rgba16_float_texture_with_mips(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        levels: &[&[[f32; 4]]],
    ) -> Result<(), GraphicsError> {
        self.update_rgba16_float(texture, width, height, levels, true)
    }

    /// [`Self::update_rgba16_float_texture`] from binary16 bit patterns, as
    /// [`Self::create_rgba16_float_texture_from_bits`] takes them.
    ///
    /// # Errors
    /// Reports the errors of [`Self::update_rgba16_float_texture`], with non-finite bits as the
    /// invalid texels.
    pub fn update_rgba16_float_texture_from_bits(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        texels: &[[u16; 4]],
    ) -> Result<(), GraphicsError> {
        self.check_float_update(texture)?;
        let packed = sampled_texture::pack_half_levels(width, height, &[texels], false)?;
        session_mut(&self.shared)?.update_float_texture(
            texture.lease.id,
            width,
            height,
            packed,
            false,
        )
    }

    /// [`Self::update_rgba16_float_texture_with_mips`] from binary16 bit patterns, as
    /// [`Self::create_rgba16_float_texture_with_mips_from_bits`] takes them.
    ///
    /// # Errors
    /// Reports the errors of [`Self::update_rgba16_float_texture_with_mips`], with non-finite
    /// bits as the invalid texels.
    pub fn update_rgba16_float_texture_with_mips_from_bits(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        levels: &[&[[u16; 4]]],
    ) -> Result<(), GraphicsError> {
        self.check_float_update(texture)?;
        let packed = sampled_texture::pack_half_levels(width, height, levels, true)?;
        session_mut(&self.shared)?.update_float_texture(
            texture.lease.id,
            width,
            height,
            packed,
            false,
        )
    }

    /// Queues a replacement of level 0 of a texture created by
    /// [`Self::create_rgba16_float_texture_with_generated_mips`] (or its `_from_bits` form),
    /// and regenerates every other level from it on the GPU.
    ///
    /// The next textured/material scene submission copies level 0 and blits the chain before
    /// any draw, in its own command buffer, so a texture rewritten every frame (an animated
    /// height field, say) needs no CPU downsampling. Values, queuing, coalescing and
    /// cancellation follow [`Self::update_rgba16_float_texture`].
    ///
    /// # Errors
    /// Returns an error for a foreign or stale texture, a texture not created with generated
    /// mips, incompatible format or dimensions, invalid texels, or an unavailable session.
    pub fn update_rgba16_float_texture_with_generated_mips(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        texels: &[[f32; 4]],
    ) -> Result<(), GraphicsError> {
        self.check_float_update(texture)?;
        let packed = sampled_texture::pack_float_levels(width, height, &[texels], false)?;
        session_mut(&self.shared)?.update_float_texture(
            texture.lease.id,
            width,
            height,
            packed,
            true,
        )
    }

    /// [`Self::update_rgba16_float_texture_with_generated_mips`] from binary16 bit patterns, as
    /// [`Self::create_rgba16_float_texture_from_bits`] takes them.
    ///
    /// # Errors
    /// Reports the errors of [`Self::update_rgba16_float_texture_with_generated_mips`], with
    /// non-finite bits as the invalid texels.
    pub fn update_rgba16_float_texture_with_generated_mips_from_bits(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        texels: &[[u16; 4]],
    ) -> Result<(), GraphicsError> {
        self.check_float_update(texture)?;
        let packed = sampled_texture::pack_half_levels(width, height, &[texels], false)?;
        session_mut(&self.shared)?.update_float_texture(
            texture.lease.id,
            width,
            height,
            packed,
            true,
        )
    }

    fn check_float_update(&self, texture: &Texture) -> Result<(), GraphicsError> {
        if texture.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "texture belongs to another graphics session",
            ));
        }
        if texture.dimension != TextureDimension::D2 {
            return Err(GraphicsError::invalid_request(format!(
                "float texture updates replace 2D textures only, not a {}",
                texture.dimension.label()
            )));
        }
        if texture.render_target {
            return Err(GraphicsError::invalid_request(
                "a render texture is written by offscreen passes, not float texture updates",
            ));
        }
        Ok(())
    }

    fn update_rgba16_float(
        &self,
        texture: &Texture,
        width: u32,
        height: u32,
        levels: &[&[[f32; 4]]],
        complete: bool,
    ) -> Result<(), GraphicsError> {
        self.check_float_update(texture)?;
        let packed = sampled_texture::pack_float_levels(width, height, levels, complete)?;
        session_mut(&self.shared)?.update_float_texture(
            texture.lease.id,
            width,
            height,
            packed,
            false,
        )
    }

    fn upload_rgba16_float(
        &self,
        width: u32,
        height: u32,
        levels: &[&[[f32; 4]]],
        complete: bool,
    ) -> Result<Texture, GraphicsError> {
        let packed = sampled_texture::pack_float_levels(width, height, levels, complete)?;
        self.upload_packed_float(width, height, &packed, false)
    }

    /// Creates an `RGBA16Float` texture from packed levels; with `generate_mips`, `packed`
    /// holds level 0 alone and the GPU fills the rest of the full chain.
    fn upload_packed_float(
        &self,
        width: u32,
        height: u32,
        packed: &[Vec<u8>],
        generate_mips: bool,
    ) -> Result<Texture, GraphicsError> {
        let mut session = session_mut(&self.shared)?;
        let id = if generate_mips {
            session.create_texture_with_generated_mips(
                width,
                height,
                &packed[0],
                SampledTextureFormat::Float16,
            )?
        } else {
            let slices: Vec<&[u8]> = packed.iter().map(Vec::as_slice).collect();
            session.create_texture(width, height, &slices, SampledTextureFormat::Float16)?
        };
        drop(session);
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::D2,
        })
    }

    fn create_rgba8_texture(
        &self,
        width: u32,
        height: u32,
        texels: &[u8],
        format: SampledTextureFormat,
    ) -> Result<Texture, GraphicsError> {
        validate_mip_level(format, width, height, 0, texels)?;
        let id = session_mut(&self.shared)?.create_texture(width, height, &[texels], format)?;
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::D2,
        })
    }

    fn create_rgba8_texture_with_mips(
        &self,
        width: u32,
        height: u32,
        levels: &[&[u8]],
        format: SampledTextureFormat,
    ) -> Result<Texture, GraphicsError> {
        if width == 0 || height == 0 {
            return Err(GraphicsError::invalid_request(
                "texture byte count does not match its dimensions",
            ));
        }
        let expected_levels = full_mip_chain_len(width, height);
        if levels.len() != expected_levels {
            return Err(GraphicsError::invalid_request(format!(
                "texture mip chain supplies {} levels but {width}x{height} needs {expected_levels} \
                 levels to reach 1x1",
                levels.len()
            )));
        }
        for (level, texels) in (0_u32..).zip(levels) {
            validate_mip_level(format, width, height, level, texels)?;
        }
        let id = session_mut(&self.shared)?.create_texture(width, height, levels, format)?;
        Ok(Texture {
            render_target: false,
            lease: self.lease(id, ResourceKind::Texture),
            dimension: TextureDimension::D2,
        })
    }

    /// Changes the samples per pixel that pipelines and targets created from now on are built
    /// for, and returns the count actually in use after the same fallback as opening: an
    /// unsupported count becomes one sample per pixel.
    ///
    /// Nothing already created is rebuilt. Every textured, instanced, material and postprocess
    /// pipeline and every render or postprocess target remembers the count it was built for,
    /// and submitting one built for another count is refused with an error naming it. Shadow
    /// maps and shadow pipelines are single-sample and unaffected. Destroy the old resources
    /// and create replacements, then draw; frames in flight that used the old resources
    /// complete normally.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn set_sample_count(&self, preferred: SampleCount) -> Result<SampleCount, GraphicsError> {
        Ok(session_mut(&self.shared)?.set_sample_count(preferred))
    }

    /// Creates a depth-tested textured pipeline from target-selected offline shader code.
    ///
    /// # Errors
    ///
    /// Returns an error when native shader loading or pipeline creation fails.
    pub fn create_textured_pipeline(
        &self,
        shader: ShaderArtifact<'_>,
    ) -> Result<TexturedPipeline, GraphicsError> {
        let id = session_mut(&self.shared)?.create_pipeline(shader)?;
        Ok(TexturedPipeline {
            lease: self.lease(id, ResourceKind::TexturedPipeline),
        })
    }

    /// Creates a depth-tested textured pipeline whose vertex stage consumes one model-view-
    /// projection matrix per instance.
    ///
    /// The shader module must contain `instanced_vertex` and `cube_fragment` entry points. Matrix
    /// columns occupy vertex locations 3 through 6 with per-instance stepping.
    ///
    /// # Errors
    ///
    /// Returns an error when native shader loading or pipeline creation fails.
    pub fn create_instanced_textured_pipeline(
        &self,
        shader: ShaderArtifact<'_>,
    ) -> Result<InstancedTexturedPipeline, GraphicsError> {
        let id = session_mut(&self.shared)?.create_instanced_pipeline(shader)?;
        Ok(InstancedTexturedPipeline {
            lease: self.lease(id, ResourceKind::InstancedTexturedPipeline),
        })
    }

    /// Creates the single-sample fullscreen pipeline for the post-processing checkpoint.
    ///
    /// The shader module must contain `post_vertex` and `post_fragment` entry points. The
    /// fragment stage samples the resolved scene color through group-0 bindings 1 and 2. Pass a
    /// [`ShaderArtifact`] directly for the no-uniform convenience form, or a
    /// [`PostprocessPipelineDescriptor`] to declare an exact-size group-0/binding-0 uniform of at
    /// most [`POSTPROCESS_UNIFORM_SIZE_LIMIT`] bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid uniform declaration, a declaration that disagrees with the
    /// artifact's recorded interface, or native shader loading, sampler creation, and pipeline
    /// creation failure.
    pub fn create_postprocess_pipeline<'inputs>(
        &self,
        descriptor: impl Into<PostprocessPipelineDescriptor<'inputs>>,
    ) -> Result<PostprocessPipeline, GraphicsError> {
        let descriptor = descriptor.into();
        validate_postprocess_interface(descriptor.shader, descriptor.uniform_size)?;
        let uniform_size = descriptor.uniform_size.unwrap_or(0);
        let config = PostprocessPipelineConfig {
            volume: None,
            volume_stage: crate::graphics::VolumeStage::None,
            samples: 1,
            uniform_size,
            bloom: None,
            additive: false,
            hdr_output: false,
        };
        let id =
            session_mut(&self.shared)?.create_postprocess_pipeline(descriptor.shader, &config)?;
        Ok(PostprocessPipeline {
            lease: self.lease(id, ResourceKind::PostprocessPipeline),
            uniform_size,
            hdr: false,
        })
    }

    /// Creates an HDR composite with six half-resolution bloom levels at bindings 3 through 8.
    /// The prefilter and downsample shaders use the ordinary no-uniform postprocess interface.
    /// Scene color is linear `RGBA16Float`; the composite must tone-map to the sRGB surface.
    ///
    /// # Errors
    /// Rejects invalid shader interfaces or unsupported native pipeline creation.
    pub fn create_hdr_postprocess_pipeline(
        &self,
        descriptor: PostprocessPipelineDescriptor<'_>,
        prefilter: ShaderArtifact<'_>,
        downsample: ShaderArtifact<'_>,
    ) -> Result<PostprocessPipeline, GraphicsError> {
        self.create_hdr_composite_pipeline(
            descriptor,
            Some(BloomShaders {
                prefilter,
                downsample,
                upsample: None,
            }),
            None,
        )
    }

    /// Adds application-authored scattering and additive depth-aware upscaling to HDR.
    /// Requires a cascaded shadow prepass on every submission. World depth is stored
    /// and read at its native sample count before the foreground depth clear.
    ///
    /// # Errors
    /// Rejects mismatched shader bindings, uniform sizes or unsupported native resources.
    pub fn create_volumetric_hdr_postprocess_pipeline(
        &self,
        descriptor: PostprocessPipelineDescriptor<'_>,
        prefilter: ShaderArtifact<'_>,
        downsample: ShaderArtifact<'_>,
        volume: VolumetricShaders<'_>,
    ) -> Result<PostprocessPipeline, GraphicsError> {
        self.create_hdr_composite_pipeline(
            descriptor,
            Some(BloomShaders {
                prefilter,
                downsample,
                upsample: None,
            }),
            Some(volume),
        )
    }

    /// Creates an HDR tone-map composite with independently optional bloom and scattering.
    ///
    /// Absent effects create no child pipelines and execute no filter, scattering or upscale
    /// passes. Applications can cache the desired combinations and select a pipeline per frame.
    /// Without bloom, the composite must declare no bindings beyond its ordinary 0/1/2 inputs;
    /// with bloom, it must declare all six textures at bindings 3 through 8, or with an upsample
    /// filter ([`BloomShaders::upsample`]) the one accumulated level at binding 3 alone. Targets
    /// remain HDR in every combination and may be shared; their allocated bloom storage is
    /// retained.
    /// Scattering retains its material-content and cascaded-shadow submission requirements.
    ///
    /// # Errors
    /// Rejects mismatched stage interfaces, uniform sizes or unsupported native resources.
    pub fn create_hdr_composite_pipeline(
        &self,
        descriptor: PostprocessPipelineDescriptor<'_>,
        bloom: Option<BloomShaders<'_>>,
        volume: Option<VolumetricShaders<'_>>,
    ) -> Result<PostprocessPipeline, GraphicsError> {
        validate_postprocess_interface(descriptor.shader, descriptor.uniform_size)?;
        hdr::validate_optional_bloom_interface(
            descriptor.shader,
            descriptor.uniform_size,
            bloom.map(|filters| filters.upsample.is_some()),
        )?;
        if let Some(filters) = bloom {
            for shader in [filters.prefilter, filters.downsample]
                .into_iter()
                .chain(filters.upsample)
            {
                validate_postprocess_interface(shader, None)?;
                validate_bloom_filter_interface(shader)?;
            }
        }
        if let Some(volume) = volume {
            for (shader, msaa, composite) in [
                (volume.scatter, false, false),
                (volume.scatter_msaa, true, false),
                (volume.composite, false, true),
                (volume.composite_msaa, true, true),
            ] {
                hdr::validate_volume_interface(shader, descriptor.uniform_size, msaa, composite)?;
            }
        }
        let uniform_size = descriptor.uniform_size.unwrap_or(0);
        let config = PostprocessPipelineConfig {
            volume,
            volume_stage: crate::graphics::VolumeStage::None,
            samples: 1,
            uniform_size,
            bloom,
            additive: false,
            hdr_output: false,
        };
        let id =
            session_mut(&self.shared)?.create_postprocess_pipeline(descriptor.shader, &config)?;
        Ok(PostprocessPipeline {
            lease: self.lease(id, ResourceKind::PostprocessPipeline),
            uniform_size,
            hdr: true,
        })
    }

    /// Creates a depth-tested pipeline from an application-authored shader module, vertex layout,
    /// and binding declaration.
    ///
    /// The declaration is validated against the interface `mulciber-shader` recorded in the
    /// artifact; a mismatch names the offending attribute or slot. The pipeline uses the
    /// session's selected sample count plus the declared [`BlendMode`] and [`DepthMode`].
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, a missing entry point, a declaration that does not
    /// match the artifact's recorded interface, an interface construct outside the current
    /// material vocabulary, or native shader loading and pipeline creation failure.
    pub fn create_material_pipeline(
        &self,
        descriptor: MaterialPipelineDescriptor<'_>,
    ) -> Result<MaterialPipeline, GraphicsError> {
        self.create_material_pipeline_with_format(descriptor, false)
    }

    /// Creates a material pipeline for linear `RGBA16Float` HDR scene targets.
    ///
    /// # Errors
    /// Reports the same declaration errors as `create_material_pipeline`, plus native format errors.
    pub fn create_hdr_material_pipeline(
        &self,
        descriptor: MaterialPipelineDescriptor<'_>,
    ) -> Result<MaterialPipeline, GraphicsError> {
        self.create_material_pipeline_with_format(descriptor, true)
    }

    fn create_material_pipeline_with_format(
        &self,
        descriptor: MaterialPipelineDescriptor<'_>,
        hdr: bool,
    ) -> Result<MaterialPipeline, GraphicsError> {
        let (layout, instance_layout, declaration) = check_material_descriptor(descriptor, hdr)?;
        if declaration
            .texture_dimensions
            .contains(&TextureDimension::CubeArray)
        {
            session_ref(&self.shared)?.require_cube_arrays()?;
        }
        let config = MaterialPipelineConfig {
            hdr,
            vertex_entry: descriptor.vertex_entry,
            fragment_entry: descriptor.fragment_entry,
            stride: layout.stride,
            attributes: &layout.attributes,
            instance_stride: instance_layout
                .as_ref()
                .map_or(0, |instance| instance.stride),
            instance_attributes: instance_layout
                .as_ref()
                .map_or(&[][..], |instance| &instance.attributes),
            uniform: declaration.uniform,
            storage: declaration.storage,
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
            scene_depth_binding: declaration.scene_depth,
            depth_texture_binding: declaration.depth_texture,
            depth_texture_array_binding: declaration.depth_texture_array,
            comparison_sampler_binding: declaration.comparison_sampler,
            blend: descriptor.blend,
            depth: descriptor.depth,
        };
        let id = session_mut(&self.shared)?.create_material_pipeline(descriptor.shader, &config)?;
        Ok(MaterialPipeline {
            hdr,
            lease: self.lease(id, ResourceKind::MaterialPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
            storage_size: declaration.storage.map_or(0, |(_, size)| size),
            instance_stride: instance_layout.map_or(0, |instance| instance.stride),
            scene_depth: declaration.scene_depth.is_some(),
            texture_dimensions: declaration.texture_dimensions,
            shadow_slot: if declaration.depth_texture.is_some() {
                Some(ShadowSlotKind::Map)
            } else if declaration.depth_texture_array.is_some() {
                Some(ShadowSlotKind::Array)
            } else {
                None
            },
            depth: descriptor.depth,
        })
    }

    /// Creates a square sampleable depth target for shadow passes.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, an extent above [`SHADOW_MAP_SIZE_LIMIT`], or native
    /// image allocation failure.
    pub fn create_shadow_map(&self, size: u32) -> Result<ShadowMap, GraphicsError> {
        if size == 0 || size > SHADOW_MAP_SIZE_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map extent {size} is outside the supported 1 through \
                 {SHADOW_MAP_SIZE_LIMIT}"
            )));
        }
        let id = session_mut(&self.shared)?.create_shadow_map(size)?;
        Ok(ShadowMap {
            lease: self.lease(id, ResourceKind::ShadowMap),
            size,
        })
    }

    /// Creates a square layered sampleable depth target for cascaded shadow passes, one
    /// cascade per layer.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, an extent above [`SHADOW_MAP_SIZE_LIMIT`], a zero
    /// layer count, a layer count above [`SHADOW_MAP_LAYER_LIMIT`], or native image allocation
    /// failure.
    pub fn create_shadow_map_array(
        &self,
        size: u32,
        layers: u32,
    ) -> Result<ShadowMapArray, GraphicsError> {
        if size == 0 || size > SHADOW_MAP_SIZE_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map array extent {size} is outside the supported 1 through \
                 {SHADOW_MAP_SIZE_LIMIT}"
            )));
        }
        if layers == 0 || layers > SHADOW_MAP_LAYER_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "shadow map array layer count {layers} is outside the supported 1 through \
                 {SHADOW_MAP_LAYER_LIMIT}"
            )));
        }
        let id = session_mut(&self.shared)?.create_shadow_map_array(size, layers)?;
        Ok(ShadowMapArray {
            lease: self.lease(id, ResourceKind::ShadowMapArray),
            size,
            layers,
        })
    }

    /// Creates a linear `RGBA16Float` color texture for offscreen passes to render into, with
    /// depth and any multisample color storage of its own at the session's current sample
    /// count.
    ///
    /// It cannot be sampled until an offscreen pass has rendered into it, and it is never
    /// updated from the CPU.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero extent, an extent above [`RENDER_TEXTURE_SIZE_LIMIT`], an
    /// adapter that cannot render, blend and filter `RGBA16Float` at the sample count, or
    /// native image allocation failure.
    pub fn create_hdr_render_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<RenderTexture, GraphicsError> {
        for (axis, extent) in [("width", width), ("height", height)] {
            if extent == 0 || extent > RENDER_TEXTURE_SIZE_LIMIT {
                return Err(GraphicsError::invalid_request(format!(
                    "render texture {axis} {extent} is outside the supported 1 through \
                     {RENDER_TEXTURE_SIZE_LIMIT}"
                )));
            }
        }
        let id = session_mut(&self.shared)?.create_render_texture(width, height)?;
        Ok(RenderTexture {
            texture: Texture {
                lease: self.lease(id, ResourceKind::Texture),
                dimension: TextureDimension::D2,
                render_target: true,
            },
            width,
            height,
        })
    }

    /// Creates a depth-only pipeline from an application-authored shader module for shadow
    /// passes.
    ///
    /// The pipeline runs the named vertex entry point into a [`ShadowMap`]'s depth target,
    /// testing and writing depth. Shadow pipelines support at most one uniform binding and one
    /// read-only storage binding (so skinned casters shadow with the same bone palette as
    /// their material records). A caster that must carve fragments out of the depth result —
    /// typically a foliage cutout alpha test — additionally names a fragment entry point,
    /// which unlocks texture and sampler bindings for the test; without a fragment entry the
    /// pipeline runs no fragment stage and the module must record no other bindings. An
    /// instance layout mirrors the caster's material pipeline so scattered geometry shadows
    /// through the same per-instance transforms.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid layout, a missing entry point, a declaration that does
    /// not match the artifact's recorded interface, a binding outside the supported kinds, or
    /// native shader loading and pipeline creation failure.
    pub fn create_shadow_pipeline(
        &self,
        descriptor: ShadowPipelineDescriptor<'_>,
    ) -> Result<ShadowPipeline, GraphicsError> {
        let layout = validate_vertex_layout(descriptor.vertex_layout)?;
        let instance_layout = descriptor
            .instance_layout
            .map(|instance| validate_instance_layout(&layout, instance))
            .transpose()?;
        let interface = descriptor.shader.parse_interface();
        let vertex_entry = find_entry_point(
            &interface,
            descriptor.vertex_entry,
            shader::INTERFACE_STAGE_VERTEX,
            "vertex",
        )?;
        let fragment_entry = descriptor
            .fragment_entry
            .map(|fragment_entry| {
                find_entry_point(
                    &interface,
                    fragment_entry,
                    shader::INTERFACE_STAGE_FRAGMENT,
                    "fragment",
                )
            })
            .transpose()?;
        let (consumed, consumed_instance) =
            validate_layouts_cover_entry(&layout, instance_layout.as_ref(), vertex_entry)?;
        let entries: Vec<_> = core::iter::once(vertex_entry)
            .chain(fragment_entry)
            .collect();
        let declaration = validate_entry_point_bindings(descriptor.bindings, &interface, &entries)?;
        if declaration.scene_depth.is_some()
            || declaration.depth_texture.is_some()
            || declaration.depth_texture_array.is_some()
            || declaration.comparison_sampler.is_some()
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines do not sample depth resources",
            ));
        }
        if declaration
            .texture_dimensions
            .iter()
            .any(|&dimension| dimension != TextureDimension::D2)
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines sample 2D textures only, not cube textures or cube texture \
                 arrays",
            ));
        }
        if descriptor.fragment_entry.is_none()
            && (!declaration.texture_bindings.is_empty()
                || !declaration.sampler_bindings.is_empty())
        {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                "shadow pipelines support texture and sampler bindings only with a declared \
                 fragment entry point",
            ));
        }
        let config = ShadowPipelineConfig {
            vertex_entry: descriptor.vertex_entry,
            fragment_entry: descriptor.fragment_entry,
            stride: layout.stride,
            attributes: &consumed,
            instance_stride: instance_layout
                .as_ref()
                .map_or(0, |instance| instance.stride),
            instance_attributes: &consumed_instance,
            uniform: declaration.uniform,
            storage: declaration.storage,
            texture_bindings: &declaration.texture_bindings,
            sampler_bindings: &declaration.sampler_bindings,
        };
        let id = session_mut(&self.shared)?.create_shadow_pipeline(descriptor.shader, &config)?;
        Ok(ShadowPipeline {
            lease: self.lease(id, ResourceKind::ShadowPipeline),
            layout,
            uniform_size: declaration.uniform.map_or(0, |(_, size)| size),
            storage_size: declaration.storage.map_or(0, |(_, size)| size),
            instance_stride: instance_layout.map_or(0, |instance| instance.stride),
            texture_count: declaration.texture_bindings.len(),
        })
    }

    /// Creates depth and optional multisample color storage for one surface generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_render_targets(&self, info: SurfaceInfo) -> Result<RenderTargets, GraphicsError> {
        let id = session_mut(&self.shared)?.create_render_targets(info)?;
        Ok(RenderTargets {
            lease: self.lease(id, ResourceKind::RenderTargets),
            info,
        })
    }

    /// Creates depth, resolved scene color, and optional multisample color storage for one surface
    /// generation, rendered at the surface's native extent.
    ///
    /// The resolved scene color is both a render target and the sampled input to the fullscreen
    /// post-processing pass.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_postprocess_targets(
        &self,
        info: SurfaceInfo,
    ) -> Result<PostprocessTargets, GraphicsError> {
        self.create_scaled_postprocess_targets(info, RenderScale::NATIVE)
    }

    /// Creates postprocess targets whose offscreen scene extent is scaled relative to the
    /// presentable extent.
    ///
    /// The scene pass renders into the scaled offscreen storage, and the fullscreen
    /// post-processing pass resamples it to the surface's native extent through its linear
    /// sampler, so a scale below native trades scene-pass fill cost for sharpness while text
    /// or overlays drawn by the postprocess stage stay native. Scales above native
    /// supersample. Both dimensions floor at one texel.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty extent or native image allocation failure.
    pub fn create_scaled_postprocess_targets(
        &self,
        info: SurfaceInfo,
        scale: RenderScale,
    ) -> Result<PostprocessTargets, GraphicsError> {
        let scene_extent = scale.scene_extent(info.extent());
        let id =
            session_mut(&self.shared)?.create_postprocess_targets(info, scene_extent, false)?;
        Ok(PostprocessTargets {
            lease: self.lease(id, ResourceKind::PostprocessTargets),
            info,
            scale,
            hdr: false,
        })
    }

    /// Creates scaled linear `RGBA16Float` scene targets and six bloom levels.
    /// Existing surface-format targets remain available through `create_scaled_postprocess_targets`.
    ///
    /// # Errors
    /// Rejects unsupported HDR attachment/filter/sample-count combinations or allocation failure.
    pub fn create_scaled_hdr_postprocess_targets(
        &self,
        info: SurfaceInfo,
        scale: RenderScale,
    ) -> Result<PostprocessTargets, GraphicsError> {
        let id = session_mut(&self.shared)?.create_postprocess_targets(
            info,
            scale.scene_extent(info.extent()),
            true,
        )?;
        Ok(PostprocessTargets {
            lease: self.lease(id, ResourceKind::PostprocessTargets),
            info,
            scale,
            hdr: true,
        })
    }

    /// Destroys an uploaded mesh after its last submitted GPU use completes.
    ///
    /// Dropping the handle performs the same reclamation lazily at the next mutable graphics
    /// operation. This explicit form reports stale or mixed-session handles immediately.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_mesh(&self, mut mesh: Mesh) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut mesh.lease, ResourceKind::Mesh)
    }

    /// Destroys an uploaded texture after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_texture(&self, mut texture: Texture) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut texture.lease, ResourceKind::Texture)
    }

    /// Destroys a render texture, its color with its depth and multisample storage, after its
    /// last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_render_texture(&self, mut target: RenderTexture) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut target.texture.lease, ResourceKind::Texture)
    }

    /// Destroys a textured pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_textured_pipeline(
        &self,
        mut pipeline: TexturedPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::TexturedPipeline)
    }

    /// Destroys an instanced textured pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_instanced_textured_pipeline(
        &self,
        mut pipeline: InstancedTexturedPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::InstancedTexturedPipeline)
    }

    /// Destroys a postprocess pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_postprocess_pipeline(
        &self,
        mut pipeline: PostprocessPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::PostprocessPipeline)
    }

    /// Destroys a material pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_material_pipeline(
        &self,
        mut pipeline: MaterialPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::MaterialPipeline)
    }

    /// Destroys a shadow map after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_map(&self, mut map: ShadowMap) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut map.lease, ResourceKind::ShadowMap)
    }

    /// Destroys a shadow map array after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_map_array(&self, mut array: ShadowMapArray) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut array.lease, ResourceKind::ShadowMapArray)
    }

    /// Destroys a shadow pipeline after its last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_shadow_pipeline(
        &self,
        mut pipeline: ShadowPipeline,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut pipeline.lease, ResourceKind::ShadowPipeline)
    }

    /// Destroys generation-dependent render targets after their last submitted GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_render_targets(&self, mut targets: RenderTargets) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut targets.lease, ResourceKind::RenderTargets)
    }

    /// Destroys generation-dependent postprocess targets after their last GPU use completes.
    ///
    /// # Errors
    ///
    /// Returns an error for a mixed-session or stale handle, or when native completion fails.
    pub fn destroy_postprocess_targets(
        &self,
        mut targets: PostprocessTargets,
    ) -> Result<(), GraphicsError> {
        self.destroy_lease(&mut targets.lease, ResourceKind::PostprocessTargets)
    }

    fn lease(&self, id: ResourceId, kind: ResourceKind) -> ResourceLease {
        ResourceLease::new(self.shared.id, id, kind, Rc::clone(&self.shared.drops))
    }

    fn destroy_lease(
        &self,
        lease: &mut ResourceLease,
        kind: ResourceKind,
    ) -> Result<(), GraphicsError> {
        if lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(format!(
                "{} handle belongs to a different graphics session than this device",
                kind.label()
            )));
        }
        session_mut(&self.shared)?.destroy_resource(DestroyRequest { kind, id: lease.id })?;
        lease.disarm();
        Ok(())
    }
}

/// Submission owner for one native session.
pub struct Queue<'window> {
    shared: Shared<'window>,
}

impl Queue<'_> {
    /// Enables or disables asynchronous GPU duration diagnostics for future submissions.
    ///
    /// Enabling an unsupported queue succeeds so rendering remains available; capability and
    /// drain results keep the fallback observable.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown or if native instrumentation allocation fails.
    pub fn set_gpu_timing_enabled(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        session_mut(&self.shared)?.set_gpu_timing_enabled(enabled)
    }

    /// Drains completed GPU duration samples without waiting for unfinished GPU work.
    ///
    /// Samples may arrive one or more frames after submission. Their frame index is the same
    /// zero-based session index reported by [`PresentedFrame::index`], allowing an application to
    /// correlate GPU work with presentation feedback. Ignoring the drain costs bounded memory.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn take_gpu_timings(&mut self) -> Result<GpuTimingFeedback, GraphicsError> {
        Ok(session_mut(&self.shared)?.take_gpu_timings())
    }

    /// Renders one explicitly selected scene recipe, presents the frame, and consumes it.
    ///
    /// This stable verb prevents each experimentally extracted workload from growing another
    /// queue-method name. `SceneContent` and `SceneOutput` compose the narrow axes supported by the
    /// current slice; they are not a general command encoder.
    ///
    /// # Errors
    ///
    /// Returns the validation, native encoding, synchronization, submission, or presentation
    /// error produced by the selected recipe.
    pub fn render_and_present(
        &mut self,
        frame: Frame<'_>,
        submission: SceneSubmission<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        let foreground_start = validate_scene_recipe(&submission)?;
        match (submission.content, submission.output) {
            (SceneContent::Textured(draws), SceneOutput::Direct(targets)) => self
                .draw_textured_scene_and_present(
                    frame,
                    TexturedScene {
                        draws,
                        targets,
                        clear: submission.clear,
                    },
                ),
            (
                SceneContent::Textured(draws),
                SceneOutput::Postprocessed {
                    pipeline,
                    targets,
                    uniform,
                },
            ) => self.draw_textured_scene_postprocessed_and_present(
                frame,
                PostprocessedScene {
                    draws,
                    postprocess_pipeline: pipeline,
                    targets,
                    uniform,
                    clear: submission.clear,
                },
            ),
            (SceneContent::Instanced(batches), SceneOutput::Direct(targets)) => self
                .draw_instanced_textured_scene_and_present(
                    frame,
                    batches,
                    targets,
                    submission.clear,
                ),
            (
                SceneContent::Instanced(batches),
                SceneOutput::Postprocessed {
                    pipeline,
                    targets,
                    uniform,
                },
            ) => self.draw_instanced_textured_scene_postprocessed_and_present(
                frame,
                batches,
                pipeline,
                targets,
                uniform,
                submission.clear,
            ),
            (SceneContent::MaterialWithForeground { .. }, SceneOutput::Direct(_)) => {
                Err(GraphicsError::with_kind(
                    GraphicsErrorKind::Unsupported,
                    "foreground material content requires postprocessed output",
                ))
            }
            (SceneContent::Material(records), SceneOutput::Direct(targets)) => self
                .draw_material_scene_and_present(
                    frame,
                    records,
                    submission.shadow,
                    targets,
                    submission.clear,
                ),
            (
                SceneContent::Material(records)
                | SceneContent::MaterialWithForeground { records, .. },
                SceneOutput::Postprocessed {
                    pipeline,
                    targets,
                    uniform,
                },
            ) => self.draw_material_scene_postprocessed_and_present(
                frame,
                records,
                submission.shadow,
                submission.overlay,
                submission.offscreen,
                foreground_start,
                pipeline,
                targets,
                uniform,
                submission.clear,
            ),
        }
    }

    /// Draws one indexed textured mesh, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for mixed-session or stale handles, a non-finite transform, or native
    /// encoding, submission, validation, or presentation failure.
    pub fn draw_textured_and_present(
        &mut self,
        frame: Frame<'_>,
        draw: TexturedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        let scene_draw = TexturedSceneDraw {
            mesh: draw.mesh,
            texture: draw.texture,
            pipeline: draw.pipeline,
            model_view_projection: draw.model_view_projection,
        };
        self.draw_textured_scene_and_present(
            frame,
            TexturedScene {
                draws: core::slice::from_ref(&scene_draw),
                targets: draw.targets,
                clear: draw.clear,
            },
        )
    }

    /// Draws a non-empty sequence of textured objects in one depth-tested render pass, presents
    /// the frame, and consumes it.
    ///
    /// Each object independently selects its mesh, texture, pipeline, and transform. The targets
    /// and clear operation belong to the scene pass rather than being repeated per object.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene, mixed-session or stale handles, a non-finite transform,
    /// or native encoding, submission, validation, or presentation failure.
    pub fn draw_textured_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        scene: TexturedScene<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_scene(frame.shared.id, frame.info, scene.draws, scene.targets)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_scene_and_present(
            token,
            scene.draws,
            scene.targets.lease.id,
            scene.clear,
        )
    }

    /// Draws one indexed textured mesh into sampled offscreen color, runs one fullscreen
    /// post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for mixed-session or stale handles, a non-finite transform, or native
    /// encoding, synchronization, submission, validation, or presentation failure.
    pub fn draw_textured_postprocessed_and_present(
        &mut self,
        frame: Frame<'_>,
        draw: PostprocessedDraw<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        let scene_draw = TexturedSceneDraw {
            mesh: draw.mesh,
            texture: draw.texture,
            pipeline: draw.scene_pipeline,
            model_view_projection: draw.model_view_projection,
        };
        self.draw_textured_scene_postprocessed_and_present(
            frame,
            PostprocessedScene {
                draws: core::slice::from_ref(&scene_draw),
                postprocess_pipeline: draw.postprocess_pipeline,
                targets: draw.targets,
                uniform: draw.uniform,
                clear: draw.clear,
            },
        )
    }

    /// Draws a non-empty sequence of textured objects into resolved scene color, runs one
    /// fullscreen post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene, mixed-session or stale handles, a non-finite transform,
    /// or native encoding, synchronization, submission, validation, or presentation failure.
    pub fn draw_textured_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        scene: PostprocessedScene<'_>,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_scene(frame.shared.id, frame.info, scene.draws, scene.targets)?;
        if scene.postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        validate_hdr_pair(scene.postprocess_pipeline.hdr, scene.targets.hdr)?;
        validate_postprocess_uniform(scene.postprocess_pipeline, scene.uniform)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_scene_postprocessed_and_present(
            token,
            scene.draws,
            scene.postprocess_pipeline.lease.id,
            scene.targets.lease.id,
            scene.uniform,
            scene.clear,
        )
    }

    /// Draws a non-empty sequence of non-empty textured instance batches in one depth-tested
    /// render pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene or batch, mixed-session or stale handles, a non-finite
    /// transform, an unsupported native count, or native encoding, submission, validation, or
    /// presentation failure.
    fn draw_instanced_textured_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        batches: &[TexturedInstanceBatch<'_>],
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_instanced_scene(frame.shared.id, frame.info, batches, targets)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_instanced_scene_and_present(
            token,
            batches,
            targets.lease.id,
            clear,
        )
    }

    /// Draws a non-empty sequence of non-empty textured instance batches into resolved scene
    /// color, runs one fullscreen post-processing pass, presents the frame, and consumes it.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty scene or batch, mixed-session or stale handles, a non-finite
    /// transform, an unsupported native count, or native encoding, synchronization, submission,
    /// validation, or presentation failure.
    fn draw_instanced_textured_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        batches: &[TexturedInstanceBatch<'_>],
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        uniform: &[u8],
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_instanced_scene(frame.shared.id, frame.info, batches, targets)?;
        if postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        validate_hdr_pair(postprocess_pipeline.hdr, targets.hdr)?;
        validate_postprocess_uniform(postprocess_pipeline, uniform)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_instanced_scene_postprocessed_and_present(
            token,
            batches,
            postprocess_pipeline.lease.id,
            targets.lease.id,
            uniform,
            clear,
        )
    }

    /// Draws an optional depth-only shadow pass followed by a non-empty sequence of
    /// application-authored material records in one depth-tested render pass, presents the
    /// frame, and consumes it.
    fn draw_material_scene_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        shadow: Option<ShadowPrepass<'_>>,
        targets: &RenderTargets,
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        let depth_clear = material_scene_depth_clear(records)?;
        self.validate_shadow_pass(frame.shared.id, shadow.as_ref())?;
        session_ref(&self.shared)?.validate_shadow_sampling(records, shadow.as_ref())?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_material_scene_and_present(
            token,
            records,
            shadow.as_ref(),
            targets.lease.id,
            clear,
            depth_clear,
        )
    }

    /// Draws an optional depth-only shadow pass, then a non-empty sequence of
    /// application-authored material records into resolved scene color, runs one fullscreen
    /// post-processing pass, draws any overlay records into the presentable target at native
    /// extent, presents the frame, and consumes it.
    #[allow(clippy::too_many_arguments)]
    fn draw_material_scene_postprocessed_and_present(
        &mut self,
        mut frame: Frame<'_>,
        records: &[MaterialRecord<'_>],
        shadow: Option<ShadowPrepass<'_>>,
        overlay: Option<&[MaterialRecord<'_>]>,
        offscreen: &[OffscreenPass<'_>],
        foreground_start: Option<usize>,
        postprocess_pipeline: &PostprocessPipeline,
        targets: &PostprocessTargets,
        uniform: &[u8],
        clear: ClearColor,
    ) -> Result<FrameDisposition, GraphicsError> {
        self.validate_material_scene(frame.shared.id, frame.info, records, targets)?;
        let depth_clear = material_scene_depth_clear(records)?;
        self.validate_shadow_pass(frame.shared.id, shadow.as_ref())?;
        session_ref(&self.shared)?.validate_shadow_sampling(records, shadow.as_ref())?;
        if let Some(overlay) = overlay {
            self.validate_overlay_records(frame.shared.id, overlay)?;
        }
        let offscreen_depth_clears =
            self.validate_offscreen_passes(frame.shared.id, offscreen, shadow.as_ref())?;
        self.validate_render_texture_sampling(
            records.iter().chain(overlay.unwrap_or(&[])),
            offscreen,
        )?;
        if postprocess_pipeline.lease.session != self.shared.id {
            return Err(GraphicsError::invalid_request(
                "postprocess pipeline belongs to a different graphics session than the queue",
            ));
        }
        validate_hdr_pair(postprocess_pipeline.hdr, targets.hdr)?;
        validate_postprocess_uniform(postprocess_pipeline, uniform)?;
        let token = frame
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.draw_material_scene_postprocessed_and_present(
            token,
            records,
            shadow.as_ref(),
            overlay.unwrap_or(&[]),
            offscreen,
            &offscreen_depth_clears,
            foreground_start,
            postprocess_pipeline.lease.id,
            targets.lease.id,
            uniform,
            clear,
            depth_clear,
        )
    }

    /// Validates a shadow prepass's handles, record shape, cascade agreement, and
    /// uniform/layout agreement.
    fn validate_shadow_pass(
        &self,
        frame_session: u64,
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<(), GraphicsError> {
        let Some(shadow) = shadow else {
            return Ok(());
        };
        let target_session = match shadow {
            ShadowPrepass::Single(pass) => {
                if pass.records.is_empty() {
                    return Err(GraphicsError::invalid_request(
                        "shadow pass must contain at least one record",
                    ));
                }
                ("shadow map", pass.map.lease.session)
            }
            ShadowPrepass::Cascaded(pass) => {
                let layers = usize::try_from(pass.map.layers()).expect("u32 layers fit usize");
                if pass.cascades.len() != layers {
                    return Err(GraphicsError::invalid_request(format!(
                        "cascaded shadow pass supplies {} cascade record lists but its map has \
                         {layers} layers",
                        pass.cascades.len()
                    )));
                }
                ("shadow map array", pass.map.lease.session)
            }
        };
        for (label, session) in shadow
            .records()
            .flat_map(|record| {
                [
                    ("shadow pipeline", record.pipeline.lease.session),
                    ("mesh", record.geometry.mesh().lease.session),
                ]
                .into_iter()
                .chain(
                    record
                        .textures
                        .iter()
                        .map(|texture| ("texture", texture.lease.session)),
                )
            })
            .chain([target_session])
        {
            if session != self.shared.id || session != frame_session {
                return Err(GraphicsError::invalid_request(format!(
                    "{label} belongs to a different graphics session than the queue and frame"
                )));
            }
        }
        for record in shadow.records() {
            let expected =
                usize::try_from(record.pipeline.uniform_size).expect("u32 size fits usize");
            if record.uniform.len() != expected {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} uniform bytes but its pipeline declares {expected}",
                    record.uniform.len()
                )));
            }
            let expected_storage =
                usize::try_from(record.pipeline.storage_size).expect("u32 size fits usize");
            if record.storage.len() != expected_storage {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} storage bytes but its pipeline declares \
                     {expected_storage}",
                    record.storage.len()
                )));
            }
            if record.geometry.mesh().layout != record.pipeline.layout {
                return Err(GraphicsError::invalid_request(
                    "shadow record's mesh vertex layout does not match its pipeline's declared \
                     layout",
                ));
            }
            if record.textures.len() != record.pipeline.texture_count {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies {} textures but its pipeline declares {} texture \
                     slots",
                    record.textures.len(),
                    record.pipeline.texture_count
                )));
            }
            if let Some(texture) = record
                .textures
                .iter()
                .find(|texture| texture.dimension != TextureDimension::D2)
            {
                return Err(GraphicsError::invalid_request(format!(
                    "shadow record supplies a {} but shadow pipelines sample 2D textures only",
                    texture.dimension.label()
                )));
            }
            validate_instance_supply(
                "shadow record",
                record.instances,
                record.pipeline.instance_stride,
            )?;
        }
        Ok(())
    }

    fn validate_material_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        records: &[MaterialRecord<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if records.is_empty() {
            return Err(GraphicsError::invalid_request(
                "material scene must contain at least one record",
            ));
        }
        self.validate_targets(frame_session, frame_info, targets)?;
        for record in records {
            validate_hdr_pair(record.pipeline.hdr, targets.hdr())?;
        }
        self.validate_material_records(frame_session, records)
    }

    /// Validates the overlay pass: non-empty records whose pipelines fit the presentable pass,
    /// which carries no depth target and samples no shadow map.
    fn validate_overlay_records(
        &self,
        frame_session: u64,
        overlay: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        if overlay.is_empty() {
            return Err(GraphicsError::invalid_request(
                "overlay pass must contain at least one record",
            ));
        }
        self.validate_material_records(frame_session, overlay)?;
        for record in overlay {
            validate_hdr_pair(record.pipeline.hdr, false)?;
            if record.pipeline.depth != DepthMode::Off {
                return Err(GraphicsError::invalid_request(
                    "overlay records draw into the presentable target, which carries no depth \
                     target; their pipelines must declare DepthMode::Off",
                ));
            }
            if record.pipeline.scene_depth {
                return Err(GraphicsError::invalid_request(
                    "overlay records may not sample scene depth",
                ));
            }
            if record.pipeline.shadow_slot.is_some() {
                return Err(GraphicsError::invalid_request(
                    "overlay records draw after the scene pass and may not sample a shadow map; \
                     their pipelines must declare no depth-texture slot",
                ));
            }
        }
        Ok(())
    }

    /// Validates the offscreen passes: each targets a distinct same-session render texture
    /// with non-empty records whose HDR pipelines sample neither scene depth nor the pass's own
    /// target, and any shadow map their records sample has been rendered. Returns each pass's
    /// depth clear, in pass order.
    fn validate_offscreen_passes(
        &self,
        frame_session: u64,
        offscreen: &[OffscreenPass<'_>],
        shadow: Option<&ShadowPrepass<'_>>,
    ) -> Result<Vec<f32>, GraphicsError> {
        let mut depth_clears = Vec::with_capacity(offscreen.len());
        for (index, pass) in offscreen.iter().enumerate() {
            let target = &pass.target.texture;
            if target.lease.session != self.shared.id || target.lease.session != frame_session {
                return Err(GraphicsError::invalid_request(
                    "render texture belongs to a different graphics session than the queue and \
                     frame",
                ));
            }
            if offscreen[..index]
                .iter()
                .any(|earlier| earlier.target.texture.id() == target.id())
            {
                return Err(GraphicsError::invalid_request(
                    "two offscreen passes in one submission target the same render texture",
                ));
            }
            if pass.records.is_empty() {
                return Err(GraphicsError::invalid_request(
                    "offscreen pass must contain at least one record",
                ));
            }
            self.validate_material_records(frame_session, pass.records)?;
            for record in pass.records {
                validate_hdr_pair(record.pipeline.hdr, true)?;
                if record.pipeline.scene_depth {
                    return Err(GraphicsError::invalid_request(
                        "offscreen records may not sample scene depth",
                    ));
                }
                if record
                    .textures
                    .iter()
                    .any(|texture| texture.id() == target.id())
                {
                    return Err(GraphicsError::invalid_request(
                        "offscreen record samples the render texture its pass renders into",
                    ));
                }
            }
            session_ref(&self.shared)?.validate_shadow_sampling(pass.records, shadow)?;
            depth_clears.push(material_scene_depth_clear(pass.records)?);
        }
        Ok(depth_clears)
    }

    /// Rejects sampling a render texture nothing has rendered: neither an earlier submission
    /// nor, for scene and overlay records, any of this submission's offscreen passes, nor, for
    /// an offscreen pass's records, an earlier pass in it.
    fn validate_render_texture_sampling<'a>(
        &self,
        scene: impl Iterator<Item = &'a MaterialRecord<'a>>,
        offscreen: &[OffscreenPass<'_>],
    ) -> Result<(), GraphicsError> {
        let session = session_ref(&self.shared)?;
        let check = |record: &MaterialRecord<'_>, passes: &[OffscreenPass<'_>]| {
            for texture in record
                .textures
                .iter()
                .filter(|texture| texture.render_target)
            {
                let pending = passes
                    .iter()
                    .any(|pass| pass.target.texture.id() == texture.id());
                if !pending && !session.render_texture_rendered(texture.id())? {
                    return Err(GraphicsError::invalid_request(
                        "material record samples a render texture that no offscreen pass has \
                         rendered",
                    ));
                }
            }
            Ok(())
        };
        for record in scene {
            check(record, offscreen)?;
        }
        for (index, pass) in offscreen.iter().enumerate() {
            for record in pass.records {
                check(record, &offscreen[..index])?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn validate_material_records(
        &self,
        frame_session: u64,
        records: &[MaterialRecord<'_>],
    ) -> Result<(), GraphicsError> {
        for record in records {
            let mesh = record.geometry.uploaded_mesh();
            let handles = [("material pipeline", record.pipeline.lease.session)]
                .into_iter()
                .chain(mesh.map(|mesh| ("mesh", mesh.lease.session)))
                .chain(
                    record
                        .textures
                        .iter()
                        .map(|texture| ("texture", texture.lease.session)),
                )
                .chain(record.shadow_map.iter().map(|source| match source {
                    ShadowSource::Map(map) => ("shadow map", map.lease.session),
                    ShadowSource::Array(array) => ("shadow map array", array.lease.session),
                }));
            for (label, session) in handles {
                if session != self.shared.id || session != frame_session {
                    return Err(GraphicsError::invalid_request(format!(
                        "{label} belongs to a different graphics session than the queue and frame"
                    )));
                }
            }
            let supplied = record.shadow_map.map(|source| match source {
                ShadowSource::Map(_) => ShadowSlotKind::Map,
                ShadowSource::Array(_) => ShadowSlotKind::Array,
            });
            if supplied != record.pipeline.shadow_slot {
                return Err(GraphicsError::invalid_request(
                    match (supplied, record.pipeline.shadow_slot) {
                        (Some(_), None) => {
                            "material record supplies a shadow map but its pipeline declares no \
                             depth-texture slot"
                        }
                        (None, Some(_)) => {
                            "material record supplies no shadow map but its pipeline declares a \
                             depth-texture slot"
                        }
                        (Some(ShadowSlotKind::Map), _) => {
                            "material record supplies a single shadow map but its pipeline \
                             declares a depth-texture-array slot"
                        }
                        _ => {
                            "material record supplies a shadow map array but its pipeline \
                             declares a plain depth-texture slot"
                        }
                    },
                ));
            }
            validate_record_textures(record.textures, &record.pipeline.texture_dimensions)?;
            let expected =
                usize::try_from(record.pipeline.uniform_size).expect("u32 size fits usize");
            if record.uniform.len() != expected {
                return Err(GraphicsError::invalid_request(format!(
                    "material record supplies {} uniform bytes but its pipeline declares {}",
                    record.uniform.len(),
                    expected
                )));
            }
            let expected_storage =
                usize::try_from(record.pipeline.storage_size).expect("u32 size fits usize");
            if record.storage.len() != expected_storage {
                return Err(GraphicsError::invalid_request(format!(
                    "material record supplies {} storage bytes but its pipeline declares \
                     {expected_storage}",
                    record.storage.len()
                )));
            }
            validate_instance_supply(
                "material record",
                record.instances,
                record.pipeline.instance_stride,
            )?;
            match record.geometry {
                GeometrySource::Mesh(mesh) => {
                    if mesh.layout != record.pipeline.layout {
                        return Err(GraphicsError::invalid_request(
                            "material record's mesh vertex layout does not match its pipeline's \
                             declared layout",
                        ));
                    }
                }
                GeometrySource::MeshPart(part) => {
                    if part.mesh().layout != record.pipeline.layout {
                        return Err(GraphicsError::invalid_request(
                            "material record's mesh vertex layout does not match its pipeline's \
                             declared layout",
                        ));
                    }
                }
                GeometrySource::Transient(geometry) => {
                    let stride = usize::try_from(record.pipeline.layout.stride)
                        .expect("validated stride fits usize");
                    if geometry.vertices.is_empty()
                        || !geometry.vertices.len().is_multiple_of(stride)
                    {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient vertex bytes must be a non-zero \
                             multiple of its pipeline's declared layout stride",
                        ));
                    }
                    if geometry.indices.is_empty() {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient geometry must supply at least one index",
                        ));
                    }
                    if geometry
                        .indices
                        .out_of_range(geometry.vertices.len() / stride)
                    {
                        return Err(GraphicsError::invalid_request(
                            "material record's transient geometry contains an out-of-range index",
                        ));
                    }
                    let supplied = geometry
                        .vertices
                        .len()
                        .checked_add(geometry.indices.byte_len())
                        .filter(|&supplied| {
                            supplied
                                <= usize::try_from(TRANSIENT_GEOMETRY_SIZE_LIMIT)
                                    .expect("u32 limit fits usize")
                        });
                    if supplied.is_none() {
                        return Err(GraphicsError::invalid_request(format!(
                            "material record's transient geometry exceeds the \
                             {TRANSIENT_GEOMETRY_SIZE_LIMIT}-byte supply limit",
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Rejects scene targets from another session as an invalid request, and same-session targets
    /// whose surface information no longer matches the frame as stale, so the two failures keep
    /// their distinct corrections: fix the caller versus rebuild the targets.
    fn validate_targets(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        let label = targets.label();
        if targets.session() != self.shared.id || targets.session() != frame_session {
            return Err(GraphicsError::invalid_request(format!(
                "{label} belong to a different graphics session than the queue and frame"
            )));
        }
        if targets.info() != frame_info {
            return Err(GraphicsError::stale_resource(format!(
                "{label} are stale for the frame's surface information; recreate them from the \
                 frame's surface info"
            )));
        }
        Ok(())
    }

    fn validate_draw_handle_sessions(
        queue_session: u64,
        frame_session: u64,
        mesh_session: u64,
        texture_session: u64,
        pipeline_session: u64,
    ) -> Result<(), GraphicsError> {
        for (label, session) in [
            ("mesh", mesh_session),
            ("texture", texture_session),
            ("pipeline", pipeline_session),
        ] {
            if session != queue_session || session != frame_session {
                return Err(GraphicsError::invalid_request(format!(
                    "{label} belongs to a different graphics session than the queue and frame"
                )));
            }
        }
        Ok(())
    }

    fn validate_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        draws: &[TexturedSceneDraw<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if draws.is_empty() {
            return Err(GraphicsError::invalid_request(
                "textured scene must contain at least one draw",
            ));
        }
        validate_hdr_pair(false, targets.hdr())?;
        self.validate_targets(frame_session, frame_info, targets)?;
        for draw in draws {
            Self::validate_draw_handle_sessions(
                self.shared.id,
                frame_session,
                draw.mesh.lease.session,
                draw.texture.lease.session,
                draw.pipeline.lease.session,
            )?;
            validate_fixed_pipeline_texture(draw.texture)?;
            if !draw
                .model_view_projection
                .iter()
                .flatten()
                .all(|component| component.is_finite())
            {
                return Err(GraphicsError::invalid_request(
                    "draw transform must contain only finite values",
                ));
            }
        }
        Ok(())
    }

    fn validate_instanced_scene(
        &self,
        frame_session: u64,
        frame_info: SurfaceInfo,
        batches: &[TexturedInstanceBatch<'_>],
        targets: &impl SceneTargets,
    ) -> Result<(), GraphicsError> {
        if batches.is_empty() {
            return Err(GraphicsError::invalid_request(
                "instanced textured scene must contain at least one batch",
            ));
        }
        validate_hdr_pair(false, targets.hdr())?;
        self.validate_targets(frame_session, frame_info, targets)?;
        for batch in batches {
            if batch.model_view_projections.is_empty() {
                return Err(GraphicsError::invalid_request(
                    "instanced textured scene batches must contain at least one transform",
                ));
            }
            Self::validate_draw_handle_sessions(
                self.shared.id,
                frame_session,
                batch.mesh.lease.session,
                batch.texture.lease.session,
                batch.pipeline.lease.session,
            )?;
            validate_fixed_pipeline_texture(batch.texture)?;
            if !batch
                .model_view_projections
                .iter()
                .flatten()
                .flatten()
                .all(|component| component.is_finite())
            {
                return Err(GraphicsError::invalid_request(
                    "instance transforms must contain only finite values",
                ));
            }
        }
        Ok(())
    }
}

/// Presentation owner for one native session.
pub struct Surface<'window> {
    shared: Shared<'window>,
}

impl<'window> Surface<'window> {
    /// Current graphics-owned surface generation.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn info(&self) -> Result<SurfaceInfo, GraphicsError> {
        Ok(session_ref(&self.shared)?.info())
    }

    /// Acquires one owned native frame token for current window metrics.
    ///
    /// Reconfiguration for changed metrics happens inside acquisition: a ready frame always
    /// matches the requested metrics, and its surface information reports the generation that
    /// render targets must match.
    ///
    /// # Errors
    ///
    /// Returns fatal native acquisition, deferred abandonment, validation, or device failures.
    pub fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<FrameAcquire<Frame<'window>>, GraphicsError> {
        reclaim_lazy_resources(&self.shared)?;
        let acquisition = session_mut(&self.shared)?.acquire(metrics)?;
        Ok(acquisition.map_ready(|token| Frame {
            info: token.info(),
            token: Some(token),
            shared: self.shared.clone(),
        }))
    }

    /// Selects synchronized presentation (`true`) or immediate presentation (`false`).
    ///
    /// Immediate presentation preserves throughput below the display refresh rate but may tear.
    /// The default is synchronized. Call between frames, before acquisition; Vulkan applies
    /// changes through its normal swapchain reconfiguration/retirement path on next acquisition.
    /// Compositors and variable-refresh displays can impose their own display policy.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` if immediate presentation is unavailable, without changing policy,
    /// or a native/session error. Unsupported requests never silently turn synchronization on.
    pub fn set_vsync(&mut self, enabled: bool) -> Result<(), GraphicsError> {
        self.set_presentation_mode(if enabled {
            crate::PresentationMode::Synchronized
        } else {
            crate::PresentationMode::Immediate
        })
    }

    /// Selects a presentation policy between frames.
    ///
    /// Vulkan adaptive uses native FIFO relaxed. Metal starts unsynchronized and enables sync
    /// only after sustained native-refresh throughput with GPU headroom; a missed deadline
    /// releases it again. Variable-refresh screens retain native synchronization. Unknown
    /// Metal timing remains immediate. Adaptive never imposes a lower FPS cap.
    ///
    /// Strict automatically chooses full/half native refresh using CPU/GPU workload and
    /// sustained recovery headroom; `HalfRefresh` always targets half. Both require native
    /// scheduling support. Metal avoids the divisor on variable-capable displays. Display
    /// capability alone does not prove per-frame VRR engagement.
    ///
    /// # Errors
    /// Returns `Unsupported` without changing policy if the native mode is unavailable,
    /// or a lifecycle/native/session error. An acquired frame must be disposed first.
    pub fn set_presentation_mode(
        &mut self,
        mode: crate::PresentationMode,
    ) -> Result<(), GraphicsError> {
        if Rc::strong_count(&self.shared.inner) != 3 {
            return Err(GraphicsError::lifecycle(
                "cannot change presentation mode while an acquired frame is live",
            ));
        }
        session_mut(&self.shared)?.set_presentation_mode(mode)
    }

    /// Reports the currently applied native policy. Adaptive resolves to immediate or
    /// synchronized where Mulciber makes that choice per present (Metal, and Vulkan switching
    /// between FIFO and immediate); native Vulkan FIFO relaxed reports adaptive, because the
    /// driver decides. A frame clock may pace onto the refresh only while this is synchronized.
    ///
    /// # Errors
    /// Returns an error after session shutdown.
    pub fn active_presentation_mode(&self) -> Result<crate::PresentationMode, GraphicsError> {
        Ok(session_ref(&self.shared)?.active_presentation_mode())
    }

    /// Whether this surface can implement the requested mode without substituting another.
    ///
    /// # Errors
    /// Returns native capability-query or session errors.
    pub fn supports_presentation_mode(
        &self,
        mode: crate::PresentationMode,
    ) -> Result<bool, GraphicsError> {
        session_ref(&self.shared)?.supports_presentation_mode(mode)
    }

    /// Native refresh interval, independent of application FPS. Updated by acquisition.
    /// Variable screens report their fastest interval. Unknown timing returns `None`.
    ///
    /// # Errors
    /// Returns an error after session shutdown.
    pub fn refresh_interval(&self) -> Result<Option<Duration>, GraphicsError> {
        Ok(session_ref(&self.shared)?.refresh_interval())
    }

    /// Native refresh interval when the presentation backend reports the screen as fixed-refresh,
    /// for pacing a frame clock onto. `None` for a variable-refresh screen, where
    /// [`Self::refresh_interval`] is only the fastest period, and wherever fixedness is
    /// unknown. Vulkan reads `VK_EXT_present_timing`, which can say so on platforms whose window
    /// metrics cannot; Metal repeats the window's fixed display timing.
    ///
    /// # Errors
    /// Returns an error after session shutdown.
    pub fn fixed_refresh_interval(&self) -> Result<Option<Duration>, GraphicsError> {
        Ok(session_ref(&self.shared)?.fixed_refresh_interval())
    }

    /// Drains presentation feedback reported by the native backend since the previous drain.
    ///
    /// Feedback is diagnostic and never blocks. Undrained samples are kept in a bounded queue, so
    /// skipping this call costs a fixed amount of memory and no correctness. A backend without
    /// native presentation feedback reports [`PresentFeedback::Unsupported`] on every drain so
    /// estimation fallbacks remain observable rather than silent.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn take_present_feedback(&mut self) -> Result<PresentFeedback, GraphicsError> {
        Ok(session_mut(&self.shared)?.take_present_feedback())
    }

    /// Asks for the next frame this surface acquires to be read back when it is presented.
    ///
    /// Request before [`Self::acquire`]: a frame already acquired is not captured. Whichever
    /// [`Queue`] verb presents the frame (direct, postprocessed, HDR, bloom, volumetric or with
    /// an overlay) copies the presentable image after its last pass, presents it, then **blocks
    /// until the GPU finishes that frame** and converts the copy into a [`FrameCapture`] for
    /// [`Self::take_frame_capture`]. That wait makes the captured frame's timing
    /// unrepresentative, so this is a screenshot path, not a per-frame readback. A frame that
    /// is abandoned, refused before native work, or whose submission or presentation fails is
    /// not captured, and the request stays pending for the next one. Repeating a pending request
    /// changes nothing. The clear-only [`crate::ClearSurface`] does not capture.
    ///
    /// Vulkan creates its swapchain images with transfer-source usage wherever the surface
    /// supports it, so a pending request needs no swapchain change. Metal makes the drawables it
    /// vends readable (`framebufferOnly` off) only while a capture is pending, because readable
    /// drawables give up display optimizations.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` without recording the request when the presentable images cannot be
    /// read back: a Vulkan surface whose swapchain images do not support
    /// `VK_IMAGE_USAGE_TRANSFER_SRC_BIT`, or a presentable format other than four 8-bit channels.
    /// Returns an error after session shutdown.
    pub fn request_frame_capture(&mut self) -> Result<(), GraphicsError> {
        session_mut(&self.shared)?.request_frame_capture()
    }

    /// Takes the most recent completed frame capture, or `None` when no requested frame has been
    /// presented since the last take.
    ///
    /// A capture completes inside the presenting [`Queue`] call, so it is available as soon as
    /// that call returns. A later capture replaces one that was never taken.
    ///
    /// # Errors
    ///
    /// Returns an error after session shutdown.
    pub fn take_frame_capture(&mut self) -> Result<Option<FrameCapture>, GraphicsError> {
        Ok(session_mut(&self.shared)?.take_frame_capture())
    }
}

/// One frame whose presentation the native system reported complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentedFrame {
    index: u64,
    presented_at: Option<Instant>,
    refresh_interval: Option<Duration>,
}

impl PresentedFrame {
    /// Constructed only by backends with native presentation feedback: Metal drawable presented
    /// handlers and the Vulkan `VK_EXT_present_timing` drain.
    pub(crate) const fn new(index: u64, presented_at: Option<Instant>) -> Self {
        Self {
            index,
            presented_at,
            refresh_interval: None,
        }
    }

    /// Native refresh duration, independent of skipped or discarded frames.
    ///
    /// `None` means the backend cannot report a fixed native refresh duration.
    /// Currently supplied by Vulkan present-timing support; Metal returns `None`.
    #[must_use]
    pub const fn refresh_interval(&self) -> Option<Duration> {
        self.refresh_interval
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub(crate) const fn with_refresh_interval(mut self, interval: Option<Duration>) -> Self {
        self.refresh_interval = interval;
        self
    }
    /// Zero-based position of this frame among the session's presented frames.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// The moment the frame reached the display.
    ///
    /// `None` means the native system reported presentation handling without a display time, such
    /// as while the window is off screen.
    #[must_use]
    pub const fn presented_at(&self) -> Option<Instant> {
        self.presented_at
    }
}

/// Native presentation feedback drained from a [`Surface`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PresentFeedback {
    /// Frames whose native presentation completed since the previous drain, in presentation
    /// order. The list is empty when no new completions have been reported yet.
    Reported(Vec<PresentedFrame>),
    /// This session's backend exposes no native presentation feedback; cadence must be estimated.
    Unsupported,
}

/// One backend-defined region in a submitted frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingScope {
    /// Complete GPU command-buffer work submitted for the frame.
    Frame,
    /// Optional depth-only shadow work.
    Shadow,
    /// Main scene rendering.
    Scene,
    /// Fullscreen post-processing and any overlay encoded into that pass.
    Postprocess,
}

/// Duration of one GPU diagnostic region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuScopeTiming {
    scope: GpuTimingScope,
    duration: Duration,
    render_stages: Option<GpuRenderStageTiming>,
}

/// Optional native render-stage measurements within a GPU timing region.
///
/// Durations sum the individual stage intervals when a region contains several
/// render passes (for example shadow cascades). Stages and passes may overlap;
/// these are elapsed intervals, not additive GPU utilization or CPU wait time.
/// On a tile-based GPU the vertex interval of a pass includes time spent
/// waiting behind the fragment stage of the pass before it, so a long vertex
/// interval on a light pass is the previous pass's cost, not this one's. The
/// fragment interval is the pass's own work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuRenderStageTiming {
    vertex: Duration,
    fragment: Duration,
}

impl GpuRenderStageTiming {
    /// Sum of measured vertex-stage intervals, excluding gaps before fragment work.
    #[must_use]
    pub const fn vertex(&self) -> Duration {
        self.vertex
    }

    /// Sum of measured fragment-stage intervals.
    #[must_use]
    pub const fn fragment(&self) -> Duration {
        self.fragment
    }
}

impl GpuScopeTiming {
    pub(crate) const fn new(scope: GpuTimingScope, duration: Duration) -> Self {
        Self {
            scope,
            duration,
            render_stages: None,
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) const fn with_render_stages(mut self, vertex: Duration, fragment: Duration) -> Self {
        self.render_stages = Some(GpuRenderStageTiming { vertex, fragment });
        self
    }

    /// Native render-stage intervals when the backend exposes them.
    ///
    /// A region's overall duration is what the region added to the frame. These
    /// details show how that time was spent within the passes themselves.
    #[must_use]
    pub const fn render_stages(&self) -> Option<GpuRenderStageTiming> {
        self.render_stages
    }

    /// Region measured by this sample.
    #[must_use]
    pub const fn scope(&self) -> GpuTimingScope {
        self.scope
    }

    /// Elapsed time in the backend's GPU timestamp domain.
    ///
    /// For [`GpuTimingScope::Frame`] this is the whole submission. For the
    /// fixed regions it is the time the region added to the frame: measured
    /// from the previous region finishing to this one finishing, so the
    /// regions of one frame add up rather than overlap. Removing a region's
    /// work should shorten the frame by about this much.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

/// Completed GPU duration data for one submitted and presented frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GpuFrameTiming {
    frame_index: u64,
    scopes: Vec<GpuScopeTiming>,
}

impl GpuFrameTiming {
    pub(crate) const fn new(frame_index: u64, scopes: Vec<GpuScopeTiming>) -> Self {
        Self {
            frame_index,
            scopes,
        }
    }

    /// Zero-based session index shared with [`PresentedFrame::index`].
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Backend-supported regions in recording order.
    ///
    /// Both backends report the complete frame plus the fixed shadow, scene, and postprocess
    /// regions that were present in the submission, when the queue supports region timing.
    #[must_use]
    pub fn scopes(&self) -> &[GpuScopeTiming] {
        &self.scopes
    }
}

/// GPU duration feedback drained from a [`Queue`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GpuTimingFeedback {
    /// Completed samples in submission order; empty when no new sample is ready.
    Reported(Vec<GpuFrameTiming>),
    /// Collection was not requested when the graphics session was opened.
    Disabled,
    /// Collection was requested, but the selected queue exposes no usable timestamp facility.
    Unsupported,
}

/// Fixed vertex layout for the first textured slice.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Linear vertex color multiplier.
    pub color: [f32; 3],
    /// Texture coordinate.
    pub uv: [f32; 2],
}

/// Data format of one vertex attribute in the buffer, and through it the WGSL type the shader
/// reads.
///
/// The 32-bit float, unsigned, and signed families as scalars through four components are read
/// as the WGSL type of the same shape. The packed formats are fetched narrower and widened by the
/// GPU: the unsigned-integer ones (`Uint8x4`, `Uint16x2`, `Uint16x4`) are read as `vec2<u32>` or
/// `vec4<u32>`, and the normalized ones (`Unorm8x4`, `Unorm16x2`, `Unorm16x4`) as `vec2<f32>` or
/// `vec4<f32>` in 0..1, which suits skinning indices and weights. Pipeline creation compares that
/// WGSL type with what `mulciber-shader` recorded for the input. Every attribute's offset must be
/// a multiple of four bytes, as Metal requires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VertexFormat {
    /// One 32-bit float (`f32`).
    Float32,
    /// Two 32-bit floats (`vec2<f32>`).
    Float32x2,
    /// Three 32-bit floats (`vec3<f32>`).
    Float32x3,
    /// Four 32-bit floats (`vec4<f32>`).
    Float32x4,
    /// One unsigned 32-bit integer (`u32`).
    Uint32,
    /// Two unsigned 32-bit integers (`vec2<u32>`).
    Uint32x2,
    /// Three unsigned 32-bit integers (`vec3<u32>`).
    Uint32x3,
    /// Four unsigned 32-bit integers (`vec4<u32>`).
    Uint32x4,
    /// One signed 32-bit integer (`i32`).
    Sint32,
    /// Two signed 32-bit integers (`vec2<i32>`).
    Sint32x2,
    /// Three signed 32-bit integers (`vec3<i32>`).
    Sint32x3,
    /// Four signed 32-bit integers (`vec4<i32>`).
    Sint32x4,
    /// Four unsigned 8-bit integers, read as `vec4<u32>` (four bytes; bone indices).
    Uint8x4,
    /// Four unsigned 8-bit values normalized to 0..1, read as `vec4<f32>` (four bytes; bone
    /// weights, packed colours).
    Unorm8x4,
    /// Two unsigned 16-bit integers, read as `vec2<u32>` (four bytes).
    Uint16x2,
    /// Four unsigned 16-bit integers, read as `vec4<u32>` (eight bytes).
    Uint16x4,
    /// Two unsigned 16-bit values normalized to 0..1, read as `vec2<f32>` (four bytes).
    Unorm16x2,
    /// Four unsigned 16-bit values normalized to 0..1, read as `vec4<f32>` (eight bytes).
    Unorm16x4,
}

impl VertexFormat {
    /// Tightly packed byte size of one attribute value.
    #[must_use]
    pub const fn byte_len(self) -> u32 {
        match self {
            Self::Uint8x4 | Self::Unorm8x4 | Self::Uint16x2 | Self::Unorm16x2 => 4,
            Self::Uint16x4 | Self::Unorm16x4 => 8,
            _ => 4 * (self.components() as u32),
        }
    }

    const fn components(self) -> u8 {
        match self {
            Self::Float32 | Self::Uint32 | Self::Sint32 => 1,
            Self::Float32x2
            | Self::Uint32x2
            | Self::Sint32x2
            | Self::Uint16x2
            | Self::Unorm16x2 => 2,
            Self::Float32x3 | Self::Uint32x3 | Self::Sint32x3 => 3,
            Self::Float32x4
            | Self::Uint32x4
            | Self::Sint32x4
            | Self::Uint8x4
            | Self::Unorm8x4
            | Self::Uint16x4
            | Self::Unorm16x4 => 4,
        }
    }

    /// The `mulciber-shader` interface format code of the WGSL type this format is read as.
    pub(crate) const fn interface_code(self) -> u8 {
        match self {
            Self::Float32 => 0,
            Self::Float32x2 | Self::Unorm16x2 => 1,
            Self::Float32x3 => 2,
            Self::Float32x4 | Self::Unorm8x4 | Self::Unorm16x4 => 3,
            Self::Uint32 => 4,
            Self::Uint32x2 | Self::Uint16x2 => 5,
            Self::Uint32x3 => 6,
            Self::Uint32x4 | Self::Uint8x4 | Self::Uint16x4 => 7,
            Self::Sint32 => 8,
            Self::Sint32x2 => 9,
            Self::Sint32x3 => 10,
            Self::Sint32x4 => 11,
        }
    }

    /// Whether the buffer holds the value at the width the shader reads it, rather than packed.
    const fn is_packed(self) -> bool {
        matches!(
            self,
            Self::Uint8x4
                | Self::Unorm8x4
                | Self::Uint16x2
                | Self::Uint16x4
                | Self::Unorm16x2
                | Self::Unorm16x4
        )
    }

    /// The format's spelling in diagnostics, with the WGSL type a packed format is read as.
    pub(crate) fn describe(self) -> std::string::String {
        let wgsl = self.wgsl_name();
        if self.is_packed() {
            format!("{self:?} (read as {wgsl})")
        } else {
            std::string::String::from(wgsl)
        }
    }

    /// WGSL spelling used in diagnostics; a packed format names the type it is read as.
    pub(crate) const fn wgsl_name(self) -> &'static str {
        match self.interface_code() {
            0 => "f32",
            1 => "vec2<f32>",
            2 => "vec3<f32>",
            3 => "vec4<f32>",
            4 => "u32",
            5 => "vec2<u32>",
            6 => "vec3<u32>",
            7 => "vec4<u32>",
            8 => "i32",
            9 => "vec2<i32>",
            10 => "vec3<i32>",
            _ => "vec4<i32>",
        }
    }

    pub(crate) const fn from_interface_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Float32,
            1 => Self::Float32x2,
            2 => Self::Float32x3,
            3 => Self::Float32x4,
            4 => Self::Uint32,
            5 => Self::Uint32x2,
            6 => Self::Uint32x3,
            7 => Self::Uint32x4,
            8 => Self::Sint32,
            9 => Self::Sint32x2,
            10 => Self::Sint32x3,
            11 => Self::Sint32x4,
            _ => return None,
        })
    }
}

/// One attribute inside an application-described vertex layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VertexAttribute {
    /// Shader input location this attribute feeds.
    pub location: u32,
    /// Data format at `offset`.
    pub format: VertexFormat,
    /// Byte offset from the start of one vertex.
    pub offset: u32,
}

/// An application-described per-vertex data layout.
///
/// The layout is declared once at material pipeline creation and once per mesh uploaded from raw
/// vertex bytes; a draw whose mesh and pipeline layouts differ is rejected. Meshes uploaded
/// through the fixed [`Vertex`] path carry [`VertexLayout::VERTEX`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VertexLayout<'attributes> {
    /// Byte distance between consecutive vertices.
    pub stride: u32,
    /// Attributes consumed from each vertex.
    pub attributes: &'attributes [VertexAttribute],
}

impl VertexLayout<'_> {
    /// The fixed layout of [`Vertex`]: position, color, and texture coordinate at locations 0
    /// through 2.
    pub const VERTEX: VertexLayout<'static> = VertexLayout {
        stride: 32,
        attributes: &[
            VertexAttribute {
                location: 0,
                format: VertexFormat::Float32x3,
                offset: 0,
            },
            VertexAttribute {
                location: 1,
                format: VertexFormat::Float32x3,
                offset: 12,
            },
            VertexAttribute {
                location: 2,
                format: VertexFormat::Float32x2,
                offset: 24,
            },
        ],
    };

    fn to_owned_layout(self) -> OwnedVertexLayout {
        let mut attributes: Vec<VertexAttribute> = self.attributes.to_vec();
        attributes.sort_unstable_by_key(|attribute| attribute.location);
        OwnedVertexLayout {
            stride: self.stride,
            attributes,
        }
    }
}

/// A location-sorted owned copy of a declared vertex layout, kept on meshes and material
/// pipelines so submission can check their compatibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OwnedVertexLayout {
    pub(crate) stride: u32,
    pub(crate) attributes: Vec<VertexAttribute>,
}

/// Index data for mesh creation.
///
/// `U16` covers meshes whose vertex count fits sixteen bits; `U32` removes that bound for
/// workloads such as chunked or merged geometry.
#[derive(Clone, Copy, Debug)]
pub enum MeshIndices<'indices> {
    /// 16-bit indices.
    U16(&'indices [u16]),
    /// 32-bit indices.
    U32(&'indices [u32]),
}

impl MeshIndices<'_> {
    /// Number of indices.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::U16(indices) => indices.len(),
            Self::U32(indices) => indices.len(),
        }
    }

    /// Whether no indices are present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn out_of_range(&self, vertex_count: usize) -> bool {
        match *self {
            // The largest index decides; `max` vectorises where an early-out `any` can't.
            Self::U16(indices) => indices
                .iter()
                .copied()
                .max()
                .is_some_and(|index| usize::from(index) >= vertex_count),
            Self::U32(indices) => indices.iter().copied().max().is_some_and(|index| {
                usize::try_from(index).map_or(true, |index| index >= vertex_count)
            }),
        }
    }

    pub(crate) const fn byte_len(&self) -> usize {
        match *self {
            Self::U16(indices) => indices.len() * 2,
            Self::U32(indices) => indices.len() * 4,
        }
    }

    const fn byte_len_checked(&self) -> Option<usize> {
        match *self {
            Self::U16(indices) => indices.len().checked_mul(2),
            Self::U32(indices) => indices.len().checked_mul(4),
        }
    }
}

fn validate_mesh_parts(
    vertex_count: usize,
    parts: &[MeshIndices<'_>],
) -> Result<u32, GraphicsError> {
    if parts.is_empty() {
        return Err(GraphicsError::invalid_request(
            "mesh must supply at least one indexed part",
        ));
    }
    let part_count = u32::try_from(parts.len())
        .map_err(|_| GraphicsError::invalid_request("mesh part count exceeds u32"))?;
    for (part_index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(GraphicsError::invalid_request(format!(
                "mesh index part {part_index} must be non-empty"
            )));
        }
        if part.out_of_range(vertex_count) {
            return Err(GraphicsError::invalid_request(format!(
                "mesh index part {part_index} contains an out-of-range index"
            )));
        }
        u32::try_from(part.len()).map_err(|_| {
            GraphicsError::invalid_request(format!(
                "mesh index part {part_index} count exceeds u32"
            ))
        })?;
        part.byte_len_checked().ok_or_else(|| {
            GraphicsError::invalid_request(format!(
                "mesh index part {part_index} byte length overflows"
            ))
        })?;
    }
    Ok(part_count)
}

/// Uploaded indexed geometry.
#[derive(Debug, Eq, PartialEq)]
pub struct Mesh {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    part_count: u32,
}

impl Mesh {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }

    /// Number of immutable indexed parts that share this mesh's vertex storage.
    #[must_use]
    pub const fn part_count(&self) -> usize {
        self.part_count as usize
    }

    /// Borrows one immutable indexed part of this mesh.
    ///
    /// The returned value owns no resource lease or GPU allocation. Its parent mesh must remain
    /// alive for the duration of the borrow, and all session, layout, and generational validation
    /// continues to use that parent.
    ///
    /// # Errors
    ///
    /// Returns an invalid-request error when `index` is outside `0..self.part_count()`.
    pub fn part(&self, index: usize) -> Result<MeshPart<'_>, GraphicsError> {
        if index >= self.part_count() {
            return Err(GraphicsError::invalid_request(format!(
                "mesh part index {index} is out of range for {} parts",
                self.part_count()
            )));
        }
        let index = u32::try_from(index).map_err(|_| {
            GraphicsError::invalid_request("mesh part index exceeds the supported u32 range")
        })?;
        Ok(MeshPart { mesh: self, index })
    }
}

/// Lightweight borrowed selection of one immutable index part in a parent [`Mesh`].
///
/// A mesh part contains no resource lease and creates no independent destruction or reclamation
/// entry. Dropping or explicitly destroying the parent retires its shared vertex storage and every
/// index part together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshPart<'mesh> {
    mesh: &'mesh Mesh,
    index: u32,
}

impl<'mesh> MeshPart<'mesh> {
    /// Parent mesh that owns this part's storage and identity.
    #[must_use]
    pub const fn mesh(self) -> &'mesh Mesh {
        self.mesh
    }

    /// Zero-based part number within the parent mesh.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }

    pub(crate) const fn index_u32(self) -> u32 {
        self.index
    }
}

/// Frame-transient indexed geometry supplied inline with one material record.
///
/// The bytes are copied into the session's frame-transient geometry region at submission, so the
/// application rebuilds them freely every frame — HUD text, gauges, debug lines, and other
/// per-frame-authored geometry — without creating or destroying [`Mesh`] resources. The vertex
/// bytes follow the record's pipeline-declared vertex layout; the application owns that layout
/// correctness exactly as it owns uniform memory layout.
#[derive(Clone, Copy)]
pub struct TransientGeometry<'resources> {
    /// Raw vertex bytes, a non-zero multiple of the pipeline's declared layout stride.
    pub vertices: &'resources [u8],
    /// Non-empty indices into the supplied vertices.
    pub indices: MeshIndices<'resources>,
}

/// Geometry supply for one material record.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum GeometrySource<'resources> {
    /// Uploaded geometry whose retained vertex layout must match the pipeline's declaration.
    Mesh(&'resources Mesh),
    /// One immutable indexed part borrowing an uploaded parent mesh.
    MeshPart(MeshPart<'resources>),
    /// Frame-transient geometry staged with this submission against the pipeline's declaration.
    Transient(TransientGeometry<'resources>),
}

impl<'resources> GeometrySource<'resources> {
    pub(crate) const fn uploaded_mesh(self) -> Option<&'resources Mesh> {
        match self {
            Self::Mesh(mesh) => Some(mesh),
            Self::MeshPart(part) => Some(part.mesh()),
            Self::Transient(_) => None,
        }
    }
}

/// Uploaded mesh geometry selected by one shadow record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshSource<'resources> {
    /// The parent mesh's default part (part zero).
    Mesh(&'resources Mesh),
    /// One explicitly selected immutable indexed part.
    MeshPart(MeshPart<'resources>),
}

impl<'resources> MeshSource<'resources> {
    pub(crate) const fn mesh(self) -> &'resources Mesh {
        match self {
            Self::Mesh(mesh) => mesh,
            Self::MeshPart(part) => part.mesh(),
        }
    }

    pub(crate) const fn part_index(self) -> u32 {
        match self {
            Self::Mesh(_) => 0,
            Self::MeshPart(part) => part.index_u32(),
        }
    }
}

/// Uploaded sampled texture: RGBA8 sRGB, RGBA8 UNORM, block-compressed, or linear
/// `RGBA16Float`, according to its creation API, and one 2D image, a cube of six square faces,
/// or an array of such cubes ([`Texture::dimension`]).
#[derive(Debug, Eq, PartialEq)]
pub struct Texture {
    lease: ResourceLease,
    dimension: TextureDimension,
    /// Whether the texture is a [`RenderTexture`]'s color, written by offscreen passes rather
    /// than uploads.
    render_target: bool,
}

/// The shape of an uploaded [`Texture`], which decides the material slot it can feed.
///
/// A 2D texture feeds a [`MaterialBinding::Texture`] slot (WGSL `texture_2d<f32>`) and the
/// fixed textured pipelines; a cube texture feeds a [`MaterialBinding::CubeTexture`] slot (WGSL
/// `texture_cube<f32>`) and a cube texture array a [`MaterialBinding::CubeTextureArray`] slot
/// (WGSL `texture_cube_array<f32>`), and nothing else.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TextureDimension {
    /// One 2D image with its mip chain.
    D2,
    /// Six square faces of one extent, each with its mip chain, in the layer order +X, -X, +Y,
    /// -Y, +Z, -Z.
    Cube,
    /// One or more cubes of one extent, each laid out as [`TextureDimension::Cube`], the shader
    /// choosing a cube by its array index.
    CubeArray,
}

impl TextureDimension {
    const fn label(self) -> &'static str {
        match self {
            Self::D2 => "2D texture",
            Self::Cube => "cube texture",
            Self::CubeArray => "cube texture array",
        }
    }
}

/// A block-compressed texture encoding the GPU samples directly.
///
/// Every encoding here packs a 4×4 texel block: BC1 into eight bytes (an eighth of its RGBA8
/// equivalent), the others into sixteen (a quarter). BC7 carries four channels and suits
/// colour, with or without the sRGB transfer function; BC5 carries two and suits a
/// tangent-space normal whose Z is reconstructed in the shader. BC1, BC2 and BC3 are the
/// older DXT1, DXT3 and DXT5 encodings that existing game data ships in: BC1 has one-bit
/// alpha, BC2 explicit four-bit alpha, BC3 interpolated alpha. The encoder is the
/// application's: Mulciber uploads blocks and never decodes or produces them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BlockCompression {
    /// BC7 RGBA, decoded through the sRGB transfer function when sampled.
    Bc7Srgb,
    /// BC7 RGBA, sampled as stored.
    Bc7Unorm,
    /// BC5 two-channel UNORM; the shader reads `.rg` and a sample's `.ba` are undefined.
    Bc5Unorm,
    /// BC1 (DXT1) RGBA with one-bit alpha, decoded through the sRGB transfer function.
    Bc1Srgb,
    /// BC1 (DXT1) RGBA with one-bit alpha, sampled as stored.
    Bc1Unorm,
    /// BC2 (DXT3) RGBA with explicit alpha, decoded through the sRGB transfer function.
    Bc2Srgb,
    /// BC2 (DXT3) RGBA with explicit alpha, sampled as stored.
    Bc2Unorm,
    /// BC3 (DXT5) RGBA with interpolated alpha, decoded through the sRGB transfer function.
    Bc3Srgb,
    /// BC3 (DXT5) RGBA with interpolated alpha, sampled as stored.
    Bc3Unorm,
}

impl BlockCompression {
    pub(crate) const fn sampled(self) -> SampledTextureFormat {
        match self {
            Self::Bc7Srgb => SampledTextureFormat::Bc7Srgb,
            Self::Bc7Unorm => SampledTextureFormat::Bc7Unorm,
            Self::Bc5Unorm => SampledTextureFormat::Bc5Unorm,
            Self::Bc1Srgb => SampledTextureFormat::Bc1Srgb,
            Self::Bc1Unorm => SampledTextureFormat::Bc1Unorm,
            Self::Bc2Srgb => SampledTextureFormat::Bc2Srgb,
            Self::Bc2Unorm => SampledTextureFormat::Bc2Unorm,
            Self::Bc3Srgb => SampledTextureFormat::Bc3Srgb,
            Self::Bc3Unorm => SampledTextureFormat::Bc3Unorm,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SampledTextureFormat {
    Srgb,
    Unorm,
    Float16,
    Bc7Srgb,
    Bc7Unorm,
    Bc5Unorm,
    Bc1Srgb,
    Bc1Unorm,
    Bc2Srgb,
    Bc2Unorm,
    Bc3Srgb,
    Bc3Unorm,
}

impl SampledTextureFormat {
    /// Texels along each side of the unit the format stores: one for an uncompressed
    /// format, four for a block-compressed one.
    pub(crate) const fn block_extent(self) -> u32 {
        match self {
            Self::Srgb | Self::Unorm | Self::Float16 => 1,
            Self::Bc7Srgb
            | Self::Bc7Unorm
            | Self::Bc5Unorm
            | Self::Bc1Srgb
            | Self::Bc1Unorm
            | Self::Bc2Srgb
            | Self::Bc2Unorm
            | Self::Bc3Srgb
            | Self::Bc3Unorm => 4,
        }
    }

    /// Bytes one stored unit takes: a texel for an uncompressed format, a block otherwise.
    pub(crate) const fn block_bytes(self) -> usize {
        match self {
            Self::Srgb | Self::Unorm => 4,
            Self::Float16 | Self::Bc1Srgb | Self::Bc1Unorm => 8,
            Self::Bc7Srgb
            | Self::Bc7Unorm
            | Self::Bc5Unorm
            | Self::Bc2Srgb
            | Self::Bc2Unorm
            | Self::Bc3Srgb
            | Self::Bc3Unorm => 16,
        }
    }

    pub(crate) const fn is_block_compressed(self) -> bool {
        self.block_extent() > 1
    }

    /// Stored units along one axis of a level `extent` texels long, rounding a partial
    /// block up: a compressed level always carries whole blocks.
    pub(crate) const fn blocks_along(self, extent: u32) -> u32 {
        extent.div_ceil(self.block_extent())
    }

    /// Tightly packed bytes in one row of blocks (or texels) of a level `width` texels wide.
    pub(crate) fn row_bytes(self, width: u32) -> Option<usize> {
        usize::try_from(self.blocks_along(width))
            .ok()?
            .checked_mul(self.block_bytes())
    }

    /// Tightly packed bytes in a whole level of the given extent.
    pub(crate) fn level_bytes(self, width: u32, height: u32) -> Option<usize> {
        self.row_bytes(width)?
            .checked_mul(usize::try_from(self.blocks_along(height)).ok()?)
    }
}

impl Texture {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }

    /// Whether this is a 2D texture, a cube texture or a cube texture array, fixed by its
    /// creation API.
    #[must_use]
    pub const fn dimension(&self) -> TextureDimension {
        self.dimension
    }
}

/// A 2D linear `RGBA16Float` color texture that a scene submission's offscreen passes render
/// into, then sampled through [`RenderTexture::texture`] like any uploaded 2D texture.
///
/// It owns depth and, at a multisample count, multisample color storage of its own, built for
/// the session's sample count when it was created; HDR material pipelines built for the same
/// count draw into it. Its contents persist until the next offscreen pass into it, so one
/// render can be sampled for many frames.
#[derive(Debug, Eq, PartialEq)]
pub struct RenderTexture {
    texture: Texture,
    width: u32,
    height: u32,
}

impl RenderTexture {
    /// The rendered color, for material records' texture slots.
    #[must_use]
    pub const fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Width in texels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in texels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// One offscreen material pass of a [`SceneSubmission`]: records drawn into a
/// [`RenderTexture`] with its own clear, after any shadow prepass and before the scene pass.
#[derive(Clone, Copy)]
pub struct OffscreenPass<'resources> {
    /// Destination; at most one pass per submission may target it.
    pub target: &'resources RenderTexture,
    /// Non-empty records encoded in slice order. Their pipelines must be HDR material pipelines
    /// and may not sample scene depth or the target itself.
    pub records: &'resources [MaterialRecord<'resources>],
    /// Linear color the target is cleared to before the first record.
    pub clear: ClearColor,
}

/// Native textured depth-tested graphics pipeline.
#[derive(Debug, Eq, PartialEq)]
pub struct TexturedPipeline {
    lease: ResourceLease,
}

/// Native textured depth-tested graphics pipeline with per-instance matrix input.
#[derive(Debug, Eq, PartialEq)]
pub struct InstancedTexturedPipeline {
    lease: ResourceLease,
}

impl InstancedTexturedPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

impl TexturedPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// Single-sample fullscreen pipeline that samples resolved scene color and optionally consumes
/// application-authored per-submission uniform data.
#[derive(Debug, Eq, PartialEq)]
pub struct PostprocessPipeline {
    hdr: bool,
    lease: ResourceLease,
    /// Zero when the pipeline declares no postprocess uniform.
    uniform_size: u32,
}

/// Largest supported postprocess uniform declaration in bytes.
///
/// Postprocess uniform data is transient for one submission and follows the same bounded model as
/// material uniform data, but backends keep its storage independent from scene/material uniforms.
pub const POSTPROCESS_UNIFORM_SIZE_LIMIT: u32 = 512;

/// Everything needed to create one fullscreen postprocess pipeline.
///
/// The pipeline always uses `post_vertex` and `post_fragment`, resolved scene color at group 0,
/// binding 1, and its pipeline-owned sampler at group 0, binding 2. `uniform_size` declares an
/// optional application-authored uniform at group 0, binding 0.
#[derive(Clone, Copy)]
pub struct PostprocessPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing `post_vertex` and `post_fragment`.
    pub shader: ShaderArtifact<'inputs>,
    /// Exact byte size of the optional group-0/binding-0 uniform, from 1 through
    /// [`POSTPROCESS_UNIFORM_SIZE_LIMIT`]; `None` declares no postprocess uniform.
    pub uniform_size: Option<u32>,
}

impl<'inputs> From<ShaderArtifact<'inputs>> for PostprocessPipelineDescriptor<'inputs> {
    fn from(shader: ShaderArtifact<'inputs>) -> Self {
        Self {
            shader,
            uniform_size: None,
        }
    }
}

/// Largest supported material uniform declaration in bytes.
///
/// Material uniform data flows through the session's per-draw uniform region, whose stride is
/// this size in both backends, so it caps one declaration. It stays a multiple of 256 bytes, the
/// largest dynamic uniform offset alignment Vulkan permits and Metal's buffer-offset alignment.
pub const MATERIAL_UNIFORM_SIZE_LIMIT: u32 = 512;
const _: () = assert!(MATERIAL_UNIFORM_SIZE_LIMIT.is_multiple_of(256));

/// Largest supported read-only storage declaration in bytes.
///
/// Sixty-four kibibytes holds a thousand and twenty-four `mat4x4<f32>` bone matrices, well past
/// any palette the skinned-record slice needs, while keeping the frame-transient storage region
/// bounded.
pub const MATERIAL_STORAGE_SIZE_LIMIT: u32 = 65536;

/// Largest supported frame-transient geometry supply in bytes, vertices and indices combined.
///
/// Four mebibytes stages past a hundred thousand fixed-layout vertices — far beyond any
/// practical per-record HUD or debug overlay — while keeping the frame-transient geometry
/// region bounded, and it caps the index count well inside the native draw-call range.
pub const TRANSIENT_GEOMETRY_SIZE_LIMIT: u32 = 4_194_304;

/// Largest supported per-record instance supply in bytes.
///
/// Four mebibytes carries 65,536 four-by-four float matrices in one record.
/// Larger supplies must be split between records, on whole-instance boundaries;
/// this limit applies independently to material and shadow records.
///
/// ```
/// use mulciber::INSTANCE_SUPPLY_SIZE_LIMIT;
///
/// let stride = 40_usize;
/// let batch_bytes = INSTANCE_SUPPLY_SIZE_LIMIT as usize / stride * stride;
/// assert!(batch_bytes > 0);
/// assert!(batch_bytes <= INSTANCE_SUPPLY_SIZE_LIMIT as usize);
/// assert_eq!(batch_bytes % stride, 0);
/// ```
pub const INSTANCE_SUPPLY_SIZE_LIMIT: u32 = 4_194_304;

/// Largest supported material sampler slot and vertex attribute location.
///
/// Metal guarantees sixteen sampler-state slots per stage and Vulkan sixteen vertex attributes.
/// A WGSL binding number is its native index, and textures and buffers live in namespaces of
/// their own, so their slots are capped separately by [`MATERIAL_TEXTURE_SLOT_LIMIT`] and
/// [`MATERIAL_BUFFER_SLOT_LIMIT`].
pub const MATERIAL_SLOT_LIMIT: u32 = 15;

/// Largest supported material texture slot, sampled or depth.
///
/// Metal's texture argument table holds 31 entries per stage on every GPU family.
pub const MATERIAL_TEXTURE_SLOT_LIMIT: u32 = 30;

/// Largest supported material uniform or storage slot.
///
/// Metal's buffer argument table holds 31 entries per stage, and the backend feeds per-vertex and
/// per-instance data through the top two.
pub const MATERIAL_BUFFER_SLOT_LIMIT: u32 = 28;

/// Most textures, sampled and depth together, one material pipeline may declare.
///
/// Vulkan guarantees sixteen sampled images per shader stage.
pub const MATERIAL_TEXTURE_COUNT_LIMIT: u32 = 16;

/// Largest supported shadow map extent along either axis.
pub const SHADOW_MAP_SIZE_LIMIT: u32 = 8192;

/// Largest supported render texture extent along either axis.
pub const RENDER_TEXTURE_SIZE_LIMIT: u32 = 8192;

/// Largest supported shadow map array layer count.
///
/// Eight layers covers every practical cascade scheme while keeping the per-frame layered
/// pre-pass bounded; cascade policy itself (split distances, per-cascade matrices, selection)
/// stays application-owned.
pub const SHADOW_MAP_LAYER_LIMIT: u32 = 8;

/// Minification and magnification filtering for one material sampler slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerFilter {
    /// Nearest-texel sampling, keeping texel edges crisp (pixel art, texture atlases).
    Nearest,
    /// Linear interpolation between adjacent texels and between mip levels.
    Linear,
}

/// Texture-coordinate addressing for one material sampler slot, applied on every axis by
/// [`MaterialBinding::Sampler`] or on one axis of a [`SamplerAddressPerAxis`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplerAddress {
    /// Coordinates wrap, tiling the texture.
    Repeat,
    /// Coordinates clamp to the edge texel.
    ClampToEdge,
}

/// Texture-coordinate addressing chosen separately for each axis of one material sampler slot,
/// declared through [`MaterialBinding::SamplerPerAxis`].
///
/// `u` and `v` address a 2D texture's horizontal and vertical coordinates; `w` is the third
/// coordinate, which 2D textures never read. An equirectangular panorama, for instance, wraps
/// round the horizon and clamps at the poles:
///
/// ```
/// # use mulciber::{SamplerAddress, SamplerAddressPerAxis};
/// let panorama = SamplerAddressPerAxis {
///     v: SamplerAddress::ClampToEdge,
///     ..SamplerAddressPerAxis::all(SamplerAddress::Repeat)
/// };
/// assert_eq!(panorama.u, SamplerAddress::Repeat);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SamplerAddressPerAxis {
    /// Addressing of the first (horizontal) coordinate.
    pub u: SamplerAddress,
    /// Addressing of the second (vertical) coordinate.
    pub v: SamplerAddress,
    /// Addressing of the third coordinate.
    pub w: SamplerAddress,
}

impl SamplerAddressPerAxis {
    /// The same addressing on every axis, as [`MaterialBinding::Sampler`] applies it.
    #[must_use]
    pub const fn all(address: SamplerAddress) -> Self {
        Self {
            u: address,
            v: address,
            w: address,
        }
    }
}

impl From<SamplerAddress> for SamplerAddressPerAxis {
    fn from(address: SamplerAddress) -> Self {
        Self::all(address)
    }
}

/// How a material pipeline's fragment output combines with the color target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendMode {
    /// Fragment color replaces the target color; fragment alpha is ignored.
    Opaque,
    /// Fragment alpha drives multisample coverage (alpha-to-coverage), keeping depth writes
    /// order-independent for hard-edged transparency such as foliage cutouts. At one sample
    /// this degrades to a hard alpha threshold.
    Cutout,
    /// Premultiplied source-over blending: `target = source + (1 - source.a) * target`.
    ///
    /// Translucent records blend against whatever the target already holds, so the application
    /// orders them after the opaque records they should composite over.
    PremultipliedTranslucent,
}

/// How a material pipeline interacts with the scene depth target.
///
/// The testing modes come in two compare directions. `TestWrite` and `TestOnly` use the
/// conventional less-than compare against a depth target cleared to the far plane (1.0).
/// `TestWriteGreater` and `TestOnlyGreater` use a greater-than compare against a depth target
/// cleared to 0.0, for reversed-Z projections that map the near plane to depth 1.0 — the
/// standard fix for far-field precision collapse on the float depth target. The projection
/// matrix that produces reversed-Z clip depth stays application-owned.
///
/// A scene submission derives its depth-clear value from its records' declared modes, so one
/// submission may not mix less-compare and greater-compare testing modes; `Off` composes with
/// either direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthMode {
    /// Test against the depth target and write surviving fragment depth (opaque geometry).
    TestWrite,
    /// Test against the depth target without writing (translucents occluded by opaque geometry).
    TestOnly,
    /// Test with a greater-than compare and write surviving fragment depth (opaque geometry
    /// under a reversed-Z projection).
    TestWriteGreater,
    /// Test with a greater-than compare without writing (translucents under a reversed-Z
    /// projection).
    TestOnlyGreater,
    /// Neither test nor write (skyboxes drawn first, overlays drawn last).
    Off,
}

impl DepthMode {
    /// Whether this mode tests with the conventional less-than compare.
    pub(crate) const fn tests_less(self) -> bool {
        matches!(self, Self::TestWrite | Self::TestOnly)
    }

    /// Whether this mode tests with the reversed-Z greater-than compare.
    pub(crate) const fn tests_greater(self) -> bool {
        matches!(self, Self::TestWriteGreater | Self::TestOnlyGreater)
    }
}

/// One declared sampler slot handed to the native backends.
#[derive(Clone, Copy)]
pub(crate) struct SamplerSlot {
    pub(crate) binding: u32,
    pub(crate) filter: SamplerFilter,
    pub(crate) address: SamplerAddressPerAxis,
}

/// One resource slot declared by a material pipeline, identified by its WGSL binding number in
/// group 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaterialBinding {
    /// Application-defined uniform data supplied as bytes with each draw record.
    ///
    /// At most one uniform slot may be declared, `size` must match the WGSL struct size recorded
    /// in the shader artifact, and it may not exceed [`MATERIAL_UNIFORM_SIZE_LIMIT`].
    Uniform {
        /// WGSL binding number.
        binding: u32,
        /// Byte length of the uniform data supplied with each record.
        size: u32,
    },
    /// Application-defined read-only storage data (`var<storage, read>`) supplied as bytes with
    /// each draw record, sized by its creation-fixed WGSL type (typically a bone-matrix array).
    ///
    /// At most one storage slot may be declared, `size` must match the WGSL type size recorded
    /// in the shader artifact, and it may not exceed [`MATERIAL_STORAGE_SIZE_LIMIT`].
    Storage {
        /// WGSL binding number.
        binding: u32,
        /// Byte length of the storage data supplied with each record.
        size: u32,
    },
    /// One sampled 2D color texture supplied with each draw record.
    Texture {
        /// WGSL binding number.
        binding: u32,
    },
    /// One sampled cube texture (WGSL `texture_cube<f32>`) supplied with each draw record from a
    /// [`Texture`] whose [`Texture::dimension`] is [`TextureDimension::Cube`].
    ///
    /// It shares the texture slots and [`MATERIAL_TEXTURE_COUNT_LIMIT`] with
    /// [`MaterialBinding::Texture`], and the record supplies it in the same `textures` list in
    /// ascending binding order. A 2D texture in a cube slot, or a cube texture in a 2D slot, is
    /// rejected at submission. Sample it through an ordinary [`MaterialBinding::Sampler`]; the
    /// direction need not be normalized. Shadow pipelines do not accept cube slots.
    CubeTexture {
        /// WGSL binding number.
        binding: u32,
    },
    /// One sampled cube texture array (WGSL `texture_cube_array<f32>`) supplied with each draw
    /// record from a [`Texture`] whose [`Texture::dimension`] is [`TextureDimension::CubeArray`],
    /// such as one reflection probe per room bound in a single draw.
    ///
    /// It shares the texture slots, [`MATERIAL_TEXTURE_COUNT_LIMIT`] and the record's `textures`
    /// list with [`MaterialBinding::Texture`] and [`MaterialBinding::CubeTexture`]; a texture of
    /// another dimension in this slot, or a cube array in another slot, is rejected at
    /// submission. Sample it through an ordinary [`MaterialBinding::Sampler`] with
    /// `textureSampleLevel(map, sampler, direction, layer, lod)`; an out-of-range layer reads
    /// undefined values on both backends. Creating a pipeline that declares it is `Unsupported`
    /// on a device without cube-array sampling, and shadow pipelines do not accept it.
    CubeTextureArray {
        /// WGSL binding number.
        binding: u32,
    },
    /// A pipeline-owned sampler with the declared filter and address modes.
    Sampler {
        /// WGSL binding number.
        binding: u32,
        /// Minification and magnification filtering.
        filter: SamplerFilter,
        /// Texture-coordinate addressing on every axis.
        address: SamplerAddress,
    },
    /// A pipeline-owned sampler like [`MaterialBinding::Sampler`] whose addressing is chosen
    /// separately for each axis, such as repeating horizontally and clamping vertically.
    SamplerPerAxis {
        /// WGSL binding number.
        binding: u32,
        /// Minification and magnification filtering.
        filter: SamplerFilter,
        /// Texture-coordinate addressing per axis.
        address: SamplerAddressPerAxis,
    },
    /// A depth snapshot of preceding world records, at the scene's sample count.
    ///
    /// The first record declaring this slot ends the opaque pass. Subsequent world
    /// records may test depth but may not write it. Requires HDR postprocessed material
    /// output; unavailable in foreground, overlay and shadow passes. The engine supplies
    /// the texture, so it occupies neither `textures` nor `shadow_map` on the record.
    /// Depth is in the scene's native 0..1 projection (including reversed Z), at render
    /// scale with top-left texel origin. Declare `texture_depth_2d` at one sample or
    /// `texture_depth_multisampled_2d` at more, matching the count in use when the pipeline
    /// is created ([`DeviceSelection::sample_count`] or [`Device::set_sample_count`]).
    /// Use `textureLoad` with fragment pixel coordinates; MSAA reduction is shader-owned.
    /// The snapshot is captured once per submission and survives foreground depth clears.
    SceneDepth {
        /// WGSL binding number; at most one scene-depth slot per pipeline.
        binding: u32,
    },
    /// One sampled `texture_depth_2d` supplied per draw record from a [`ShadowMap`].
    ///
    /// At most one depth-texture slot — plain or arrayed — may be declared per material
    /// pipeline.
    DepthTexture {
        /// WGSL binding number.
        binding: u32,
    },
    /// One sampled `texture_depth_2d_array` supplied per draw record from a
    /// [`ShadowMapArray`], typically holding one shadow cascade per layer.
    ///
    /// At most one depth-texture slot — plain or arrayed — may be declared per material
    /// pipeline.
    DepthTextureArray {
        /// WGSL binding number.
        binding: u32,
    },
    /// A pipeline-owned `sampler_comparison` with fixed shadow-recipe state: linear filtering,
    /// clamp-to-edge addressing, and a less-or-equal comparison, so
    /// `textureSampleCompare(map, sampler, uv, reference)` returns one where the reference depth
    /// is at most the stored depth. Depth bias stays application-owned in the authored shader.
    ///
    /// At most one comparison-sampler slot may be declared per material pipeline.
    ComparisonSampler {
        /// WGSL binding number.
        binding: u32,
    },
}

impl MaterialBinding {
    /// The WGSL `@binding` number this declaration names.
    const fn slot(&self) -> u32 {
        match *self {
            Self::Uniform { binding, .. }
            | Self::Storage { binding, .. }
            | Self::Texture { binding }
            | Self::CubeTexture { binding }
            | Self::CubeTextureArray { binding }
            | Self::Sampler { binding, .. }
            | Self::SamplerPerAxis { binding, .. }
            | Self::SceneDepth { binding }
            | Self::DepthTexture { binding }
            | Self::DepthTextureArray { binding }
            | Self::ComparisonSampler { binding } => binding,
        }
    }
}

/// Everything needed to create one application-authored material pipeline.
#[derive(Clone, Copy)]
pub struct MaterialPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing both entry points.
    pub shader: ShaderArtifact<'inputs>,
    /// Vertex entry point name.
    pub vertex_entry: &'inputs str,
    /// Fragment entry point name.
    pub fragment_entry: &'inputs str,
    /// Per-vertex input layout; together with any instance layout it must match the vertex
    /// entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
    /// Optional per-instance input layout fed from each record's instance supply at
    /// instance-stepping rate.
    ///
    /// A location may appear in the vertex layout or the instance layout but not both, and the
    /// two layouts together must match the vertex entry point's recorded inputs exactly. A
    /// pipeline declaring an instance layout draws each record once per supplied instance
    /// (typically a column-major model or model-view-projection matrix as four `vec4<f32>`
    /// locations), indexed implicitly by the instance-rate attributes.
    pub instance_layout: Option<VertexLayout<'inputs>>,
    /// Declared resource slots; must match the module's recorded bindings.
    pub bindings: &'inputs [MaterialBinding],
    /// How fragment output combines with the color target.
    pub blend: BlendMode,
    /// How the pipeline interacts with the scene depth target.
    pub depth: DepthMode,
}

impl MaterialPipelineDescriptor<'_> {
    /// Checks the declaration against the artifact's recorded interface exactly as
    /// [`Device::create_material_pipeline`] does, without a device.
    ///
    /// A test can hold an application's vertex layouts, binding list and uniform and storage
    /// sizes to the compiled shader this way. Passing does not promise native creation succeeds:
    /// the device can still refuse the module or run out of memory.
    ///
    /// # Errors
    ///
    /// Returns the declaration errors [`Device::create_material_pipeline`] reports.
    pub fn validate(&self) -> Result<(), GraphicsError> {
        check_material_descriptor(*self, false).map(drop)
    }

    /// Checks the declaration as [`Device::create_hdr_material_pipeline`] does, without a
    /// device; unlike [`Self::validate`] it accepts a [`MaterialBinding::SceneDepth`] slot.
    ///
    /// # Errors
    ///
    /// Returns the declaration errors [`Device::create_hdr_material_pipeline`] reports.
    pub fn validate_hdr(&self) -> Result<(), GraphicsError> {
        check_material_descriptor(*self, true).map(drop)
    }
}

/// The device-independent half of material pipeline creation: the declaration checked against
/// the artifact's recorded interface.
fn check_material_descriptor(
    descriptor: MaterialPipelineDescriptor<'_>,
    hdr: bool,
) -> Result<
    (
        OwnedVertexLayout,
        Option<OwnedVertexLayout>,
        BindingDeclaration,
    ),
    GraphicsError,
> {
    let layout = validate_vertex_layout(descriptor.vertex_layout)?;
    let instance_layout = descriptor
        .instance_layout
        .map(|instance| validate_instance_layout(&layout, instance))
        .transpose()?;
    let interface = descriptor.shader.parse_interface();
    let vertex_entry = find_entry_point(
        &interface,
        descriptor.vertex_entry,
        shader::INTERFACE_STAGE_VERTEX,
        "vertex",
    )?;
    let fragment_entry = find_entry_point(
        &interface,
        descriptor.fragment_entry,
        shader::INTERFACE_STAGE_FRAGMENT,
        "fragment",
    )?;
    validate_layouts_against_entry(&layout, instance_layout.as_ref(), vertex_entry)?;
    let declaration = validate_entry_point_bindings(
        descriptor.bindings,
        &interface,
        &[vertex_entry, fragment_entry],
    )?;
    if declaration.scene_depth.is_some()
        && (!hdr
            || matches!(
                descriptor.depth,
                DepthMode::TestWrite | DepthMode::TestWriteGreater
            ))
    {
        return Err(GraphicsError::invalid_request(
            "scene-depth materials require HDR and must not write depth",
        ));
    }
    Ok((layout, instance_layout, declaration))
}

/// Application-authored material pipeline with declared blend and depth modes.
#[derive(Debug, Eq, PartialEq)]
pub struct MaterialPipeline {
    pub(crate) scene_depth: bool,
    hdr: bool,
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    /// Zero when no uniform slot is declared.
    uniform_size: u32,
    /// Zero when no storage slot is declared.
    storage_size: u32,
    /// Zero when no instance layout is declared.
    instance_stride: u32,
    /// The shape each declared texture slot samples, in ascending binding order.
    texture_dimensions: Vec<TextureDimension>,
    /// The kind of depth-texture slot the pipeline declares, fed per record.
    shadow_slot: Option<ShadowSlotKind>,
    /// Declared depth mode, retained so a scene submission can derive its depth-clear value
    /// and reject mixed compare directions.
    depth: DepthMode,
}

/// Which depth-texture slot kind a material pipeline declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShadowSlotKind {
    /// A `texture_depth_2d` slot fed from a [`ShadowMap`].
    Map,
    /// A `texture_depth_2d_array` slot fed from a [`ShadowMapArray`].
    Array,
}

impl MaterialPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// A square sampleable depth target rendered by a scene submission's shadow pass.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowMap {
    lease: ResourceLease,
    size: u32,
}

impl ShadowMap {
    /// Extent of the map along both axes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// A square layered sampleable depth target rendered by a scene submission's cascaded shadow
/// pass, one cascade per layer.
///
/// All layers share one extent; per-cascade fitting happens entirely in the application's
/// light matrices.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowMapArray {
    lease: ResourceLease,
    size: u32,
    layers: u32,
}

impl ShadowMapArray {
    /// Extent of every layer along both axes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Number of layers, each holding one cascade.
    #[must_use]
    pub const fn layers(&self) -> u32 {
        self.layers
    }

    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// The rendered depth resource feeding a material record's declared depth-texture slot.
#[derive(Clone, Copy)]
pub enum ShadowSource<'resources> {
    /// A single square map for a pipeline declaring [`MaterialBinding::DepthTexture`].
    Map(&'resources ShadowMap),
    /// A layered map for a pipeline declaring [`MaterialBinding::DepthTextureArray`].
    Array(&'resources ShadowMapArray),
}

/// Everything needed to create one application-authored depth-only shadow pipeline.
#[derive(Clone, Copy)]
pub struct ShadowPipelineDescriptor<'inputs> {
    /// Offline-compiled shader module containing the vertex entry point.
    pub shader: ShaderArtifact<'inputs>,
    /// Vertex entry point name.
    pub vertex_entry: &'inputs str,
    /// Optional fragment entry point name for casters that carve fragments out of the depth
    /// result, typically an alpha test that `discard`s below a cutout threshold; the pipeline
    /// runs no fragment stage when absent.
    ///
    /// The fragment stage rasterizes into no color target, so its only observable effect is
    /// discarding fragments; texture and sampler bindings become available so the test can
    /// sample the same base-color texture the caster's material pass uses.
    pub fragment_entry: Option<&'inputs str>,
    /// Per-vertex input layout; together with any instance layout it must cover the vertex
    /// entry point's recorded inputs.
    pub vertex_layout: VertexLayout<'inputs>,
    /// Optional per-instance input layout fed from each record's instance supply at
    /// instance-stepping rate, mirroring the caster's material pipeline.
    pub instance_layout: Option<VertexLayout<'inputs>>,
    /// Declared resource slots; shadow pipelines support at most one uniform slot and one
    /// read-only storage slot, plus texture and sampler slots when a fragment entry point is
    /// declared. The module must record no other bindings.
    pub bindings: &'inputs [MaterialBinding],
}

/// Application-authored depth-only pipeline drawn by a shadow pass.
#[derive(Debug, Eq, PartialEq)]
pub struct ShadowPipeline {
    lease: ResourceLease,
    layout: OwnedVertexLayout,
    /// Zero when no uniform slot is declared.
    uniform_size: u32,
    /// Zero when no storage slot is declared.
    storage_size: u32,
    /// Zero when no instance layout is declared.
    instance_stride: u32,
    texture_count: usize,
}

impl ShadowPipeline {
    pub(crate) const fn id(&self) -> ResourceId {
        self.lease.id
    }
}

/// One depth-only draw inside a shadow pass.
#[derive(Clone, Copy)]
pub struct ShadowRecord<'resources> {
    /// Shadow pipeline whose declared layout matches the mesh.
    pub pipeline: &'resources ShadowPipeline,
    /// Uploaded parent mesh or one immutable indexed part to render into the shadow map.
    pub geometry: MeshSource<'resources>,
    /// Uniform data matching the pipeline's declared uniform size (typically the light's
    /// view-projection times the record's model transform); empty when no uniform is declared.
    pub uniform: &'resources [u8],
    /// Read-only storage data matching the pipeline's declared storage size (typically the same
    /// bone-matrix palette as the caster's material record); empty when no storage is declared.
    pub storage: &'resources [u8],
    /// Textures for the pipeline's declared texture slots in ascending binding order, typically
    /// the same base-color texture the caster's material record samples for its alpha test;
    /// empty when the pipeline declares no texture slots.
    pub textures: &'resources [&'resources Texture],
    /// Per-instance data laid out per the pipeline's declared instance layout, mirroring the
    /// caster's material record: a non-empty multiple of the instance stride when the pipeline
    /// declares an instance layout, and empty when it declares none. Bounded by
    /// [`INSTANCE_SUPPLY_SIZE_LIMIT`].
    pub instances: &'resources [u8],
}

/// One depth-only pre-pass rendered into a shadow map before the scene pass samples it.
#[derive(Clone, Copy)]
pub struct ShadowPass<'resources> {
    /// Destination map, cleared to the far plane before the first record.
    pub map: &'resources ShadowMap,
    /// Non-empty depth-only records encoded in slice order.
    pub records: &'resources [ShadowRecord<'resources>],
}

/// One depth-only pre-pass per cascade layer, rendered into a shadow map array before the
/// scene pass samples it.
///
/// Each cascade renders with its own record list because every cascade carries its own light
/// matrix in its record uniforms, and the application may cull casters per cascade.
#[derive(Clone, Copy)]
pub struct CascadedShadowPass<'resources> {
    /// Destination layered map; every layer is cleared to the far plane before its records.
    pub map: &'resources ShadowMapArray,
    /// One depth-only record list per layer in layer order; the list count must equal the
    /// map's layer count. A cascade with no records still clears its layer, leaving that
    /// cascade fully lit.
    pub cascades: &'resources [&'resources [ShadowRecord<'resources>]],
}

/// Depth-only pre-pass work submitted ahead of one material scene pass.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum ShadowPrepass<'resources> {
    /// One pass into a single square map.
    Single(ShadowPass<'resources>),
    /// One pass per cascade layer into a layered map.
    Cascaded(CascadedShadowPass<'resources>),
}

impl<'resources> ShadowPrepass<'resources> {
    /// Every record in encode order: the single pass's list, or each cascade's list in layer
    /// order.
    pub(crate) fn records(&self) -> impl Iterator<Item = &'resources ShadowRecord<'resources>> {
        let (single, cascaded): (
            &'resources [ShadowRecord<'resources>],
            &'resources [&'resources [ShadowRecord<'resources>]],
        ) = match *self {
            Self::Single(pass) => (pass.records, &[]),
            Self::Cascaded(pass) => (&[], pass.cascades),
        };
        single.iter().chain(cascaded.iter().copied().flatten())
    }
}

/// Validated creation inputs handed to the native backends.
pub(crate) struct MaterialPipelineConfig<'inputs> {
    pub(crate) hdr: bool,
    pub(crate) vertex_entry: &'inputs str,
    pub(crate) fragment_entry: &'inputs str,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    pub(crate) instance_stride: u32,
    /// Declared instance-rate attributes; empty when no instance layout is declared.
    pub(crate) instance_attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    pub(crate) storage: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    pub(crate) texture_bindings: &'inputs [u32],
    /// Declared sampler slots with their filter and address modes.
    pub(crate) sampler_bindings: &'inputs [SamplerSlot],
    /// Snapshot binding and whether its shader expects multisampled depth.
    pub(crate) scene_depth_binding: Option<(u32, bool)>,
    /// Declared depth-texture slot fed from a shadow map per record.
    pub(crate) depth_texture_binding: Option<u32>,
    /// Declared depth-texture-array slot fed from a shadow map array per record.
    pub(crate) depth_texture_array_binding: Option<u32>,
    /// Declared fixed-recipe comparison-sampler slot.
    pub(crate) comparison_sampler_binding: Option<u32>,
    pub(crate) blend: BlendMode,
    pub(crate) depth: DepthMode,
}

/// Validated shadow pipeline creation inputs handed to the native backends.
pub(crate) struct ShadowPipelineConfig<'inputs> {
    pub(crate) vertex_entry: &'inputs str,
    /// Declared fragment entry point for depth-carving casters; absent for the depth-only form.
    pub(crate) fragment_entry: Option<&'inputs str>,
    pub(crate) stride: u32,
    pub(crate) attributes: &'inputs [VertexAttribute],
    /// Declared per-instance stride in bytes; zero when no instance layout is declared.
    pub(crate) instance_stride: u32,
    /// Declared instance-rate attributes consumed by the entry points.
    pub(crate) instance_attributes: &'inputs [VertexAttribute],
    /// Declared uniform slot as (binding, size).
    pub(crate) uniform: Option<(u32, u32)>,
    /// Declared read-only storage slot as (binding, size).
    pub(crate) storage: Option<(u32, u32)>,
    /// Declared texture binding numbers in ascending order.
    pub(crate) texture_bindings: &'inputs [u32],
    /// Declared sampler slots with their filter and address modes.
    pub(crate) sampler_bindings: &'inputs [SamplerSlot],
}

/// Extent- and generation-dependent color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct RenderTargets {
    lease: ResourceLease,
    info: SurfaceInfo,
}

/// Scale applied to the offscreen scene extent of postprocess targets, in percent of the
/// presentable extent.
///
/// The scale is a property of created targets rather than a per-frame toggle: changing it
/// means creating replacement targets, exactly like reacting to a surface reconfiguration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderScale {
    percent: u32,
}

impl RenderScale {
    /// Native 1:1 rendering.
    pub const NATIVE: Self = Self { percent: 100 };

    /// Smallest supported scale in percent.
    pub const MIN_PERCENT: u32 = 25;

    /// Largest supported scale in percent; values above one hundred supersample.
    pub const MAX_PERCENT: u32 = 200;

    /// Selects a scale in percent of the presentable extent.
    ///
    /// # Errors
    ///
    /// Returns an error for a value outside [`RenderScale::MIN_PERCENT`] through
    /// [`RenderScale::MAX_PERCENT`].
    pub fn percent(percent: u32) -> Result<Self, GraphicsError> {
        if !(Self::MIN_PERCENT..=Self::MAX_PERCENT).contains(&percent) {
            return Err(GraphicsError::invalid_request(format!(
                "render scale {percent} percent is outside the supported {} through {}",
                Self::MIN_PERCENT,
                Self::MAX_PERCENT
            )));
        }
        Ok(Self { percent })
    }

    /// Value in percent of the presentable extent.
    #[must_use]
    pub const fn as_percent(self) -> u32 {
        self.percent
    }

    /// The offscreen scene extent this scale selects for one presentable extent, flooring
    /// each dimension at one texel.
    pub(crate) fn scene_extent(self, extent: SurfaceExtent) -> SurfaceExtent {
        let scale = |axis: u32| -> u32 {
            let scaled = u64::from(axis) * u64::from(self.percent) / 100;
            u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
        };
        SurfaceExtent::new(scale(extent.width()), scale(extent.height()))
    }
}

/// Extent- and generation-dependent two-pass color/depth targets.
#[derive(Debug, Eq, PartialEq)]
pub struct PostprocessTargets {
    hdr: bool,
    lease: ResourceLease,
    info: SurfaceInfo,
    scale: RenderScale,
}

impl PostprocessTargets {
    /// Surface information these targets were created for.
    ///
    /// Recreate the targets when an acquired frame reports different surface information.
    #[must_use]
    pub const fn info(&self) -> SurfaceInfo {
        self.info
    }

    /// Scale applied to the offscreen scene extent relative to the presentable extent.
    #[must_use]
    pub const fn render_scale(&self) -> RenderScale {
        self.scale
    }
}

trait SceneTargets {
    fn hdr(&self) -> bool {
        false
    }
    fn session(&self) -> u64;
    fn info(&self) -> SurfaceInfo;
    fn label(&self) -> &'static str;
}

impl SceneTargets for RenderTargets {
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn label(&self) -> &'static str {
        ResourceKind::RenderTargets.label()
    }
}

impl SceneTargets for PostprocessTargets {
    fn hdr(&self) -> bool {
        self.hdr
    }
    fn session(&self) -> u64 {
        self.lease.session
    }

    fn info(&self) -> SurfaceInfo {
        self.info
    }

    fn label(&self) -> &'static str {
        ResourceKind::PostprocessTargets.label()
    }
}

impl RenderTargets {
    /// Surface information these targets were created for.
    ///
    /// Recreate the targets when an acquired frame reports different surface information; a draw
    /// into mismatched targets is rejected.
    #[must_use]
    pub const fn info(&self) -> SurfaceInfo {
        self.info
    }
}

/// Resources and dynamic data for one textured indexed draw.
#[derive(Clone, Copy)]
pub struct TexturedDraw<'resources> {
    /// Geometry to draw.
    pub mesh: &'resources Mesh,
    /// Sampled color texture.
    pub texture: &'resources Texture,
    /// Pipeline compatible with the session's selected sample count and surface format.
    pub pipeline: &'resources TexturedPipeline,
    /// Targets matching the acquired surface generation.
    pub targets: &'resources RenderTargets,
    /// Column-major model-view-projection matrix.
    pub model_view_projection: [[f32; 4]; 4],
    /// Linear clear color.
    pub clear: ClearColor,
}

/// Resources and dynamic data for one object in a textured scene pass.
#[derive(Clone, Copy)]
pub struct TexturedSceneDraw<'resources> {
    /// Geometry to draw.
    pub mesh: &'resources Mesh,
    /// Sampled color texture.
    pub texture: &'resources Texture,
    /// Depth-tested pipeline compatible with the scene targets.
    pub pipeline: &'resources TexturedPipeline,
    /// Column-major model-view-projection matrix for this object.
    pub model_view_projection: [[f32; 4]; 4],
}

/// A sequence of textured objects rendered directly into one presentable frame.
#[derive(Clone, Copy)]
pub struct TexturedScene<'resources> {
    /// Non-empty object sequence, encoded in slice order.
    pub draws: &'resources [TexturedSceneDraw<'resources>],
    /// Targets matching the acquired surface generation.
    pub targets: &'resources RenderTargets,
    /// Linear color used to clear the scene before its first object.
    pub clear: ClearColor,
}

/// One homogeneous instance batch inside a textured scene pass.
#[derive(Clone, Copy)]
pub struct TexturedInstanceBatch<'resources> {
    /// Geometry shared by every instance in this batch.
    pub mesh: &'resources Mesh,
    /// Sampled color texture shared by every instance in this batch.
    pub texture: &'resources Texture,
    /// Instanced depth-tested pipeline compatible with the scene targets.
    pub pipeline: &'resources InstancedTexturedPipeline,
    /// Non-empty column-major model-view-projection matrix sequence in instance order.
    pub model_view_projections: &'resources [[[f32; 4]; 4]],
}

/// One application-authored material draw inside a scene pass.
#[derive(Clone, Copy)]
pub struct MaterialRecord<'resources> {
    /// Material pipeline compatible with the scene targets.
    pub pipeline: &'resources MaterialPipeline,
    /// Geometry supply: an uploaded mesh whose vertex layout matches the pipeline's declared
    /// layout, or frame-transient bytes laid out per that declaration.
    pub geometry: GeometrySource<'resources>,
    /// Textures for the pipeline's declared texture slots in ascending binding order.
    pub textures: &'resources [&'resources Texture],
    /// The depth resource feeding the pipeline's depth-texture slot; required exactly when the
    /// pipeline declares one, matching the declared kind (plain map or layered array), and it
    /// must have been rendered by a shadow pass (this frame or earlier).
    pub shadow_map: Option<ShadowSource<'resources>>,
    /// Uniform data matching the pipeline's declared uniform size; empty when the pipeline
    /// declares no uniform slot. The application owns WGSL memory-layout correctness.
    pub uniform: &'resources [u8],
    /// Read-only storage data matching the pipeline's declared storage size (typically a
    /// bone-matrix palette); empty when the pipeline declares no storage slot.
    pub storage: &'resources [u8],
    /// Per-instance data laid out per the pipeline's declared instance layout: a non-empty
    /// multiple of the instance stride when the pipeline declares an instance layout (the
    /// record draws once per instance), and empty when it declares none. Bounded by
    /// [`INSTANCE_SUPPLY_SIZE_LIMIT`]; the bytes are copied into the session's frame-transient
    /// instance region at submission.
    pub instances: &'resources [u8],
}

/// Resources and dynamic data for one offscreen textured draw followed by a fullscreen pass.
#[derive(Clone, Copy)]
pub struct PostprocessedDraw<'resources> {
    /// Geometry to draw into the offscreen scene color.
    pub mesh: &'resources Mesh,
    /// Texture sampled by the scene pass.
    pub texture: &'resources Texture,
    /// Depth-tested scene pipeline compatible with the selected sample count.
    pub scene_pipeline: &'resources TexturedPipeline,
    /// Single-sample fullscreen pipeline that samples the resolved scene color.
    pub postprocess_pipeline: &'resources PostprocessPipeline,
    /// Offscreen, depth, and optional multisample targets matching the acquired frame.
    pub targets: &'resources PostprocessTargets,
    /// Uniform data copied for this submission; its length must exactly match the postprocess
    /// pipeline declaration, and it must be empty when the pipeline declares no uniform.
    pub uniform: &'resources [u8],
    /// Column-major model-view-projection matrix for the scene draw.
    pub model_view_projection: [[f32; 4]; 4],
    /// Linear scene-pass clear color.
    pub clear: ClearColor,
}

/// A sequence of textured objects followed by one fullscreen post-processing pass.
#[derive(Clone, Copy)]
pub struct PostprocessedScene<'resources> {
    /// Non-empty object sequence, encoded in slice order into offscreen scene color.
    pub draws: &'resources [TexturedSceneDraw<'resources>],
    /// Single-sample fullscreen pipeline that samples the resolved scene color.
    pub postprocess_pipeline: &'resources PostprocessPipeline,
    /// Offscreen, depth, and optional multisample targets matching the acquired frame.
    pub targets: &'resources PostprocessTargets,
    /// Uniform data copied for this submission; its length must exactly match the postprocess
    /// pipeline declaration, and it must be empty when the pipeline declares no uniform.
    pub uniform: &'resources [u8],
    /// Linear color used to clear the scene before its first object.
    pub clear: ClearColor,
}

/// One narrow scene recipe accepted by [`Queue::render_and_present`].
#[derive(Clone, Copy)]
pub struct SceneSubmission<'resources> {
    /// Geometry records and their submission grouping.
    pub content: SceneContent<'resources>,
    /// Direct or postprocessed destination for the scene pass.
    pub output: SceneOutput<'resources>,
    /// Optional depth-only pre-pass work — a single map or one pass per cascade layer —
    /// rendered before the scene pass; composes with material content only.
    pub shadow: Option<ShadowPrepass<'resources>>,
    /// Optional non-empty material records drawn into the presentable target after the
    /// fullscreen postprocess draw, at the surface's native extent.
    ///
    /// The overlay keeps record-based text and UI sharp while a below-native [`RenderScale`]
    /// shrinks the scene pass: overlay records never touch the scaled offscreen storage. It
    /// composes with material content and postprocessed output only. The presentable pass
    /// carries no depth target, so every overlay record's pipeline must declare
    /// [`DepthMode::Off`] and no depth-texture slot; painter's order is the record order.
    /// Overlay pipelines rasterize at one sample, so [`BlendMode::Cutout`] degrades to a hard
    /// alpha threshold here.
    pub overlay: Option<&'resources [MaterialRecord<'resources>]>,
    /// Offscreen passes into [`RenderTexture`]s, encoded in slice order after any shadow
    /// prepass (whose map their records may sample) and before the scene pass, so the scene
    /// and overlay records may sample what they rendered. Empty for none; any composes with
    /// material content and postprocessed output only.
    pub offscreen: &'resources [OffscreenPass<'resources>],
    /// Linear color used to clear the scene before its first draw or batch.
    pub clear: ClearColor,
}

/// Geometry content for one [`SceneSubmission`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum SceneContent<'resources> {
    /// Non-empty heterogeneous textured records encoded in slice order.
    Textured(&'resources [TexturedSceneDraw<'resources>]),
    /// Non-empty homogeneous instance batches encoded in slice order.
    Instanced(&'resources [TexturedInstanceBatch<'resources>]),
    /// Non-empty application-authored material records encoded in slice order.
    Material(&'resources [MaterialRecord<'resources>]),
    /// Ordered world and foreground records, separated by a fresh depth clear.
    ///
    /// Records before `foreground_start` draw the world. The remaining records draw
    /// over its color with independent depth, before postprocessing and the HUD overlay.
    /// Both groups must be non-empty and use the same depth comparison direction.
    /// The foreground retains scene resolution, MSAA, material bindings and shadow sampling.
    /// Requires [`SceneOutput::Postprocessed`].
    MaterialWithForeground {
        /// World records followed by foreground records; staged once in this order.
        records: &'resources [MaterialRecord<'resources>],
        /// Index of the first foreground record, strictly inside `records`.
        foreground_start: usize,
    },
}

/// Output path for one [`SceneSubmission`].
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum SceneOutput<'resources> {
    /// Render directly into generation-matched presentable targets.
    Direct(&'resources RenderTargets),
    /// Resolve into sampled scene color, then run one fullscreen postprocess pass.
    Postprocessed {
        /// Fullscreen pipeline that samples the resolved scene color.
        pipeline: &'resources PostprocessPipeline,
        /// Generation-matched offscreen, depth, and optional multisample targets.
        targets: &'resources PostprocessTargets,
        /// Uniform data copied for this submission; its length must exactly match the pipeline
        /// declaration, and it must be empty when the pipeline declares no uniform.
        uniform: &'resources [u8],
    },
}

/// Validated postprocess creation inputs handed to the native backends.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeStage {
    #[default]
    None,
    Scatter,
    Composite,
}

/// Application-authored filters for the HDR bloom chain.
#[derive(Clone, Copy)]
pub struct BloomShaders<'inputs> {
    /// Extracts highlights from the resolved scene into the first half-resolution level.
    pub prefilter: ShaderArtifact<'inputs>,
    /// Filters each preceding level into the next smaller level.
    pub downsample: ShaderArtifact<'inputs>,
    /// Optional progressive upsampling. After the downsampling, it reads each level from the
    /// smallest up and its output is added (blended one-to-one) into the next larger level, so the
    /// first level ends up holding the whole bloom and the composite reads it alone, at binding 3.
    /// Its chain goes on halving past six levels until a level's smaller side is at most
    /// [`BLOOM_SMALLEST`] texels, so its coarsest level covers about the same share of the screen
    /// at any resolution. A filter learns its level's size from its input (`textureDimensions`).
    pub upsample: Option<ShaderArtifact<'inputs>>,
}

/// Application shaders for shadowed, half-resolution HDR scattering.
/// All use `post_vertex` / `post_fragment`, and the composite's exact uniform
/// layout at binding 0. Binding 1 is world depth. Binding 2 is a depth texture
/// array for scattering, or the half-resolution `RGBA16Float` result for compositing.
/// The composite adds RGB to scene color; its alpha is ignored. It runs before
/// foreground, bloom and tone mapping. Both MSAA variants read multisampled depth.
#[derive(Clone, Copy)]
pub struct VolumetricShaders<'inputs> {
    /// Scattering with single-sample world depth.
    pub scatter: ShaderArtifact<'inputs>,
    /// Scattering with multisampled world depth.
    pub scatter_msaa: ShaderArtifact<'inputs>,
    /// Depth-aware additive upscale with single-sample world depth.
    pub composite: ShaderArtifact<'inputs>,
    /// Depth-aware additive upscale with multisampled world depth.
    pub composite_msaa: ShaderArtifact<'inputs>,
}

pub(crate) struct PostprocessPipelineConfig<'inputs> {
    pub(crate) volume: Option<VolumetricShaders<'inputs>>,
    pub(crate) volume_stage: VolumeStage,
    pub(crate) samples: u32,
    pub(crate) bloom: Option<BloomShaders<'inputs>>,
    /// Whether the output is added to the target's contents (one-to-one) instead of replacing it.
    pub(crate) additive: bool,
    pub(crate) hdr_output: bool,
    /// Declared group-0/binding-0 byte size, or zero when absent.
    pub(crate) uniform_size: u32,
}

/// One acquired native drawable or swapchain image.
#[must_use = "an acquired frame must be presented or explicitly abandoned"]
pub struct Frame<'window> {
    shared: Shared<'window>,
    token: Option<backend::TexturedFrameToken>,
    info: SurfaceInfo,
}

impl Frame<'_> {
    /// Surface generation owning this frame.
    #[must_use]
    pub const fn surface_info(&self) -> SurfaceInfo {
        self.info
    }

    /// Releases the frame through the backend-specific non-presentation path.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot complete safe abandonment.
    pub fn abandon(mut self) -> Result<FrameDisposition, GraphicsError> {
        let token = self
            .token
            .take()
            .ok_or_else(|| GraphicsError::lifecycle("frame is already disposed"))?;
        session_mut(&self.shared)?.abandon(token)
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take()
            && let Ok(mut session) = session_mut(&self.shared)
        {
            session.defer_abandon(token);
        }
    }
}

/// Number of levels in a full mip chain from the base extent down to its 1x1 level.
pub(crate) fn full_mip_chain_len(width: u32, height: u32) -> usize {
    let largest = width.max(height);
    usize::try_from(32 - largest.leading_zeros()).expect("level count fits usize")
}

/// Presents six single-level faces as six one-level chains, borrowing each face in place.
fn single_level_faces<T>(faces: &[T; 6]) -> [&[T]; 6] {
    faces.each_ref().map(core::slice::from_ref)
}

/// Extent of one mip level along one axis, flooring at one texel.
pub(crate) const fn mip_extent(base: u32, level: u32) -> u32 {
    let scaled = base >> level;
    if scaled == 0 { 1 } else { scaled }
}

/// Checks that one mip level's byte count matches its tightly packed extent: texels for an
/// uncompressed format, whole 4×4 blocks for a compressed one.
fn validate_mip_level(
    format: SampledTextureFormat,
    width: u32,
    height: u32,
    level: u32,
    texels: &[u8],
) -> Result<(), GraphicsError> {
    if width == 0 || height == 0 {
        return Err(GraphicsError::invalid_request(
            "texture byte count does not match its dimensions",
        ));
    }
    let level_width = mip_extent(width, level);
    let level_height = mip_extent(height, level);
    let expected = format
        .level_bytes(level_width, level_height)
        .filter(|size| *size <= isize::MAX.cast_unsigned())
        .ok_or_else(|| {
            GraphicsError::invalid_request("texture dimensions overflow address space")
        })?;
    if texels.len() != expected {
        if level == 0 {
            return Err(GraphicsError::invalid_request(
                "texture byte count does not match its dimensions",
            ));
        }
        return Err(GraphicsError::invalid_request(format!(
            "texture mip level {level} supplies {} bytes but its {level_width}x{level_height} \
             extent needs {expected}",
            texels.len()
        )));
    }
    Ok(())
}

/// Derives the scene depth-clear value from the records' declared depth modes.
///
/// Greater-compare (reversed-Z) records select a 0.0 clear and conventional less-compare
/// records select the 1.0 far-plane clear; one depth target cannot serve both conventions,
/// so mixing the directions in a single scene is rejected. A scene of only [`DepthMode::Off`]
/// records keeps the conventional far-plane clear.
fn material_scene_depth_clear(records: &[MaterialRecord<'_>]) -> Result<f32, GraphicsError> {
    let mut less = false;
    let mut greater = false;
    for record in records {
        less |= record.pipeline.depth.tests_less();
        greater |= record.pipeline.depth.tests_greater();
    }
    if less && greater {
        return Err(GraphicsError::invalid_request(
            "material scene mixes less-compare and greater-compare depth modes against one \
             depth target",
        ));
    }
    Ok(if greater { 0.0 } else { 1.0 })
}

/// Checks one record's instance supply against its pipeline's declared instance stride: empty
/// when no instance layout is declared, otherwise a non-empty stride multiple inside the
/// supply limit.
fn validate_instance_supply(
    record_label: &str,
    instances: &[u8],
    instance_stride: u32,
) -> Result<(), GraphicsError> {
    if instance_stride == 0 {
        if !instances.is_empty() {
            return Err(GraphicsError::invalid_request(format!(
                "{record_label} supplies instance bytes but its pipeline declares no instance \
                 layout"
            )));
        }
        return Ok(());
    }
    let stride = usize::try_from(instance_stride).expect("validated stride fits usize");
    if instances.is_empty() || !instances.len().is_multiple_of(stride) {
        return Err(GraphicsError::invalid_request(format!(
            "{record_label}'s instance bytes must be a non-zero multiple of its pipeline's \
             declared {instance_stride}-byte instance stride"
        )));
    }
    if instances.len() > usize::try_from(INSTANCE_SUPPLY_SIZE_LIMIT).expect("u32 limit fits usize")
    {
        return Err(GraphicsError::invalid_request(format!(
            "{record_label}'s instance supply exceeds the {INSTANCE_SUPPLY_SIZE_LIMIT}-byte \
             supply limit"
        )));
    }
    Ok(())
}

/// Checks stride and attribute fit, and returns the location-sorted owned layout.
fn validate_vertex_layout(layout: VertexLayout<'_>) -> Result<OwnedVertexLayout, GraphicsError> {
    if layout.stride == 0 {
        return Err(GraphicsError::invalid_request(
            "vertex layout stride must be non-zero",
        ));
    }
    // Metal requires four-byte strides and attribute offsets; holding Vulkan to the same rule
    // keeps one layout valid on both backends.
    if !layout.stride.is_multiple_of(4) {
        return Err(GraphicsError::invalid_request(format!(
            "vertex layout stride {} is not a multiple of four bytes",
            layout.stride
        )));
    }
    let owned = layout.to_owned_layout();
    for window in owned.attributes.windows(2) {
        if window[0].location == window[1].location {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout declares location {} twice",
                window[0].location
            )));
        }
    }
    for attribute in &owned.attributes {
        if attribute.location > MATERIAL_SLOT_LIMIT {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "vertex attribute location {} exceeds the supported locations 0 through \
                     {MATERIAL_SLOT_LIMIT}",
                    attribute.location
                ),
            ));
        }
    }
    for attribute in &owned.attributes {
        if !attribute.offset.is_multiple_of(4) {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout attribute at location {} has offset {}, which is not a multiple \
                 of four bytes",
                attribute.location, attribute.offset
            )));
        }
        let end = attribute
            .offset
            .checked_add(attribute.format.byte_len())
            .filter(|&end| end <= layout.stride);
        if end.is_none() {
            return Err(GraphicsError::invalid_request(format!(
                "vertex layout attribute at location {} does not fit inside the {}-byte stride",
                attribute.location, layout.stride
            )));
        }
    }
    Ok(owned)
}

fn find_entry_point<'interface>(
    interface: &'interface shader::ShaderInterface,
    name: &str,
    stage: u8,
    stage_label: &str,
) -> Result<&'interface shader::InterfaceEntryPoint, GraphicsError> {
    interface
        .entry_points
        .iter()
        .find(|entry| entry.stage == stage && entry.name == name)
        .ok_or_else(|| {
            GraphicsError::invalid_request(format!(
                "shader artifact records no {stage_label} entry point named `{name}`"
            ))
        })
}

/// Checks an instance layout's stride and attribute fit, and rejects locations the vertex
/// layout already declares, returning the location-sorted owned layout.
fn validate_instance_layout(
    vertex: &OwnedVertexLayout,
    instance: VertexLayout<'_>,
) -> Result<OwnedVertexLayout, GraphicsError> {
    let owned = validate_vertex_layout(instance)?;
    for attribute in &owned.attributes {
        if vertex
            .attributes
            .iter()
            .any(|declared| declared.location == attribute.location)
        {
            return Err(GraphicsError::invalid_request(format!(
                "instance layout declares location {} that the vertex layout already declares",
                attribute.location
            )));
        }
    }
    Ok(owned)
}

/// Finds the declared attribute feeding one recorded input — in the vertex layout or, when
/// declared, the instance layout — and checks its format, naming the first mismatch.
fn find_declared_attribute<'layouts>(
    layout: &'layouts OwnedVertexLayout,
    instance_layout: Option<&'layouts OwnedVertexLayout>,
    input: shader::InterfaceVertexInput,
    entry_name: &str,
) -> Result<(&'layouts VertexAttribute, bool), GraphicsError> {
    let vertex_attribute = layout
        .attributes
        .iter()
        .find(|attribute| attribute.location == input.location)
        .map(|attribute| (attribute, false));
    let attribute = vertex_attribute.or_else(|| {
        instance_layout.and_then(|instance| {
            instance
                .attributes
                .iter()
                .find(|attribute| attribute.location == input.location)
                .map(|attribute| (attribute, true))
        })
    });
    let Some((attribute, from_instance)) = attribute else {
        return Err(GraphicsError::invalid_request(format!(
            "entry point `{entry_name}` consumes location {} that the declared layouts do not \
             supply",
            input.location
        )));
    };
    if input.format != attribute.format.interface_code() {
        let recorded = VertexFormat::from_interface_code(input.format)
            .map_or("an unsupported format", VertexFormat::wgsl_name);
        return Err(GraphicsError::invalid_request(format!(
            "declared layouts supply location {} as {} but the shader artifact records {}",
            attribute.location,
            attribute.format.describe(),
            recorded
        )));
    }
    Ok((attribute, from_instance))
}

/// Requires the declared vertex and instance attributes together to match the artifact's
/// recorded vertex inputs exactly, naming the first offending location.
fn validate_layouts_against_entry(
    layout: &OwnedVertexLayout,
    instance_layout: Option<&OwnedVertexLayout>,
    entry: &shader::InterfaceEntryPoint,
) -> Result<(), GraphicsError> {
    for &input in &entry.inputs {
        find_declared_attribute(layout, instance_layout, input, &entry.name)?;
    }
    let declared = layout.attributes.iter().chain(
        instance_layout
            .map(|instance| instance.attributes.as_slice())
            .unwrap_or_default(),
    );
    for attribute in declared {
        if !entry
            .inputs
            .iter()
            .any(|input| input.location == attribute.location)
        {
            return Err(GraphicsError::invalid_request(format!(
                "declared layouts supply location {} that entry point `{}` does not consume",
                attribute.location, entry.name
            )));
        }
    }
    Ok(())
}

/// Requires every recorded vertex input to have a matching declared attribute — extra declared
/// attributes are legal and simply not consumed by the depth-only stage — and returns the
/// consumed vertex-rate and instance-rate subsets for native vertex-input construction.
fn validate_layouts_cover_entry(
    layout: &OwnedVertexLayout,
    instance_layout: Option<&OwnedVertexLayout>,
    entry: &shader::InterfaceEntryPoint,
) -> Result<(Vec<VertexAttribute>, Vec<VertexAttribute>), GraphicsError> {
    let mut consumed = Vec::with_capacity(entry.inputs.len());
    let mut consumed_instance = Vec::new();
    for &input in &entry.inputs {
        let (attribute, from_instance) =
            find_declared_attribute(layout, instance_layout, input, &entry.name)?;
        if from_instance {
            consumed_instance.push(*attribute);
        } else {
            consumed.push(*attribute);
        }
    }
    Ok((consumed, consumed_instance))
}

struct BindingDeclaration {
    uniform: Option<(u32, u32)>,
    /// 2D, cube and cube-array texture slots together, in ascending binding order.
    texture_bindings: Vec<u32>,
    /// The shape each slot in `texture_bindings` samples, index for index.
    texture_dimensions: Vec<TextureDimension>,
    sampler_bindings: Vec<SamplerSlot>,
    scene_depth: Option<(u32, bool)>,
    depth_texture: Option<u32>,
    depth_texture_array: Option<u32>,
    comparison_sampler: Option<u32>,
    /// Declared read-only storage slot as (binding, size).
    storage: Option<(u32, u32)>,
}

const fn interface_binding_label(kind: u8) -> &'static str {
    match kind {
        shader::INTERFACE_BINDING_UNIFORM => "uniform data",
        shader::INTERFACE_BINDING_SAMPLED_TEXTURE => "a sampled texture",
        shader::INTERFACE_BINDING_SAMPLER => "a sampler",
        shader::INTERFACE_BINDING_STORAGE => "a storage buffer",
        shader::INTERFACE_BINDING_DEPTH_TEXTURE => "a depth texture",
        shader::INTERFACE_BINDING_MULTISAMPLED_DEPTH => "a multisampled depth texture",
        shader::INTERFACE_BINDING_COMPARISON_SAMPLER => "a comparison sampler",
        shader::INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY => "a depth texture array",
        shader::INTERFACE_BINDING_CUBE_TEXTURE => "a cube texture",
        shader::INTERFACE_BINDING_CUBE_TEXTURE_ARRAY => "a cube texture array",
        _ => "an unsupported resource",
    }
}

/// The native argument table a binding kind is placed in, since a WGSL binding number is used
/// as the native index unchanged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotNamespace {
    Buffer,
    Texture,
    Sampler,
}

impl SlotNamespace {
    const fn of(kind: u8) -> Self {
        match kind {
            shader::INTERFACE_BINDING_UNIFORM | shader::INTERFACE_BINDING_STORAGE => Self::Buffer,
            shader::INTERFACE_BINDING_SAMPLER | shader::INTERFACE_BINDING_COMPARISON_SAMPLER => {
                Self::Sampler
            }
            _ => Self::Texture,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Buffer => "buffer",
            Self::Texture => "texture",
            Self::Sampler => "sampler",
        }
    }

    const fn ceiling(self) -> u32 {
        match self {
            Self::Buffer => MATERIAL_BUFFER_SLOT_LIMIT,
            Self::Texture => MATERIAL_TEXTURE_SLOT_LIMIT,
            Self::Sampler => MATERIAL_SLOT_LIMIT,
        }
    }
}

/// Validates a pipeline's declared slots against the bindings its entry points use rather than
/// the whole module, so one module can hold entry points with different resources and each
/// pipeline declares only what its own pair binds. A declared slot the module records but these
/// entry points never use is refused by name.
fn validate_entry_point_bindings(
    bindings: &[MaterialBinding],
    interface: &shader::ShaderInterface,
    entries: &[&shader::InterfaceEntryPoint],
) -> Result<BindingDeclaration, GraphicsError> {
    let used = shader::ShaderInterface {
        entry_points: Vec::new(),
        bindings: interface.bindings_used_by(entries),
    };
    for binding in bindings {
        let slot = binding.slot();
        if !used
            .bindings
            .iter()
            .any(|recorded| recorded.binding == slot)
            && interface
                .bindings
                .iter()
                .any(|recorded| recorded.binding == slot)
        {
            let names: Vec<_> = entries
                .iter()
                .map(|entry| format!("`{}`", entry.name))
                .collect();
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot}, which the shader module records but \
                 entry points {} do not use",
                names.join(" and ")
            )));
        }
    }
    validate_bindings_against_interface(bindings, &used)
}

/// Requires the declared slots and the artifact's recorded bindings to match exactly, naming the
/// first offending slot, and rejects interface constructs outside the material vocabulary.
#[allow(clippy::too_many_lines)]
fn validate_bindings_against_interface(
    bindings: &[MaterialBinding],
    interface: &shader::ShaderInterface,
) -> Result<BindingDeclaration, GraphicsError> {
    for recorded in &interface.bindings {
        if recorded.group != 0 {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "shader binding {}:{} is outside group 0, which the material vocabulary does not \
                 support",
                    recorded.group, recorded.binding
                ),
            ));
        }
    }
    let mut declaration = BindingDeclaration {
        uniform: None,
        texture_bindings: Vec::new(),
        texture_dimensions: Vec::new(),
        sampler_bindings: Vec::new(),
        scene_depth: None,
        depth_texture: None,
        depth_texture_array: None,
        comparison_sampler: None,
        storage: None,
    };
    let mut declared: Vec<(u32, u8, u32)> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let (slot, kind, size) = match *binding {
            MaterialBinding::Uniform { binding, size } => {
                if declaration.uniform.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one uniform slot",
                    ));
                }
                if size == 0 || size > MATERIAL_UNIFORM_SIZE_LIMIT {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        format!(
                            "material uniform slot {binding} declares {size} bytes, outside the \
                         supported 1 through {MATERIAL_UNIFORM_SIZE_LIMIT}"
                        ),
                    ));
                }
                declaration.uniform = Some((binding, size));
                (binding, shader::INTERFACE_BINDING_UNIFORM, size)
            }
            MaterialBinding::Storage { binding, size } => {
                if declaration.storage.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one storage slot",
                    ));
                }
                if size == 0 || size > MATERIAL_STORAGE_SIZE_LIMIT {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        format!(
                            "material storage slot {binding} declares {size} bytes, outside the \
                         supported 1 through {MATERIAL_STORAGE_SIZE_LIMIT}"
                        ),
                    ));
                }
                declaration.storage = Some((binding, size));
                (binding, shader::INTERFACE_BINDING_STORAGE, size)
            }
            MaterialBinding::Texture { binding } => {
                declaration.texture_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_SAMPLED_TEXTURE, 0)
            }
            MaterialBinding::CubeTexture { binding } => {
                declaration.texture_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_CUBE_TEXTURE, 0)
            }
            MaterialBinding::CubeTextureArray { binding } => {
                declaration.texture_bindings.push(binding);
                (binding, shader::INTERFACE_BINDING_CUBE_TEXTURE_ARRAY, 0)
            }
            MaterialBinding::Sampler {
                binding,
                filter,
                address,
            } => {
                declaration.sampler_bindings.push(SamplerSlot {
                    binding,
                    filter,
                    address: SamplerAddressPerAxis::all(address),
                });
                (binding, shader::INTERFACE_BINDING_SAMPLER, 0)
            }
            MaterialBinding::SamplerPerAxis {
                binding,
                filter,
                address,
            } => {
                declaration.sampler_bindings.push(SamplerSlot {
                    binding,
                    filter,
                    address,
                });
                (binding, shader::INTERFACE_BINDING_SAMPLER, 0)
            }
            MaterialBinding::SceneDepth { binding } => {
                let multisampled = interface.bindings.iter().any(|slot| {
                    slot.binding == binding
                        && slot.kind == shader::INTERFACE_BINDING_MULTISAMPLED_DEPTH
                });
                if declaration
                    .scene_depth
                    .replace((binding, multisampled))
                    .is_some()
                {
                    return Err(GraphicsError::invalid_request(
                        "material pipelines support at most one scene-depth slot",
                    ));
                }
                (
                    binding,
                    if multisampled {
                        shader::INTERFACE_BINDING_MULTISAMPLED_DEPTH
                    } else {
                        shader::INTERFACE_BINDING_DEPTH_TEXTURE
                    },
                    0,
                )
            }
            MaterialBinding::DepthTexture { binding } => {
                if declaration.depth_texture.is_some() || declaration.depth_texture_array.is_some()
                {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one depth-texture slot",
                    ));
                }
                declaration.depth_texture = Some(binding);
                (binding, shader::INTERFACE_BINDING_DEPTH_TEXTURE, 0)
            }
            MaterialBinding::DepthTextureArray { binding } => {
                if declaration.depth_texture.is_some() || declaration.depth_texture_array.is_some()
                {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one depth-texture slot",
                    ));
                }
                declaration.depth_texture_array = Some(binding);
                (binding, shader::INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY, 0)
            }
            MaterialBinding::ComparisonSampler { binding } => {
                if declaration.comparison_sampler.is_some() {
                    return Err(GraphicsError::with_kind(
                        GraphicsErrorKind::Unsupported,
                        "material pipelines support at most one comparison-sampler slot",
                    ));
                }
                declaration.comparison_sampler = Some(binding);
                (binding, shader::INTERFACE_BINDING_COMPARISON_SAMPLER, 0)
            }
        };
        let namespace = SlotNamespace::of(kind);
        if slot > namespace.ceiling() {
            return Err(GraphicsError::with_kind(
                GraphicsErrorKind::Unsupported,
                format!(
                    "material {} slot {slot} exceeds the supported slots 0 through {}",
                    namespace.label(),
                    namespace.ceiling()
                ),
            ));
        }
        if declared.iter().any(|&(previous, _, _)| previous == slot) {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} twice"
            )));
        }
        declared.push((slot, kind, size));
    }
    let textures = declared
        .iter()
        .filter(|&&(_, kind, _)| SlotNamespace::of(kind) == SlotNamespace::Texture)
        .count();
    if textures > MATERIAL_TEXTURE_COUNT_LIMIT as usize {
        return Err(GraphicsError::with_kind(
            GraphicsErrorKind::Unsupported,
            format!(
                "material bindings declare {textures} textures, more than the supported \
                 {MATERIAL_TEXTURE_COUNT_LIMIT}"
            ),
        ));
    }
    for &(slot, kind, size) in &declared {
        let Some(recorded) = interface
            .bindings
            .iter()
            .find(|recorded| recorded.binding == slot)
        else {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} that the shader artifact does not record"
            )));
        };
        if recorded.kind != kind {
            return Err(GraphicsError::invalid_request(format!(
                "material bindings declare slot {slot} as {} but the shader artifact records {}",
                interface_binding_label(kind),
                interface_binding_label(recorded.kind)
            )));
        }
        if (kind == shader::INTERFACE_BINDING_UNIFORM || kind == shader::INTERFACE_BINDING_STORAGE)
            && recorded.size != size
        {
            return Err(GraphicsError::invalid_request(format!(
                "material {} slot {slot} declares {size} bytes but the shader artifact \
                 records {}",
                if kind == shader::INTERFACE_BINDING_UNIFORM {
                    "uniform"
                } else {
                    "storage"
                },
                recorded.size
            )));
        }
    }
    for recorded in &interface.bindings {
        if !declared
            .iter()
            .any(|&(slot, _, _)| slot == recorded.binding)
        {
            return Err(GraphicsError::invalid_request(format!(
                "the shader artifact records binding slot {} for the pipeline's entry points, \
                 but the material bindings do not declare it",
                recorded.binding
            )));
        }
    }
    declaration.texture_bindings.sort_unstable();
    declaration.texture_dimensions = declaration
        .texture_bindings
        .iter()
        .map(|&slot| {
            match declared
                .iter()
                .find(|&&(declared, _, _)| declared == slot)
                .map(|&(_, kind, _)| kind)
            {
                Some(shader::INTERFACE_BINDING_CUBE_TEXTURE) => TextureDimension::Cube,
                Some(shader::INTERFACE_BINDING_CUBE_TEXTURE_ARRAY) => TextureDimension::CubeArray,
                _ => TextureDimension::D2,
            }
        })
        .collect();
    declaration
        .sampler_bindings
        .sort_unstable_by_key(|slot| slot.binding);
    Ok(declaration)
}

/// Requires a material record to supply one texture per declared slot, in ascending binding
/// order, each of the shape its slot samples.
fn validate_record_textures(
    textures: &[&Texture],
    slots: &[TextureDimension],
) -> Result<(), GraphicsError> {
    if textures.len() != slots.len() {
        return Err(GraphicsError::invalid_request(format!(
            "material record supplies {} textures but its pipeline declares {} texture slots",
            textures.len(),
            slots.len()
        )));
    }
    for (index, (texture, &slot)) in textures.iter().zip(slots).enumerate() {
        if texture.dimension != slot {
            return Err(GraphicsError::invalid_request(format!(
                "material record supplies a {} for texture {index}, but that slot samples a {}",
                texture.dimension.label(),
                slot.label()
            )));
        }
    }
    Ok(())
}

/// The fixed textured pipelines sample `texture_2d<f32>` only.
fn validate_fixed_pipeline_texture(texture: &Texture) -> Result<(), GraphicsError> {
    if texture.dimension == TextureDimension::D2 {
        return Ok(());
    }
    Err(GraphicsError::invalid_request(
        "textured draws sample a 2D texture; cube textures and cube texture arrays bind through \
         material pipelines only",
    ))
}

/// Validates the fixed postprocess entry points and binding recipe against the artifact's
/// module-wide reflection. Artifacts now record per-entry-point binding use, but the fixed
/// postprocess, composite, bloom and volume recipes still check the whole module (and `MULSHDR2`
/// artifacts carry no per-entry record), so an absent postprocess uniform cannot reject a
/// module-level binding 0 that is consumed only by a scene entry point in the same module.
fn validate_postprocess_interface(
    shader: ShaderArtifact<'_>,
    uniform_size: Option<u32>,
) -> Result<(), GraphicsError> {
    let interface = shader.parse_interface();
    find_entry_point(
        &interface,
        "post_vertex",
        shader::INTERFACE_STAGE_VERTEX,
        "vertex",
    )?;
    find_entry_point(
        &interface,
        "post_fragment",
        shader::INTERFACE_STAGE_FRAGMENT,
        "fragment",
    )?;

    let validate_fixed = |binding: u32, kind: u8, label: &str| {
        let recorded = interface
            .bindings
            .iter()
            .find(|recorded| recorded.group == 0 && recorded.binding == binding)
            .ok_or_else(|| {
                GraphicsError::invalid_request(format!(
                    "postprocess pipeline requires {label} at group 0, binding {binding}, but the \
                     shader artifact does not record it"
                ))
            })?;
        if recorded.kind != kind {
            return Err(GraphicsError::invalid_request(format!(
                "postprocess pipeline requires {label} at group 0, binding {binding}, but the \
                 shader artifact records {}",
                interface_binding_label(recorded.kind)
            )));
        }
        Ok(recorded)
    };
    validate_fixed(
        1,
        shader::INTERFACE_BINDING_SAMPLED_TEXTURE,
        "resolved scene color",
    )?;
    validate_fixed(
        2,
        shader::INTERFACE_BINDING_SAMPLER,
        "the scene-color sampler",
    )?;

    if let Some(size) = uniform_size {
        if size == 0 || size > POSTPROCESS_UNIFORM_SIZE_LIMIT {
            return Err(GraphicsError::invalid_request(format!(
                "postprocess uniform declares {size} bytes, outside the supported 1 through \
                 {POSTPROCESS_UNIFORM_SIZE_LIMIT}"
            )));
        }
        let recorded = validate_fixed(0, shader::INTERFACE_BINDING_UNIFORM, "uniform data")?;
        if recorded.size != size {
            return Err(GraphicsError::invalid_request(format!(
                "postprocess uniform at group 0, binding 0 declares {size} bytes but the shader \
                 artifact records {}",
                recorded.size
            )));
        }
    }
    Ok(())
}

fn validate_postprocess_uniform(
    pipeline: &PostprocessPipeline,
    uniform: &[u8],
) -> Result<(), GraphicsError> {
    let expected = usize::try_from(pipeline.uniform_size).expect("u32 size fits usize");
    if uniform.len() == expected {
        return Ok(());
    }
    if uniform.len() > usize::try_from(POSTPROCESS_UNIFORM_SIZE_LIMIT).expect("limit fits usize") {
        return Err(GraphicsError::invalid_request(format!(
            "postprocess submission supplies {} uniform bytes, exceeding the {}-byte limit and \
             its pipeline's declared size {expected}",
            uniform.len(),
            POSTPROCESS_UNIFORM_SIZE_LIMIT
        )));
    }
    let message = match (expected, uniform.is_empty()) {
        (0, _) => format!(
            "postprocess submission supplies {} unexpected uniform bytes but its pipeline \
             declares no uniform",
            uniform.len()
        ),
        (_, true) => format!(
            "postprocess submission supplies no uniform bytes but its pipeline declares \
             {expected}"
        ),
        _ => format!(
            "postprocess submission supplies {} uniform bytes but its pipeline declares \
             {expected}",
            uniform.len()
        ),
    };
    Err(GraphicsError::invalid_request(message))
}

fn session_ref<'a, 'window>(
    shared: &'a Shared<'window>,
) -> Result<core::cell::Ref<'a, backend::TexturedSession<'window>>, GraphicsError> {
    core::cell::Ref::filter_map(shared.inner.borrow(), Option::as_ref)
        .map_err(|_| GraphicsError::lifecycle("graphics session is shut down"))
}

fn session_mut<'a, 'window>(
    shared: &'a Shared<'window>,
) -> Result<core::cell::RefMut<'a, backend::TexturedSession<'window>>, GraphicsError> {
    // Lazy drops deliberately do not run here: resource creation and diagnostic drains may call
    // this several times in one frame. Surface acquisition owns the single bounded reclamation
    // boundary instead.
    core::cell::RefMut::filter_map(shared.inner.borrow_mut(), Option::as_mut)
        .map_err(|_| GraphicsError::lifecycle("graphics session is shut down"))
}

fn reclaim_lazy_resources(shared: &Shared<'_>) -> Result<(), GraphicsError> {
    let pending = shared.drops.take_bounded(LAZY_RECLAIM_BUDGET);
    if pending.is_empty() {
        return Ok(());
    }
    let mut session = match session_mut(shared) {
        Ok(session) => session,
        Err(error) => {
            shared.drops.restore_front(pending);
            return Err(error);
        }
    };
    if let Err(error) = session.reclaim_resources(&pending) {
        shared.drops.restore_front(pending);
        return Err(error);
    }
    Ok(())
}

fn validate_scene_recipe(submission: &SceneSubmission<'_>) -> Result<Option<usize>, GraphicsError> {
    let (records, foreground) = match submission.content {
        SceneContent::Material(records) => (records, None),
        SceneContent::MaterialWithForeground {
            records,
            foreground_start,
        } => {
            validate_foreground_start(records.len(), foreground_start)?;
            (records, Some(foreground_start))
        }
        _ => (&[][..], None),
    };
    validate_scene_depth_order(
        records
            .iter()
            .map(|record| (record.pipeline.scene_depth, record.pipeline.depth)),
        foreground,
        matches!(submission.output, SceneOutput::Postprocessed { targets, .. } if targets.hdr),
    )?;
    if submission.shadow.is_some()
        && !matches!(
            submission.content,
            SceneContent::Material(_) | SceneContent::MaterialWithForeground { .. }
        )
    {
        return Err(GraphicsError::with_kind(
            GraphicsErrorKind::Unsupported,
            "the shadow pass composes with material scene content only",
        ));
    }
    if !submission.offscreen.is_empty()
        && !matches!(
            (submission.content, submission.output),
            (
                SceneContent::Material(_) | SceneContent::MaterialWithForeground { .. },
                SceneOutput::Postprocessed { .. }
            )
        )
    {
        return Err(GraphicsError::with_kind(
            GraphicsErrorKind::Unsupported,
            "offscreen passes compose with material scene content and postprocessed output only",
        ));
    }
    if submission.overlay.is_some()
        && !matches!(
            (submission.content, submission.output),
            (
                SceneContent::Material(_) | SceneContent::MaterialWithForeground { .. },
                SceneOutput::Postprocessed { .. }
            )
        )
    {
        return Err(GraphicsError::with_kind(
            GraphicsErrorKind::Unsupported,
            "the overlay pass composes with material scene content and postprocessed output \
                 only",
        ));
    }
    match submission.content {
        SceneContent::MaterialWithForeground {
            records,
            foreground_start,
        } => {
            validate_foreground_start(records.len(), foreground_start)?;
            Ok(Some(foreground_start))
        }
        _ => Ok(None),
    }
}

fn validate_foreground_start(
    record_count: usize,
    foreground_start: usize,
) -> Result<(), GraphicsError> {
    if foreground_start == 0 || foreground_start >= record_count {
        return Err(GraphicsError::invalid_request(
            "foreground split must leave non-empty world and foreground material records",
        ));
    }
    Ok(())
}

/// Reads one completed HDR scene pixel for the repository's native validation probe.
/// Call only after rendering these targets with the supplied queue. This is not a supported API.
/// # Errors
/// Rejects mixed sessions, non-HDR/stale targets, or native completion/copy failure.
#[cfg(feature = "native-validation")]
pub fn read_hdr_validation_pixel(
    queue: &Queue<'_>,
    targets: &PostprocessTargets,
) -> Result<[u16; 4], GraphicsError> {
    if targets.lease.session != queue.shared.id || !targets.hdr {
        return Err(GraphicsError::invalid_request(
            "validation readback needs same-session HDR targets",
        ));
    }
    session_mut(&queue.shared)?.read_hdr_validation_pixel(targets.lease.id)
}

#[cfg(test)]
mod foreground_tests {
    use super::validate_foreground_start;

    #[test]
    fn foreground_requires_world_and_foreground_records() {
        assert!(validate_foreground_start(2, 1).is_ok());
        assert!(validate_foreground_start(10, 9).is_ok());
        for (count, split) in [(0, 0), (1, 0), (1, 1), (3, 0), (3, 3), (3, usize::MAX)] {
            assert!(validate_foreground_start(count, split).is_err());
        }
    }
}

#[cfg(test)]
mod offscreen_tests {
    use super::{
        ClearColor, DropQueue, GraphicsErrorKind, OffscreenPass, RenderTargets, RenderTexture,
        ResourceKind, ResourceLease, SceneContent, SceneOutput, SceneSubmission, Texture,
        TextureDimension, validate_scene_recipe,
    };
    use crate::resource::Arena;
    use crate::{SurfaceExtent, SurfaceInfo};
    use std::rc::Rc;

    fn lease(arena: &mut Arena<()>, kind: ResourceKind) -> ResourceLease {
        let mut lease = ResourceLease::new(
            1,
            arena.insert(()).expect("test identity"),
            kind,
            Rc::new(DropQueue::default()),
        );
        lease.disarm();
        lease
    }

    #[test]
    fn offscreen_passes_need_material_content_and_postprocessed_output() {
        let mut arena = Arena::new("resource");
        let target = RenderTexture {
            texture: Texture {
                lease: lease(&mut arena, ResourceKind::Texture),
                dimension: TextureDimension::D2,
                render_target: true,
            },
            width: 4,
            height: 4,
        };
        let targets = RenderTargets {
            lease: lease(&mut arena, ResourceKind::RenderTargets),
            info: SurfaceInfo::initial(SurfaceExtent::new(4, 4)).expect("non-empty extent"),
        };
        let passes = [OffscreenPass {
            target: &target,
            records: &[],
            clear: ClearColor::BLACK,
        }];
        let submission = |content, offscreen| SceneSubmission {
            content,
            output: SceneOutput::Direct(&targets),
            shadow: None,
            overlay: None,
            offscreen,
            clear: ClearColor::BLACK,
        };
        for content in [SceneContent::Material(&[]), SceneContent::Textured(&[])] {
            let refused = validate_scene_recipe(&submission(content, &passes))
                .expect_err("offscreen passes need postprocessed material scenes");
            assert_eq!(refused.kind(), GraphicsErrorKind::Unsupported);
            assert!(validate_scene_recipe(&submission(content, &[])).is_ok());
        }
    }
}

#[cfg(test)]
mod block_compressed_tests {
    use super::{BlockCompression, SampledTextureFormat, validate_mip_level};

    #[test]
    fn a_compressed_level_is_whole_blocks_at_every_extent() {
        let bc7 = BlockCompression::Bc7Srgb.sampled();
        // A full block row, a partial one, and the tail levels narrower than a block.
        assert_eq!(bc7.level_bytes(8, 8), Some(64));
        assert_eq!(bc7.level_bytes(5, 3), Some(32));
        assert_eq!(bc7.level_bytes(2, 2), Some(16));
        assert_eq!(bc7.level_bytes(1, 1), Some(16));
        assert_eq!(bc7.row_bytes(2048), Some(512 * 16));
        assert_eq!(
            BlockCompression::Bc5Unorm.sampled().level_bytes(4, 4),
            Some(16)
        );
        // BC1 packs a block into eight bytes; BC2 and BC3 into sixteen like BC7.
        let bc1 = BlockCompression::Bc1Srgb.sampled();
        assert_eq!(bc1.level_bytes(8, 8), Some(32));
        assert_eq!(bc1.level_bytes(5, 3), Some(16));
        assert_eq!(bc1.level_bytes(1, 1), Some(8));
        assert_eq!(
            BlockCompression::Bc3Unorm.sampled().level_bytes(8, 4),
            Some(32)
        );
        assert_eq!(
            BlockCompression::Bc2Srgb.sampled().level_bytes(2, 2),
            Some(16)
        );
        // The uncompressed formats keep their texel sizes through the same helper.
        assert_eq!(SampledTextureFormat::Srgb.level_bytes(3, 3), Some(36));
        assert_eq!(SampledTextureFormat::Float16.level_bytes(2, 1), Some(16));
    }

    #[test]
    fn a_compressed_mip_chain_is_measured_in_blocks() {
        let bc7 = BlockCompression::Bc7Unorm.sampled();
        let blocks = [0_u8; 64];
        assert!(validate_mip_level(bc7, 8, 8, 0, &blocks).is_ok());
        assert!(validate_mip_level(bc7, 8, 8, 1, &blocks[..16]).is_ok());
        assert!(validate_mip_level(bc7, 8, 8, 2, &blocks[..16]).is_ok());
        assert!(validate_mip_level(bc7, 8, 8, 3, &blocks[..16]).is_ok());
        // The RGBA8 byte count for the same extent is the wrong answer here.
        assert!(validate_mip_level(bc7, 8, 8, 0, &[0; 256]).is_err());
        assert!(validate_mip_level(bc7, 8, 8, 3, &[0; 4]).is_err());
        assert!(validate_mip_level(bc7, 0, 8, 0, &[]).is_err());
        assert!(validate_mip_level(SampledTextureFormat::Unorm, 8, 8, 0, &[0; 256]).is_ok());
        // A BC1 chain's tail levels are each one eight-byte block, not sixteen.
        let bc1 = BlockCompression::Bc1Unorm.sampled();
        assert!(validate_mip_level(bc1, 8, 8, 0, &blocks[..32]).is_ok());
        assert!(validate_mip_level(bc1, 8, 8, 3, &blocks[..8]).is_ok());
        assert!(validate_mip_level(bc1, 8, 8, 3, &blocks[..16]).is_err());
    }
}

#[cfg(test)]
mod slot_tests {
    use super::{
        MATERIAL_BUFFER_SLOT_LIMIT, MATERIAL_SLOT_LIMIT, MATERIAL_TEXTURE_COUNT_LIMIT,
        MATERIAL_TEXTURE_SLOT_LIMIT, MaterialBinding, SamplerAddress, SamplerAddressPerAxis,
        SamplerFilter, validate_bindings_against_interface,
    };
    use std::{vec, vec::Vec};

    use crate::shader::{
        INTERFACE_BINDING_SAMPLED_TEXTURE, INTERFACE_BINDING_SAMPLER, INTERFACE_BINDING_UNIFORM,
        InterfaceBinding, ShaderInterface,
    };

    fn interface(bindings: &[(u32, u8, u32)]) -> ShaderInterface {
        ShaderInterface {
            entry_points: vec![],
            bindings: bindings
                .iter()
                .map(|&(binding, kind, size)| InterfaceBinding {
                    group: 0,
                    binding,
                    kind,
                    size,
                })
                .collect(),
        }
    }

    fn sampler(binding: u32) -> MaterialBinding {
        MaterialBinding::Sampler {
            binding,
            filter: SamplerFilter::Linear,
            address: SamplerAddress::Repeat,
        }
    }

    #[test]
    fn each_kind_of_slot_is_capped_by_its_own_native_table() {
        let texture = MATERIAL_TEXTURE_SLOT_LIMIT;
        let buffer = MATERIAL_BUFFER_SLOT_LIMIT;
        let highest = interface(&[
            (texture, INTERFACE_BINDING_SAMPLED_TEXTURE, 0),
            (buffer, INTERFACE_BINDING_UNIFORM, 64),
            (MATERIAL_SLOT_LIMIT, INTERFACE_BINDING_SAMPLER, 0),
        ]);
        let declaration = validate_bindings_against_interface(
            &[
                MaterialBinding::Texture { binding: texture },
                MaterialBinding::Uniform {
                    binding: buffer,
                    size: 64,
                },
                sampler(MATERIAL_SLOT_LIMIT),
            ],
            &highest,
        )
        .expect("every kind reaches its own ceiling");
        assert_eq!(declaration.texture_bindings, [texture]);

        for (binding, kind, declared) in [
            (
                texture + 1,
                INTERFACE_BINDING_SAMPLED_TEXTURE,
                MaterialBinding::Texture {
                    binding: texture + 1,
                },
            ),
            (
                buffer + 1,
                INTERFACE_BINDING_UNIFORM,
                MaterialBinding::Uniform {
                    binding: buffer + 1,
                    size: 64,
                },
            ),
            (
                MATERIAL_SLOT_LIMIT + 1,
                INTERFACE_BINDING_SAMPLER,
                sampler(MATERIAL_SLOT_LIMIT + 1),
            ),
        ] {
            let size = if kind == INTERFACE_BINDING_UNIFORM {
                64
            } else {
                0
            };
            assert!(
                validate_bindings_against_interface(
                    &[declared],
                    &interface(&[(binding, kind, size)])
                )
                .is_err()
            );
        }
    }

    #[test]
    fn samplers_carry_their_addressing_per_axis() {
        let slots = interface(&[
            (1, INTERFACE_BINDING_SAMPLER, 0),
            (2, INTERFACE_BINDING_SAMPLER, 0),
        ]);
        let panorama = SamplerAddressPerAxis {
            v: SamplerAddress::ClampToEdge,
            ..SamplerAddress::Repeat.into()
        };
        let declaration = validate_bindings_against_interface(
            &[
                MaterialBinding::SamplerPerAxis {
                    binding: 2,
                    filter: SamplerFilter::Linear,
                    address: panorama,
                },
                sampler(1),
            ],
            &slots,
        )
        .expect("a per-axis sampler fills a sampler slot");
        let addresses: Vec<_> = declaration
            .sampler_bindings
            .iter()
            .map(|slot| (slot.binding, slot.address))
            .collect();
        assert_eq!(
            addresses,
            [
                (1, SamplerAddressPerAxis::all(SamplerAddress::Repeat)),
                (2, panorama)
            ]
        );
        // It is a sampler like any other: a texture slot refuses it, and a slot is declared once.
        assert!(
            validate_bindings_against_interface(
                &[MaterialBinding::SamplerPerAxis {
                    binding: 0,
                    filter: SamplerFilter::Nearest,
                    address: panorama,
                }],
                &interface(&[(0, INTERFACE_BINDING_SAMPLED_TEXTURE, 0)]),
            )
            .is_err()
        );
        assert!(
            validate_bindings_against_interface(
                &[
                    sampler(1),
                    MaterialBinding::SamplerPerAxis {
                        binding: 1,
                        filter: SamplerFilter::Linear,
                        address: panorama,
                    }
                ],
                &interface(&[(1, INTERFACE_BINDING_SAMPLER, 0)]),
            )
            .is_err()
        );
    }

    #[test]
    fn a_material_declares_no_more_textures_than_vulkan_guarantees_a_stage() {
        let slots = |count: u32| {
            (0..count)
                .map(|binding| (binding, INTERFACE_BINDING_SAMPLED_TEXTURE, 0))
                .collect::<Vec<_>>()
        };
        let declare = |count: u32| {
            (0..count)
                .map(|binding| MaterialBinding::Texture { binding })
                .collect::<Vec<_>>()
        };
        let most = MATERIAL_TEXTURE_COUNT_LIMIT;
        assert!(
            validate_bindings_against_interface(&declare(most), &interface(&slots(most))).is_ok()
        );
        assert!(
            validate_bindings_against_interface(&declare(most + 1), &interface(&slots(most + 1)))
                .is_err()
        );
    }
}

#[cfg(test)]
mod cube_texture_slot_tests {
    use std::rc::Rc;
    use std::vec;

    use super::{
        MATERIAL_TEXTURE_COUNT_LIMIT, MaterialBinding, Texture, TextureDimension,
        validate_bindings_against_interface, validate_fixed_pipeline_texture,
        validate_record_textures,
    };
    use crate::resource::{Arena, DropQueue, ResourceKind, ResourceLease};
    use crate::shader::{
        INTERFACE_BINDING_CUBE_TEXTURE, INTERFACE_BINDING_CUBE_TEXTURE_ARRAY,
        INTERFACE_BINDING_SAMPLED_TEXTURE, INTERFACE_BINDING_SAMPLER, InterfaceBinding,
        ShaderInterface,
    };

    fn interface(bindings: &[(u32, u8)]) -> ShaderInterface {
        ShaderInterface {
            entry_points: vec![],
            bindings: bindings
                .iter()
                .map(|&(binding, kind)| InterfaceBinding {
                    group: 0,
                    binding,
                    kind,
                    size: 0,
                })
                .collect(),
        }
    }

    fn texture(arena: &mut Arena<()>, dimension: TextureDimension) -> Texture {
        let mut lease = ResourceLease::new(
            1,
            arena.insert(()).expect("test identity"),
            ResourceKind::Texture,
            Rc::new(DropQueue::default()),
        );
        lease.disarm();
        Texture {
            lease,
            dimension,
            render_target: false,
        }
    }

    #[test]
    fn a_cube_slot_matches_only_a_recorded_cube_texture() {
        let declaration = validate_bindings_against_interface(
            &[
                MaterialBinding::CubeTexture { binding: 3 },
                MaterialBinding::Texture { binding: 1 },
            ],
            &interface(&[
                (1, INTERFACE_BINDING_SAMPLED_TEXTURE),
                (3, INTERFACE_BINDING_CUBE_TEXTURE),
            ]),
        )
        .expect("2D and cube slots declared as recorded");
        // Both kinds share the ascending texture list the record supplies.
        assert_eq!(declaration.texture_bindings, [1, 3]);
        assert_eq!(
            declaration.texture_dimensions,
            [TextureDimension::D2, TextureDimension::Cube]
        );

        let error = validate_bindings_against_interface(
            &[MaterialBinding::Texture { binding: 0 }],
            &interface(&[(0, INTERFACE_BINDING_CUBE_TEXTURE)]),
        )
        .err()
        .expect("a 2D declaration of a recorded cube is rejected");
        assert!(error.message().contains("records a cube texture"));
        let error = validate_bindings_against_interface(
            &[MaterialBinding::CubeTexture { binding: 0 }],
            &interface(&[(0, INTERFACE_BINDING_SAMPLED_TEXTURE)]),
        )
        .err()
        .expect("a cube declaration of a recorded 2D texture is rejected");
        assert!(error.message().contains("as a cube texture"));
        assert!(
            validate_bindings_against_interface(
                &[MaterialBinding::CubeTexture { binding: 0 }],
                &interface(&[(0, INTERFACE_BINDING_SAMPLER)]),
            )
            .is_err()
        );
    }

    #[test]
    fn cube_slots_count_against_the_texture_limit() {
        let most = MATERIAL_TEXTURE_COUNT_LIMIT;
        let declare = |count: u32| {
            (0..count)
                .map(|binding| MaterialBinding::CubeTexture { binding })
                .collect::<vec::Vec<_>>()
        };
        let record = |count: u32| {
            interface(
                &(0..count)
                    .map(|binding| (binding, INTERFACE_BINDING_CUBE_TEXTURE))
                    .collect::<vec::Vec<_>>(),
            )
        };
        assert!(validate_bindings_against_interface(&declare(most), &record(most)).is_ok());
        assert!(
            validate_bindings_against_interface(&declare(most + 1), &record(most + 1)).is_err()
        );
    }

    #[test]
    fn records_supply_each_slot_its_own_shape() {
        let mut arena = Arena::new("texture");
        let flat = texture(&mut arena, TextureDimension::D2);
        let cube = texture(&mut arena, TextureDimension::Cube);
        let slots = [TextureDimension::D2, TextureDimension::Cube];
        assert!(validate_record_textures(&[&flat, &cube], &slots).is_ok());
        let error = validate_record_textures(&[&cube, &flat], &slots)
            .expect_err("a cube in a 2D slot is rejected");
        assert!(
            error
                .message()
                .contains("supplies a cube texture for texture 0, but that slot samples a 2D")
        );
        let error = validate_record_textures(&[&flat, &flat], &slots)
            .expect_err("a 2D texture in a cube slot is rejected");
        assert!(
            error
                .message()
                .contains("supplies a 2D texture for texture 1, but that slot samples a cube")
        );
        assert!(validate_record_textures(&[&flat], &slots).is_err());
        // The fixed textured pipelines sample 2D textures only.
        assert!(validate_fixed_pipeline_texture(&flat).is_ok());
        assert!(validate_fixed_pipeline_texture(&cube).is_err());
        assert_eq!(cube.dimension(), TextureDimension::Cube);
    }

    #[test]
    fn a_cube_array_slot_matches_only_a_recorded_cube_array() {
        let declaration = validate_bindings_against_interface(
            &[
                MaterialBinding::CubeTextureArray { binding: 12 },
                MaterialBinding::Texture { binding: 1 },
                MaterialBinding::CubeTexture { binding: 3 },
            ],
            &interface(&[
                (1, INTERFACE_BINDING_SAMPLED_TEXTURE),
                (3, INTERFACE_BINDING_CUBE_TEXTURE),
                (12, INTERFACE_BINDING_CUBE_TEXTURE_ARRAY),
            ]),
        )
        .expect("2D, cube and cube-array slots declared as recorded");
        // All three share the ascending texture list the record supplies.
        assert_eq!(declaration.texture_bindings, [1, 3, 12]);
        assert_eq!(
            declaration.texture_dimensions,
            [
                TextureDimension::D2,
                TextureDimension::Cube,
                TextureDimension::CubeArray
            ]
        );
        for (declared, recorded, message) in [
            (
                MaterialBinding::CubeTexture { binding: 0 },
                INTERFACE_BINDING_CUBE_TEXTURE_ARRAY,
                "as a cube texture but the shader artifact records a cube texture array",
            ),
            (
                MaterialBinding::Texture { binding: 0 },
                INTERFACE_BINDING_CUBE_TEXTURE_ARRAY,
                "as a sampled texture but the shader artifact records a cube texture array",
            ),
            (
                MaterialBinding::CubeTextureArray { binding: 0 },
                INTERFACE_BINDING_CUBE_TEXTURE,
                "as a cube texture array but the shader artifact records a cube texture",
            ),
            (
                MaterialBinding::CubeTextureArray { binding: 0 },
                INTERFACE_BINDING_SAMPLED_TEXTURE,
                "as a cube texture array but the shader artifact records a sampled texture",
            ),
        ] {
            let error =
                validate_bindings_against_interface(&[declared], &interface(&[(0, recorded)]))
                    .err()
                    .expect("a declaration of another texture kind is rejected");
            assert!(error.message().contains(message), "{}", error.message());
        }
        // An undeclared recorded cube array is refused like any undeclared slot.
        assert!(
            validate_bindings_against_interface(
                &[],
                &interface(&[(0, INTERFACE_BINDING_CUBE_TEXTURE_ARRAY)]),
            )
            .is_err()
        );
        // Cube-array slots count against the texture limit with the rest.
        let most = MATERIAL_TEXTURE_COUNT_LIMIT;
        let declare = |count: u32| {
            (0..count)
                .map(|binding| MaterialBinding::CubeTextureArray { binding })
                .collect::<vec::Vec<_>>()
        };
        let record = |count: u32| {
            interface(
                &(0..count)
                    .map(|binding| (binding, INTERFACE_BINDING_CUBE_TEXTURE_ARRAY))
                    .collect::<vec::Vec<_>>(),
            )
        };
        assert!(validate_bindings_against_interface(&declare(most), &record(most)).is_ok());
        assert!(
            validate_bindings_against_interface(&declare(most + 1), &record(most + 1)).is_err()
        );
    }

    #[test]
    fn records_supply_a_cube_array_slot_only_a_cube_array() {
        let mut arena = Arena::new("texture");
        let flat = texture(&mut arena, TextureDimension::D2);
        let cube = texture(&mut arena, TextureDimension::Cube);
        let probes = texture(&mut arena, TextureDimension::CubeArray);
        let slots = [TextureDimension::Cube, TextureDimension::CubeArray];
        assert!(validate_record_textures(&[&cube, &probes], &slots).is_ok());
        let error = validate_record_textures(&[&cube, &cube], &slots)
            .expect_err("a cube in a cube-array slot is rejected");
        assert!(error.message().contains(
            "supplies a cube texture for texture 1, but that slot samples a cube texture array"
        ));
        let error = validate_record_textures(&[&probes, &probes], &slots)
            .expect_err("a cube array in a cube slot is rejected");
        assert!(error.message().contains(
            "supplies a cube texture array for texture 0, but that slot samples a cube texture"
        ));
        assert!(validate_record_textures(&[&cube, &flat], &slots).is_err());
        assert!(validate_fixed_pipeline_texture(&probes).is_err());
        assert_eq!(probes.dimension(), TextureDimension::CubeArray);
    }
}

#[cfg(test)]
mod packed_vertex_format_tests {
    use super::{
        VertexAttribute, VertexFormat, VertexLayout, find_declared_attribute,
        validate_vertex_layout,
    };
    use crate::shader::InterfaceVertexInput;

    #[test]
    fn packed_formats_are_narrow_in_the_buffer_and_wide_in_the_shader() {
        for (format, bytes, wgsl) in [
            (VertexFormat::Uint8x4, 4, "vec4<u32>"),
            (VertexFormat::Unorm8x4, 4, "vec4<f32>"),
            (VertexFormat::Uint16x2, 4, "vec2<u32>"),
            (VertexFormat::Uint16x4, 8, "vec4<u32>"),
            (VertexFormat::Unorm16x2, 4, "vec2<f32>"),
            (VertexFormat::Unorm16x4, 8, "vec4<f32>"),
            (VertexFormat::Float32x3, 12, "vec3<f32>"),
            (VertexFormat::Sint32x4, 16, "vec4<i32>"),
        ] {
            assert_eq!(format.byte_len(), bytes, "{format:?}");
            assert_eq!(format.wgsl_name(), wgsl, "{format:?}");
            assert_eq!(
                VertexFormat::from_interface_code(format.interface_code())
                    .map(VertexFormat::wgsl_name),
                Some(wgsl)
            );
        }
        assert_eq!(
            VertexFormat::Uint8x4.describe(),
            "Uint8x4 (read as vec4<u32>)"
        );
        assert_eq!(VertexFormat::Float32x2.describe(), "vec2<f32>");
    }

    #[test]
    fn layouts_keep_four_byte_strides_and_offsets() {
        let attribute = |location, offset, format| VertexAttribute {
            location,
            format,
            offset,
        };
        let skinned = [
            attribute(0, 0, VertexFormat::Float32x3),
            attribute(1, 12, VertexFormat::Uint8x4),
            attribute(2, 16, VertexFormat::Unorm8x4),
            attribute(3, 20, VertexFormat::Unorm16x2),
        ];
        let layout = |stride, attributes| VertexLayout { stride, attributes };
        assert!(validate_vertex_layout(layout(24, &skinned)).is_ok());
        // The packed attributes still have to fit inside the stride.
        assert!(validate_vertex_layout(layout(22, &skinned)).is_err());
        // A stride or an offset that is not a multiple of four is refused.
        assert!(validate_vertex_layout(layout(26, &skinned)).is_err());
        let unaligned = [attribute(0, 2, VertexFormat::Uint8x4)];
        let error = validate_vertex_layout(layout(8, &unaligned)).expect_err("offset 2");
        assert!(error.message().contains("not a multiple of four bytes"));
    }

    #[test]
    fn a_packed_attribute_satisfies_its_wide_wgsl_type_only() {
        let attributes = [
            VertexAttribute {
                location: 4,
                format: VertexFormat::Uint8x4,
                offset: 0,
            },
            VertexAttribute {
                location: 5,
                format: VertexFormat::Unorm8x4,
                offset: 4,
            },
        ];
        let layout = validate_vertex_layout(VertexLayout {
            stride: 8,
            attributes: &attributes,
        })
        .expect("valid layout");
        let input = |location, format| InterfaceVertexInput { location, format };
        let uint4 = VertexFormat::Uint32x4.interface_code();
        let float4 = VertexFormat::Float32x4.interface_code();
        assert!(find_declared_attribute(&layout, None, input(4, uint4), "skin").is_ok());
        assert!(find_declared_attribute(&layout, None, input(5, float4), "skin").is_ok());
        let error = find_declared_attribute(&layout, None, input(4, float4), "skin")
            .expect_err("bone indices read as floats");
        assert!(error.message().contains(
            "supply location 4 as Uint8x4 (read as vec4<u32>) but the shader artifact records \
             vec4<f32>"
        ));
        let float3 = VertexFormat::Float32x3.interface_code();
        assert!(find_declared_attribute(&layout, None, input(5, float3), "skin").is_err());
    }
}

#[cfg(test)]
mod entry_point_binding_tests {
    use super::{MaterialBinding, SamplerAddress, SamplerFilter, validate_entry_point_bindings};
    use std::{string::ToString, vec, vec::Vec};

    use crate::shader::{
        INTERFACE_BINDING_SAMPLED_TEXTURE, INTERFACE_BINDING_SAMPLER, INTERFACE_BINDING_STORAGE,
        INTERFACE_BINDING_UNIFORM, INTERFACE_STAGE_FRAGMENT, INTERFACE_STAGE_VERTEX,
        InterfaceBinding, InterfaceEntryPoint, ShaderInterface,
    };

    /// One module: a uniform, a texture and sampler, and a bone palette only the skinned vertex
    /// entry point reaches.
    fn module() -> ShaderInterface {
        let entry = |stage, name: &str, used: &[usize]| InterfaceEntryPoint {
            stage,
            name: name.to_string(),
            inputs: vec![],
            used: used.to_vec(),
        };
        let binding = |binding, kind, size| InterfaceBinding {
            group: 0,
            binding,
            kind,
            size,
        };
        ShaderInterface {
            entry_points: vec![
                entry(INTERFACE_STAGE_VERTEX, "prop_vertex", &[0]),
                entry(INTERFACE_STAGE_VERTEX, "skinned_vertex", &[0, 3]),
                entry(INTERFACE_STAGE_FRAGMENT, "prop_fragment", &[0, 1, 2]),
            ],
            bindings: vec![
                binding(0, INTERFACE_BINDING_UNIFORM, 80),
                binding(1, INTERFACE_BINDING_SAMPLED_TEXTURE, 0),
                binding(2, INTERFACE_BINDING_SAMPLER, 0),
                binding(3, INTERFACE_BINDING_STORAGE, 3072),
            ],
        }
    }

    fn declare(storage: bool) -> Vec<MaterialBinding> {
        let mut bindings = vec![
            MaterialBinding::Uniform {
                binding: 0,
                size: 80,
            },
            MaterialBinding::Texture { binding: 1 },
            MaterialBinding::Sampler {
                binding: 2,
                filter: SamplerFilter::Linear,
                address: SamplerAddress::Repeat,
            },
        ];
        if storage {
            bindings.push(MaterialBinding::Storage {
                binding: 3,
                size: 3072,
            });
        }
        bindings
    }

    #[test]
    fn each_pipeline_declares_the_union_of_its_own_entry_points() {
        let interface = module();
        let [prop, skinned, fragment] = [0, 1, 2].map(|index| &interface.entry_points[index]);
        let plain = validate_entry_point_bindings(&declare(false), &interface, &[prop, fragment])
            .expect("the plain pipeline declares no storage slot");
        assert!(plain.storage.is_none());
        assert_eq!(plain.texture_bindings, [1]);
        let skinning =
            validate_entry_point_bindings(&declare(true), &interface, &[skinned, fragment])
                .expect("the skinned pipeline declares the bone palette");
        assert_eq!(skinning.storage, Some((3, 3072)));
        // A vertex-only pipeline of the plain entry point binds the uniform alone.
        let uniform = [MaterialBinding::Uniform {
            binding: 0,
            size: 80,
        }];
        assert!(validate_entry_point_bindings(&uniform, &interface, &[prop]).is_ok());
    }

    #[test]
    fn a_slot_outside_the_entry_points_or_a_missing_one_is_refused() {
        let interface = module();
        let [prop, skinned, fragment] = [0, 1, 2].map(|index| &interface.entry_points[index]);
        let error = validate_entry_point_bindings(&declare(true), &interface, &[prop, fragment])
            .err()
            .expect("the plain pair never reads the bone palette");
        assert!(error.message().contains(
            "slot 3, which the shader module records but entry points `prop_vertex` and \
             `prop_fragment` do not use"
        ));
        let error =
            validate_entry_point_bindings(&declare(false), &interface, &[skinned, fragment])
                .err()
                .expect("the skinned pair reads the bone palette");
        assert!(
            error
                .message()
                .contains("records binding slot 3 for the pipeline's entry points")
        );
        // A slot the module never records keeps its own diagnostic.
        let mut unknown = declare(false);
        unknown.push(MaterialBinding::Texture { binding: 7 });
        let error = validate_entry_point_bindings(&unknown, &interface, &[prop, fragment])
            .err()
            .expect("slot 7 is not in the module");
        assert!(
            error
                .message()
                .contains("slot 7 that the shader artifact does not record")
        );
    }

    #[test]
    fn a_module_wide_artifact_attributes_every_binding_to_every_entry_point() {
        let mut interface = module();
        for entry in &mut interface.entry_points {
            entry.used = (0..interface.bindings.len()).collect();
        }
        let [prop, _, fragment] = [0, 1, 2].map(|index| &interface.entry_points[index]);
        assert!(
            validate_entry_point_bindings(&declare(false), &interface, &[prop, fragment]).is_err()
        );
        assert!(
            validate_entry_point_bindings(&declare(true), &interface, &[prop, fragment]).is_ok()
        );
    }
}

#[cfg(test)]
mod mesh_indices_tests {
    use super::MeshIndices;

    #[test]
    fn out_of_range_checks_the_largest_index() {
        assert!(!MeshIndices::U16(&[0, 2, 1]).out_of_range(3));
        assert!(MeshIndices::U16(&[0, 3, 1]).out_of_range(3));
        assert!(!MeshIndices::U16(&[]).out_of_range(0));
        assert!(!MeshIndices::U32(&[5, 0, 4]).out_of_range(6));
        assert!(MeshIndices::U32(&[0, 1, 6]).out_of_range(6));
        assert!(MeshIndices::U32(&[u32::MAX, 0]).out_of_range(1 << 20));
    }
}
