use super::{DepthMode, GraphicsError};

/// The snapshot is immutable through the remaining world records. Foreground has its
/// own depth clear and must not accidentally sample the world's depth.
pub(super) fn validate_scene_depth_order(
    records: impl Iterator<Item = (bool, DepthMode)>,
    foreground: Option<usize>,
    hdr_postprocessed: bool,
) -> Result<(), GraphicsError> {
    let mut snapshot = false;
    for (index, (samples, depth)) in records.enumerate() {
        let in_foreground = foreground.is_some_and(|start| index >= start);
        if samples {
            if !hdr_postprocessed || in_foreground || index == 0 {
                return Err(GraphicsError::invalid_request(
                    "scene depth requires preceding world records and HDR postprocessed output; it is unavailable in foreground records",
                ));
            }
            snapshot = true;
        }
        if snapshot
            && !in_foreground
            && matches!(depth, DepthMode::TestWrite | DepthMode::TestWriteGreater)
        {
            return Err(GraphicsError::invalid_request(
                "world records at or after the scene-depth snapshot may not write depth",
            ));
        }
    }
    Ok(())
}

impl super::MaterialPipelineConfig<'_> {
    pub(crate) fn validate_scene_depth_samples(
        &self,
        multisampled: bool,
    ) -> Result<(), GraphicsError> {
        if self
            .scene_depth_binding
            .is_some_and(|(_, expected)| expected != multisampled)
        {
            return Err(GraphicsError::invalid_request(
                "scene-depth shader texture kind does not match the selected scene sample count",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod scene_depth_tests {
    use super::{DepthMode::*, validate_scene_depth_order};
    use std::vec;

    #[test]
    fn snapshot_preserves_world_then_allows_independent_foreground_depth() {
        let records = [
            (false, TestWriteGreater),
            (true, TestOnlyGreater),
            (false, Off),
            (false, TestWriteGreater),
        ];
        assert!(validate_scene_depth_order(records.into_iter(), Some(3), true).is_ok());
        assert!(validate_scene_depth_order(records.into_iter(), None, true).is_err());
        assert!(validate_scene_depth_order(records.into_iter(), Some(3), false).is_err());
    }

    #[test]
    fn rejects_missing_opaque_pass_foreground_sampling_and_writable_consumers() {
        for records in [
            vec![(true, TestOnly)],
            vec![(false, TestWrite), (true, TestWrite)],
            vec![(false, TestWrite), (true, TestOnly), (false, TestWrite)],
        ] {
            assert!(validate_scene_depth_order(records.into_iter(), None, true).is_err());
        }
        assert!(
            validate_scene_depth_order(
                [(false, TestWrite), (true, TestOnly)].into_iter(),
                Some(1),
                true
            )
            .is_err()
        );
        assert!(validate_scene_depth_order([(false, TestWrite)].into_iter(), None, false).is_ok());
    }
    #[test]
    fn scene_depth_reflection_distinguishes_msaa_and_can_coexist_with_shadows() {
        use super::super::{MaterialBinding, validate_bindings_against_interface};
        use crate::shader::{
            INTERFACE_BINDING_DEPTH_TEXTURE, INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY,
            INTERFACE_BINDING_MULTISAMPLED_DEPTH, INTERFACE_BINDING_SAMPLED_TEXTURE,
            InterfaceBinding, ShaderInterface,
        };
        for (kind, msaa) in [
            (INTERFACE_BINDING_DEPTH_TEXTURE, false),
            (INTERFACE_BINDING_MULTISAMPLED_DEPTH, true),
        ] {
            let interface = ShaderInterface {
                entry_points: vec![],
                bindings: vec![
                    InterfaceBinding {
                        group: 0,
                        binding: 2,
                        kind,
                        size: 0,
                    },
                    InterfaceBinding {
                        group: 0,
                        binding: 3,
                        kind: INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY,
                        size: 0,
                    },
                ],
            };
            let declaration = validate_bindings_against_interface(
                &[
                    MaterialBinding::SceneDepth { binding: 2 },
                    MaterialBinding::DepthTextureArray { binding: 3 },
                ],
                &interface,
            )
            .expect("scene depth and shadows occupy separate slots");
            assert_eq!(declaration.scene_depth, Some((2, msaa)));
            assert_eq!(declaration.depth_texture_array, Some(3));
            assert!(declaration.texture_bindings.is_empty());
            assert!(
                validate_bindings_against_interface(
                    &[
                        MaterialBinding::SceneDepth { binding: 2 },
                        MaterialBinding::SceneDepth { binding: 3 }
                    ],
                    &interface
                )
                .is_err()
            );
        }
        let interface = ShaderInterface {
            entry_points: vec![],
            bindings: vec![InterfaceBinding {
                group: 0,
                binding: 2,
                kind: INTERFACE_BINDING_SAMPLED_TEXTURE,
                size: 0,
            }],
        };
        assert!(
            validate_bindings_against_interface(
                &[MaterialBinding::SceneDepth { binding: 2 }],
                &interface
            )
            .is_err()
        );
    }
}
