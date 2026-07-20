//! Native `VK_EXT_present_timing` configuration and drained presented-time feedback.
//!
//! When the adapter and surface support the extension chain, every present carries a
//! `VK_KHR_present_id2` identifier and a timing request for one present stage, and the probe
//! drains completed reports after each present. The CPU present-return estimation keeps running
//! beside it so the two paths can be compared under the Gate 4 load-spike scenario.

use super::{ProbeError, Renderer, check, check_enumeration, vk};

/// Presentation-timing requests the driver may buffer before the probe drains them. The probe
/// drains after every present, so this only needs to cover frames in flight plus slack.
const TIMING_QUEUE_SIZE: u32 = 32;

/// Per-swapchain native present-timing state, recreated with the swapchain it describes.
pub(super) struct SwapchainPresentTiming {
    /// Swapchain-reported identifier of the time domain every request asks for.
    time_domain_id: u64,
    /// Monotonically increasing `VK_KHR_present_id2` value for this swapchain.
    next_present_id: u64,
    /// Chained present ids paired with the pacing frame index whose return they identify, oldest
    /// first. Presents that did not reach the presentation engine keep `None` and stay untimed.
    pending: Vec<(u64, Option<u64>)>,
}

/// One present's native timing request, copied out of the per-swapchain state.
pub(super) struct PresentTimingRequest {
    pub(super) present_id: u64,
    pub(super) time_domain_id: u64,
    pub(super) stage: u32,
}

impl Renderer {
    /// Configures native present timing for the freshly created current swapchain, or records the
    /// observable reason this swapchain falls back to estimation.
    pub(super) fn configure_present_timing(&mut self) -> Result<(), ProbeError> {
        self.present_timing = None;
        if self.device.adapter.present_timing.is_err() {
            return Ok(());
        }
        check(
            // SAFETY: The device and freshly created swapchain are live and unpresented.
            unsafe {
                self.device
                    .functions
                    .set_swapchain_present_timing_queue_size
                    .expect("loaded function")(
                    self.device.handle,
                    self.swapchain,
                    TIMING_QUEUE_SIZE,
                )
            },
            "vkSetSwapchainPresentTimingQueueSizeEXT",
        )?;
        let mut timing_properties = vk::VkSwapchainTimingPropertiesEXT {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_TIMING_PROPERTIES_EXT,
            ..Default::default()
        };
        let mut timing_properties_counter = 0_u64;
        check(
            // SAFETY: Device/swapchain are live and both outputs are writable.
            unsafe {
                self.device
                    .functions
                    .get_swapchain_timing_properties
                    .expect("loaded function")(
                    self.device.handle,
                    self.swapchain,
                    &raw mut timing_properties,
                    &raw mut timing_properties_counter,
                )
            },
            "vkGetSwapchainTimingPropertiesEXT",
        )?;
        self.present_pacing
            .record_refresh_duration(timing_properties.refreshDuration);

        let function = self
            .device
            .functions
            .get_swapchain_time_domain_properties
            .expect("loaded function");
        let mut domain_properties = vk::VkSwapchainTimeDomainPropertiesEXT {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_TIME_DOMAIN_PROPERTIES_EXT,
            ..Default::default()
        };
        let mut domains_counter = 0_u64;
        // SAFETY: This is the Vulkan two-call enumeration pattern over the properties struct.
        check_enumeration(
            unsafe {
                function(
                    self.device.handle,
                    self.swapchain,
                    &raw mut domain_properties,
                    &raw mut domains_counter,
                )
            },
            "enumerate swapchain time domains",
        )?;
        let capacity = domain_properties.timeDomainCount as usize;
        let mut domains = vec![vk::VkTimeDomainKHR::default(); capacity];
        let mut domain_ids = vec![0_u64; capacity];
        domain_properties.pTimeDomains = domains.as_mut_ptr();
        domain_properties.pTimeDomainIds = domain_ids.as_mut_ptr();
        // SAFETY: Both arrays contain `timeDomainCount` writable entries.
        check_enumeration(
            unsafe {
                function(
                    self.device.handle,
                    self.swapchain,
                    &raw mut domain_properties,
                    &raw mut domains_counter,
                )
            },
            "enumerate swapchain time domains",
        )?;
        let returned = (domain_properties.timeDomainCount as usize).min(capacity);
        let selected = domains[..returned]
            .iter()
            .zip(&domain_ids[..returned])
            .find(|(domain, _)| **domain == vk::VK_TIME_DOMAIN_CLOCK_MONOTONIC_KHR)
            .or_else(|| {
                domains[..returned]
                    .iter()
                    .zip(&domain_ids[..returned])
                    .next()
            });
        let Some((&domain, &time_domain_id)) = selected else {
            self.present_pacing
                .record_native_inactive("swapchain exposes no time domain");
            return Ok(());
        };
        self.present_pacing
            .record_native_domain(time_domain_label(domain));
        self.present_timing = Some(SwapchainPresentTiming {
            time_domain_id,
            next_present_id: 0,
            pending: Vec::new(),
        });
        Ok(())
    }

