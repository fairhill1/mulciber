//! HDR format agreement and the fixed bloom shader interface.
use super::{GraphicsError, ShaderArtifact, format, shader};

pub(super) fn validate_hdr_pair(pipeline: bool, target: bool) -> Result<(), GraphicsError> {
    if pipeline != target {
        return Err(GraphicsError::invalid_request(
            "pipeline and target HDR formats disagree",
        ));
    }
    Ok(())
}

pub(super) fn validate_bloom_interface(
    shader: ShaderArtifact<'_>,
    uniform_size: Option<u32>,
) -> Result<(), GraphicsError> {
    let interface = shader.parse_interface();
    if uniform_size.is_none()
        && interface
            .bindings
            .iter()
            .any(|slot| slot.group == 0 && slot.binding == 0)
    {
        return Err(GraphicsError::invalid_request(
            "HDR composite uniform must be declared explicitly",
        ));
    }
    for binding in 3..9 {
        if !interface.bindings.iter().any(|slot| {
            slot.group == 0
                && slot.binding == binding
                && slot.kind == shader::INTERFACE_BINDING_SAMPLED_TEXTURE
        }) {
            return Err(GraphicsError::invalid_request(format!(
                "HDR composite requires bloom texture at group 0, binding {binding}"
            )));
        }
    }
    if interface
        .bindings
        .iter()
        .any(|slot| slot.group != 0 || slot.binding > 8)
    {
        return Err(GraphicsError::invalid_request(
            "HDR composite contains unsupported bindings",
        ));
    }
    Ok(())
}

pub(super) fn validate_bloom_filter_interface(
    shader: ShaderArtifact<'_>,
) -> Result<(), GraphicsError> {
    if shader
        .parse_interface()
        .bindings
        .iter()
        .any(|slot| slot.group != 0 || !matches!(slot.binding, 1 | 2))
    {
        return Err(GraphicsError::invalid_request(
            "bloom filters accept only texture 1 and sampler 2",
        ));
    }
    Ok(())
}

/// Repeated halving keeps tiny and non-square targets valid. Six levels are always supplied.
pub(crate) fn bloom_extents(width: u32, height: u32) -> [(u32, u32); 6] {
    let mut extent = (width, height);
    core::array::from_fn(|_| {
        extent = ((extent.0 / 2).max(1), (extent.1 / 2).max(1));
        extent
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphicsErrorKind;
    use crate::graphics::validate_postprocess_interface;
    use std::vec::Vec;

    #[test]
    fn hdr_and_surface_formats_cannot_be_mixed() {
        assert!(validate_hdr_pair(true, true).is_ok());
        assert!(validate_hdr_pair(false, false).is_ok());
        for (pipeline, target) in [(true, false), (false, true)] {
            assert_eq!(
                validate_hdr_pair(pipeline, target).unwrap_err().kind(),
                GraphicsErrorKind::InvalidRequest
            );
        }
    }

    #[test]
    fn bloom_handles_native_scaled_odd_and_minimum_extents() {
        assert_eq!(
            bloom_extents(1920, 1080),
            [
                (960, 540),
                (480, 270),
                (240, 135),
                (120, 67),
                (60, 33),
                (30, 16)
            ]
        );
        assert_eq!(bloom_extents(1, 1), [(1, 1); 6]);
        assert_eq!(
            bloom_extents(1, 7),
            [(1, 3), (1, 1), (1, 1), (1, 1), (1, 1), (1, 1)]
        );
        for (width, height) in [(2559, 1439), (1080, 1920), (127, 63), (960, 540)] {
            let levels = bloom_extents(width, height);
            assert!(levels.iter().all(|&(w, h)| w > 0 && h > 0));
            assert!(
                levels
                    .windows(2)
                    .all(|p| p[1].0 <= p[0].0 && p[1].1 <= p[0].1)
            );
            let pixels: u64 = levels
                .iter()
                .map(|&(w, h)| u64::from(w) * u64::from(h))
                .sum();
            assert!(pixels <= u64::from(width) * u64::from(height) / 3);
        }
    }

    // Reflection-only fixture: no native module is loaded and no device/window is created.
    fn artifact(slots: &[(u32, u8)]) -> Vec<u8> {
        let mut interface = Vec::new();
        interface.extend_from_slice(&2_u32.to_le_bytes());
        for (stage, name) in [
            (shader::INTERFACE_STAGE_VERTEX, "post_vertex"),
            (shader::INTERFACE_STAGE_FRAGMENT, "post_fragment"),
        ] {
            interface.push(stage);
            interface.extend_from_slice(&u32::try_from(name.len()).unwrap().to_le_bytes());
            interface.extend_from_slice(name.as_bytes());
            interface.extend_from_slice(&0_u32.to_le_bytes());
        }
        interface.extend_from_slice(&u32::try_from(slots.len()).unwrap().to_le_bytes());
        for &(binding, kind) in slots {
            interface.extend_from_slice(&0_u32.to_le_bytes());
            interface.extend_from_slice(&binding.to_le_bytes());
            interface.push(kind);
            interface.extend_from_slice(&0_u32.to_le_bytes());
        }
        let mut bytes = Vec::from(&b"MULSHDR2"[..]);
        bytes.extend_from_slice(
            &(if cfg!(target_os = "macos") {
                2_u32
            } else {
                1_u32
            })
            .to_le_bytes(),
        );
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(interface.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&0x0723_0203_u32.to_le_bytes());
        bytes.extend(interface);
        bytes
    }

    #[test]
    fn bloom_requires_every_level_and_rejects_wrong_binding_kinds() {
        let texture = shader::INTERFACE_BINDING_SAMPLED_TEXTURE;
        let sampler = shader::INTERFACE_BINDING_SAMPLER;
        let mut slots = std::vec![(1, texture), (2, sampler)];
        slots.extend((3..9).map(|slot| (slot, texture)));
        let bytes = artifact(&slots);
        let shader = ShaderArtifact::new(&bytes).unwrap();
        assert!(validate_postprocess_interface(shader, None).is_ok());
        assert!(validate_bloom_interface(shader, None).is_ok());
        for missing in 2..slots.len() {
            let mut incomplete = slots.clone();
            incomplete.remove(missing);
            let bytes = artifact(&incomplete);
            assert!(validate_bloom_interface(ShaderArtifact::new(&bytes).unwrap(), None).is_err());
        }
        slots[4].1 = sampler;
        let bytes = artifact(&slots);
        assert!(validate_bloom_interface(ShaderArtifact::new(&bytes).unwrap(), None).is_err());
    }

    #[test]
    fn downsample_cannot_silently_require_unbound_uniforms_or_textures() {
        let mut slots = std::vec![
            (1, shader::INTERFACE_BINDING_SAMPLED_TEXTURE),
            (2, shader::INTERFACE_BINDING_SAMPLER)
        ];
        let bytes = artifact(&slots);
        let shader = ShaderArtifact::new(&bytes).unwrap();
        assert!(validate_postprocess_interface(shader, None).is_ok());
        assert!(validate_bloom_filter_interface(shader).is_ok());
        slots.push((0, shader::INTERFACE_BINDING_UNIFORM));
        let bytes = artifact(&slots);
        assert!(validate_bloom_filter_interface(ShaderArtifact::new(&bytes).unwrap()).is_err());
        slots[2] = (3, shader::INTERFACE_BINDING_SAMPLED_TEXTURE);
        let bytes = artifact(&slots);
        assert!(validate_bloom_filter_interface(ShaderArtifact::new(&bytes).unwrap()).is_err());
    }
}
