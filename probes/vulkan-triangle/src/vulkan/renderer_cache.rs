use super::{
    Duration, PipelineCachePersistence, ProbeError, Renderer, check, fs, pipeline_cache_uuid_hex,
    ptr, replace_file_atomically, validate_pipeline_cache_header, vk,
};

impl Renderer {
    pub(super) fn create_pipeline_cache(&mut self, rebuild: bool) -> Result<(), ProbeError> {
        if self.pipeline_cache.is_disabled() {
            println!("Pipeline cache mode: disabled (correctness control)");
            return Ok(());
        }
        let identity = self.device.adapter.pipeline_cache_identity;
        println!(
            "Pipeline cache identity: vendor=0x{:04x} device=0x{:04x} uuid={}",
            identity.vendor_id,
            identity.device_id,
            pipeline_cache_uuid_hex(identity)
        );
        let mut initial_data = Vec::new();
        if rebuild {
            println!(
                "Pipeline cache: rebuilding {} from an empty cache",
                self.pipeline_cache.path.display()
            );
        } else {
            match fs::read(&self.pipeline_cache.path) {
                Ok(bytes) => match validate_pipeline_cache_header(&bytes, identity) {
                    Ok(()) => {
                        println!(
                            "Pipeline cache: loaded {} compatible bytes from {}",
                            bytes.len(),
                            self.pipeline_cache.path.display()
                        );
                        initial_data = bytes;
                    }
                    Err(reason) if self.pipeline_cache.is_strict() => {
                        return Err(ProbeError(format!(
                            "strict pipeline cache rejected {}: {reason}",
                            self.pipeline_cache.path.display()
                        )));
                    }
                    Err(reason) => println!(
                        "Pipeline cache: ignored incompatible artifact {} ({reason})",
                        self.pipeline_cache.path.display()
                    ),
                },
                Err(error)
                    if error.kind() != std::io::ErrorKind::NotFound
                        || self.pipeline_cache.is_strict() =>
                {
                    return Err(ProbeError(format!(
                        "pipeline cache could not read {} in {} mode: {error}",
                        self.pipeline_cache.path.display(),
                        if self.pipeline_cache.is_strict() {
                            "strict"
                        } else {
                            "learning"
                        }
                    )));
                }
                Err(error) => println!(
                    "Pipeline cache: starting empty because {} could not be read ({error})",
                    self.pipeline_cache.path.display()
                ),
            }
        }

        let mut result = self.create_pipeline_cache_handle(&initial_data);
        if result != vk::VK_SUCCESS && !initial_data.is_empty() && !self.pipeline_cache.is_strict()
        {
            if !self.pipeline_cache.handle.is_null() {
                // SAFETY: A failed creation unexpectedly returned a handle; discard it before retry.
                unsafe {
                    self.device
                        .functions
                        .destroy_pipeline_cache
                        .expect("loaded function")(
                        self.device.handle,
                        self.pipeline_cache.handle,
                        ptr::null(),
                    );
                }
                self.pipeline_cache.handle = ptr::null_mut();
            }
            println!(
                "Pipeline cache: driver rejected compatible artifact {} with VkResult {result}; retrying empty",
                self.pipeline_cache.path.display()
            );
            initial_data.clear();
            result = self.create_pipeline_cache_handle(&initial_data);
        }
        check(result, "vkCreatePipelineCache")?;
        println!(
            "Pipeline cache mode: {} ({})",
            if self.pipeline_cache.is_strict() {
                "strict read-only hit proof"
            } else {
                "learning with atomic persistence"
            },
            self.pipeline_cache.path.display()
        );
        Ok(())
    }

    pub(super) fn create_pipeline_cache_handle(&mut self, bytes: &[u8]) -> vk::VkResult {
        let info = vk::VkPipelineCacheCreateInfo {
            sType: vk::VK_STRUCTURE_TYPE_PIPELINE_CACHE_CREATE_INFO,
            initialDataSize: bytes.len(),
            pInitialData: if bytes.is_empty() {
                ptr::null()
            } else {
                bytes.as_ptr().cast()
            },
            ..Default::default()
        };
        self.pipeline_cache.handle = ptr::null_mut();
        // SAFETY: The device is live, initial bytes outlive the call, and output is writable.
        unsafe {
            self.device
                .functions
                .create_pipeline_cache
                .expect("loaded function")(
                self.device.handle,
                &raw const info,
                ptr::null(),
                &raw mut self.pipeline_cache.handle,
            )
        }
    }