    /// Issues the next present id and timing request for the current swapchain, if native timing
    /// is configured.
    pub(super) fn present_timing_request(&mut self) -> Option<PresentTimingRequest> {
        let requested_stage = self.device.adapter.present_timing.ok()?.stage;
        let state = self.present_timing.as_mut()?;
        state.next_present_id += 1;
        Some(PresentTimingRequest {
            present_id: state.next_present_id,
            time_domain_id: state.time_domain_id,
            stage: requested_stage,
        })
    }

    /// Records whether a chained present id reached the presentation engine as pacing frame
    /// `frame`, so a drained report can be attributed to it.
    pub(super) fn record_present_timing_outcome(&mut self, present_id: u64, frame: Option<u64>) {
        if let Some(state) = self.present_timing.as_mut() {
            state.pending.push((present_id, frame));
        }
    }

    /// Drains every completed presentation-timing report for the current swapchain into the
    /// pacing record.
    pub(super) fn drain_present_timing(&mut self) -> Result<(), ProbeError> {
        let Some(state) = self.present_timing.as_ref() else {
            return Ok(());
        };
        if state.pending.is_empty() {
            return Ok(());
        }
        let function = self
            .device
            .functions
            .get_past_presentation_timing
            .expect("loaded function");
        let info = vk::VkPastPresentationTimingInfoEXT {
            sType: vk::VK_STRUCTURE_TYPE_PAST_PRESENTATION_TIMING_INFO_EXT,
            swapchain: self.swapchain,
            ..Default::default()
        };
        let mut properties = vk::VkPastPresentationTimingPropertiesEXT {
            sType: vk::VK_STRUCTURE_TYPE_PAST_PRESENTATION_TIMING_PROPERTIES_EXT,
            ..Default::default()
        };
        // SAFETY: This is the Vulkan two-call enumeration pattern over the properties struct.
        check_enumeration(
            unsafe { function(self.device.handle, &raw const info, &raw mut properties) },
            "query past presentation timing",
        )?;
        let capacity = properties.presentationTimingCount as usize;
        if capacity == 0 {
            return Ok(());
        }
        let mut stage_times = vec![vk::VkPresentStageTimeEXT::default(); capacity];
        let mut timings: Vec<vk::VkPastPresentationTimingEXT> = stage_times
            .iter_mut()
            .map(|slot| vk::VkPastPresentationTimingEXT {
                sType: vk::VK_STRUCTURE_TYPE_PAST_PRESENTATION_TIMING_EXT,
                presentStageCount: 1,
                pPresentStages: &raw mut *slot,
                ..Default::default()
            })
            .collect();
        properties.pPresentationTimings = timings.as_mut_ptr();
        // SAFETY: The array holds `presentationTimingCount` entries and each entry provides one
        // writable present-stage slot for the single requested stage.
        check_enumeration(
            unsafe { function(self.device.handle, &raw const info, &raw mut properties) },
            "query past presentation timing",
        )?;
        let returned = (properties.presentationTimingCount as usize).min(capacity);
        for timing in &timings[..returned] {
            let state = self
                .present_timing
                .as_mut()
                .expect("present-timing state is retained across the drain");
            let Some(position) = state
                .pending
                .iter()
                .position(|&(id, _)| id == timing.presentId)
            else {
                continue;
            };
            let (_, frame) = state.pending.remove(position);
            if timing.reportComplete == vk::VK_TRUE && timing.presentStageCount >= 1 {
                // SAFETY: The driver wrote this entry's single stage slot allocated above.
                let stage_time = unsafe { (*timing.pPresentStages).time };
                if let Some(frame) = frame {
                    self.present_pacing.record_native_time(frame, stage_time);
                }
            }
        }
        Ok(())
    }

    /// Drops the per-swapchain timing state before the current swapchain is retired; frames whose
    /// reports never arrived stay untimed.
    pub(super) fn abandon_present_timing(&mut self) {
        self.present_timing = None;
    }
}

fn time_domain_label(domain: vk::VkTimeDomainKHR) -> &'static str {
    if domain == vk::VK_TIME_DOMAIN_CLOCK_MONOTONIC_KHR {
        "clock-monotonic time domain"
    } else {
        "device-selected time domain"
    }
}
