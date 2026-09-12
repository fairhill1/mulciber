//! Capability checks for immutable, linearly filterable sampled uploads.
use super::{ClearSurface, GraphicsError, check, vk};

pub(super) fn validate_format(
    surface: &ClearSurface<'_>,
    format: vk::VkFormat,
    width: u32,
    height: u32,
    levels: u32,
) -> Result<(), GraphicsError> {
    let unsupported = || {
        GraphicsError::with_kind(
            crate::GraphicsErrorKind::Unsupported,
            "sampled texture format/extent requires transfer destination and linear filtering (RGBA16Float for float uploads)",
        )
    };
    let mut properties = vk::VkFormatProperties::default();
    let functions = &surface.device().instance.functions;
    unsafe {
        functions
            .get_physical_device_format_properties
            .expect("loaded function")(
            surface.device().adapter.handle,
            format,
            &raw mut properties,
        );
    }
    let required = (vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT
        | vk::VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT
        | vk::VK_FORMAT_FEATURE_TRANSFER_DST_BIT)
        .cast_unsigned();
    if properties.optimalTilingFeatures & required != required {
        return Err(unsupported());
    }
    let mut image = vk::VkImageFormatProperties::default();
    let result = unsafe {
        functions
            .get_physical_device_image_format_properties
            .expect("loaded function")(
            surface.device().adapter.handle,
            format,
            vk::VK_IMAGE_TYPE_2D,
            vk::VK_IMAGE_TILING_OPTIMAL,
            (vk::VK_IMAGE_USAGE_SAMPLED_BIT | vk::VK_IMAGE_USAGE_TRANSFER_DST_BIT).cast_unsigned(),
            0,
            &raw mut image,
        )
    };
    if result == vk::VK_ERROR_FORMAT_NOT_SUPPORTED {
        return Err(unsupported());
    }
    check(result, "sampled texture image format support")?;
    if width > image.maxExtent.width
        || height > image.maxExtent.height
        || levels > image.maxMipLevels
        || image.sampleCounts & vk::VK_SAMPLE_COUNT_1_BIT.cast_unsigned() == 0
    {
        return Err(unsupported());
    }
    Ok(())
}