    pub(super) fn pipeline_create_flags(&self) -> vk::VkPipelineCreateFlags {
        if self.pipeline_cache.is_strict() && self.device.adapter.pipeline_creation_cache_control {
            vk::VK_PIPELINE_CREATE_FAIL_ON_PIPELINE_COMPILE_REQUIRED_BIT as u32
        } else {
            0
        }
    }

    pub(super) fn check_pipeline_feedback(
        &self,
        name: &str,
        result: vk::VkResult,
        feedback: vk::VkPipelineCreationFeedback,
        elapsed: Duration,
    ) -> Result<(), ProbeError> {
        if result == vk::VK_PIPELINE_COMPILE_REQUIRED {
            return Err(ProbeError(format!(
                "pipeline cache miss for {name}: compilation was required in strict mode"
            )));
        }
        check(result, &format!("pipeline creation for {name}"))?;
        let valid = feedback.flags & vk::VK_PIPELINE_CREATION_FEEDBACK_VALID_BIT as u32 != 0;
        let hit = feedback.flags
            & vk::VK_PIPELINE_CREATION_FEEDBACK_APPLICATION_PIPELINE_CACHE_HIT_BIT as u32
            != 0;
        println!(
            "Pipeline cache feedback: name={name} valid={valid} app_hit={hit} driver={:.3} ms cpu={:.3} ms",
            Duration::from_nanos(feedback.duration).as_secs_f64() * 1_000.0,
            elapsed.as_secs_f64() * 1_000.0
        );
        if !valid {
            return Err(ProbeError(format!(
                "pipeline creation feedback for {name} was not valid"
            )));
        }
        if self.pipeline_cache.is_strict() && !hit {
            return Err(ProbeError(format!(
                "pipeline cache miss for {name}: application cache hit feedback was absent"
            )));
        }
        Ok(())
    }

    pub(super) fn save_pipeline_cache(&mut self) -> Result<(), ProbeError> {
        if self.pipeline_cache.persistence != PipelineCachePersistence::Pending
            || self.pipeline_cache.is_strict()
            || self.pipeline_cache.handle.is_null()
        {
            return Ok(());
        }
        let get_data = self
            .device
            .functions
            .get_pipeline_cache_data
            .expect("loaded function");
        let mut complete_bytes = None;
        for _ in 0..8 {
            let mut size = 0_usize;
            // SAFETY: The cache is live and the size output is writable.
            check(
                unsafe {
                    get_data(
                        self.device.handle,
                        self.pipeline_cache.handle,
                        &raw mut size,
                        ptr::null_mut(),
                    )
                },
                "vkGetPipelineCacheData size query",
            )?;
            let mut bytes = vec![0_u8; size];
            // SAFETY: Storage has `size` writable bytes and the cache remains live.
            let result = unsafe {
                get_data(
                    self.device.handle,
                    self.pipeline_cache.handle,
                    &raw mut size,
                    bytes.as_mut_ptr().cast(),
                )
            };
            if result == vk::VK_INCOMPLETE {
                continue;
            }
            check(result, "vkGetPipelineCacheData payload query")?;
            bytes.truncate(size);
            complete_bytes = Some(bytes);
            break;
        }
        let bytes = complete_bytes.ok_or_else(|| {
            ProbeError("vkGetPipelineCacheData remained incomplete after 8 retries".into())
        })?;
        validate_pipeline_cache_header(&bytes, self.device.adapter.pipeline_cache_identity)
            .map_err(|reason| {
                ProbeError(format!(
                    "driver returned an invalid pipeline cache artifact: {reason}"
                ))
            })?;
        replace_file_atomically(&self.pipeline_cache.path, &bytes)?;
        self.pipeline_cache.persistence = PipelineCachePersistence::Saved;
        println!(
            "Pipeline cache: atomically stored {} bytes at {}",
            bytes.len(),
            self.pipeline_cache.path.display()
        );
        Ok(())
    }
}
