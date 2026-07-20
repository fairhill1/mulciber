//! Native `VK_EXT_present_timing` presentation feedback for the Vulkan sessions.
//!
//! When the adapter and surface support the `VK_KHR_present_id2` +
//! `VK_KHR_calibrated_timestamps` + `VK_EXT_present_timing` chain, every present carries a
//! present id and one present-stage timing request, and completed reports are drained
//! non-blockingly into the bounded feedback queue behind `Surface::take_present_feedback`.
//!
//! Native present-stage times arrive in a swapchain-scoped time domain whose epoch is not the
//! process clock on the physically surveyed tier, so each swapchain's times are re-anchored to
//! the `Instant` at which its first completed report was drained. Intervals between drained
//! display times are therefore native-exact within one swapchain, while their absolute placement
//! carries at most one drain latency of bias; times are never paired across swapchain
//! recreations, whose domains restart at unrelated epochs.

use core::ptr;
use std::time::{Duration, Instant};
use std::vec;
use std::vec::Vec;

use super::{ClearSurface, Instance, check, check_enumeration, vk};
use crate::{GraphicsError, PresentFeedback, PresentedFrame};

/// Presentation-timing requests the driver may buffer before the session drains them. Draining
/// happens after every present, so this only needs to cover frames in flight plus slack.
const TIMING_QUEUE_SIZE: u32 = 32;

/// Undrained samples and outstanding requests kept while the application ignores feedback.
const PRESENT_FEEDBACK_CAP: usize = 1024;

/// Native present-timing feedback selected for the chosen adapter and surface, or the observable
/// reason this session answers [`PresentFeedback::Unsupported`] on every drain.
#[derive(Clone, Copy)]
pub(super) struct PresentTimingSelection {
    /// The single present stage every timing request asks a timestamp for.
    pub(super) stage: u32,
}

/// Per-swapchain native present-timing state, replaced with the swapchain it describes.
pub(super) struct PresentTiming {
    /// Swapchain-reported identifier of the time domain every request asks for.
    time_domain_id: u64,
    /// Monotonically increasing `VK_KHR_present_id2` value for this swapchain.
    next_present_id: u64,
    /// Chained present ids paired with the session frame index they identify, oldest first.
    pending: Vec<(u64, u64)>,
    /// Drain instant and native time of this swapchain's first completed report; later native
    /// times become `Instant`s relative to this pair.
    anchor: Option<(Instant, u64)>,
}

/// One present's native timing request, copied out of the per-swapchain state.
struct PresentTimingRequest {
    present_id: u64,
    time_domain_id: u64,
    stage: u32,
}

/// Prefers the stage closest to light leaving the display, falling back through earlier stages.
fn choose_present_stage(supported: u32) -> Option<u32> {
    [
        vk::VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_VISIBLE_BIT_EXT as u32,
        vk::VK_PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT_EXT as u32,
        vk::VK_PRESENT_STAGE_REQUEST_DEQUEUED_BIT_EXT as u32,
        vk::VK_PRESENT_STAGE_QUEUE_OPERATIONS_END_BIT_EXT as u32,
    ]
    .into_iter()
    .find(|stage| supported & stage != 0)
}

/// Chooses native present timing for one physical device against the instance surface, or the
/// observable reason this session reports feedback as unsupported.
pub(super) fn choose_present_timing(
    instance: &Instance,
    device: vk::VkPhysicalDevice,
    timing_extensions: bool,
    present_id2_features: &vk::VkPhysicalDevicePresentId2FeaturesKHR,
    present_timing_features: &vk::VkPhysicalDevicePresentTimingFeaturesEXT,
) -> Result<Result<PresentTimingSelection, &'static str>, GraphicsError> {
    if !timing_extensions {
        return Ok(Err(
            "device lacks VK_KHR_present_id2, VK_KHR_calibrated_timestamps, or \
             VK_EXT_present_timing",
        ));
    }
    if !instance.surface_capabilities2 {
        return Ok(Err("instance lacks VK_KHR_get_surface_capabilities2"));
    }
    if present_id2_features.presentId2 != vk::VK_TRUE
        || present_timing_features.presentTiming != vk::VK_TRUE
    {
        return Ok(Err(
            "device does not enable the presentId2/presentTiming features",
        ));
    }
    let mut id2_capabilities = vk::VkSurfaceCapabilitiesPresentId2KHR {
        sType: vk::VK_STRUCTURE_TYPE_SURFACE_CAPABILITIES_PRESENT_ID_2_KHR,
        ..Default::default()
    };
    let mut timing_capabilities = vk::VkPresentTimingSurfaceCapabilitiesEXT {
        sType: vk::VK_STRUCTURE_TYPE_PRESENT_TIMING_SURFACE_CAPABILITIES_EXT,
        pNext: (&raw mut id2_capabilities).cast(),
        ..Default::default()
    };
    let mut capabilities = vk::VkSurfaceCapabilities2KHR {
        sType: vk::VK_STRUCTURE_TYPE_SURFACE_CAPABILITIES_2_KHR,
        pNext: (&raw mut timing_capabilities).cast(),
        ..Default::default()
    };
    let surface_info = vk::VkPhysicalDeviceSurfaceInfo2KHR {
        sType: vk::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SURFACE_INFO_2_KHR,
        surface: instance.surface,
        ..Default::default()
    };
    check(
        unsafe {
            // SAFETY: Device and surface are live, and the query chain structs are writable.
            instance
                .functions
                .get_surface_capabilities2
                .expect("loaded function")(
                device, &raw const surface_info, &raw mut capabilities
            )
        },
        "vkGetPhysicalDeviceSurfaceCapabilities2KHR",
    )?;
    if timing_capabilities.presentTimingSupported != vk::VK_TRUE {
        return Ok(Err("surface reports no present-timing support"));
    }
    if id2_capabilities.presentId2Supported != vk::VK_TRUE {
        return Ok(Err("surface reports no present-id2 support"));
    }
    Ok(
        choose_present_stage(timing_capabilities.presentStageQueries)
            .map(|stage| PresentTimingSelection { stage })
            .ok_or("surface exposes no present-stage timestamps"),
    )
}

impl ClearSurface<'_> {
    /// Configures native present timing for the freshly created current swapchain. A swapchain
    /// that exposes no time domain leaves timing unconfigured, so its frames stay unreported.
    pub(super) fn configure_present_timing(&mut self) -> Result<(), GraphicsError> {
        self.present_timing = None;
        if self.device().adapter.present_timing.is_err() {
            return Ok(());
        }
        let device_handle = self.device().handle;
        let swapchain = self.swapchain.handle;
        check(
            unsafe {
                // SAFETY: The device and freshly created swapchain are live and unpresented.
                self.device()
                    .functions
                    .set_swapchain_present_timing_queue_size
                    .expect("loaded function")(
                    device_handle, swapchain, TIMING_QUEUE_SIZE
                )
            },
            "vkSetSwapchainPresentTimingQueueSizeEXT",
        )?;
        let function = self
            .device()
            .functions
            .get_swapchain_time_domain_properties
            .expect("loaded function");
        let mut domain_properties = vk::VkSwapchainTimeDomainPropertiesEXT {
            sType: vk::VK_STRUCTURE_TYPE_SWAPCHAIN_TIME_DOMAIN_PROPERTIES_EXT,
            ..Default::default()
        };
        let mut domains_counter = 0_u64;
        check_enumeration(
            unsafe {
                // SAFETY: This is the Vulkan two-call enumeration pattern over the properties
                // struct.
                function(
                    device_handle,
                    swapchain,
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
        check_enumeration(
            unsafe {
                // SAFETY: Both arrays contain `timeDomainCount` writable entries.
                function(
                    device_handle,
                    swapchain,
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
        if let Some((_, &time_domain_id)) = selected {
            self.present_timing = Some(PresentTiming {
                time_domain_id,
                next_present_id: 0,
                pending: Vec::new(),
                anchor: None,
            });
        }
        Ok(())
    }

    /// Drops per-swapchain timing state before its swapchain is retired; frames whose reports
    /// never arrived stay unreported.
    pub(super) fn abandon_present_timing(&mut self) {
        self.present_timing = None;
    }

    /// Presents one frame, chaining a present id and native timing request when configured, and
    /// drains completed reports afterward. Every call consumes one session frame index so
    /// feedback indices count presented dispositions.
    pub(super) fn queue_present_with_feedback(
        &mut self,
        image_index: u32,
        render_finished: vk::VkSemaphore,
        operation: &str,
    ) -> Result<(), GraphicsError> {
        let request = self.present_timing_request();
        let present_id = request.as_ref().map_or(0, |request| request.present_id);
        let present_id2 = vk::VkPresentId2KHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_ID_2_KHR,
            pNext: ptr::null(),
            swapchainCount: 1,
            pPresentIds: &raw const present_id,
        };
        let timing_info = vk::VkPresentTimingInfoEXT {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_TIMING_INFO_EXT,
            timeDomainId: request.as_ref().map_or(0, |request| request.time_domain_id),
            presentStageQueries: request.as_ref().map_or(0, |request| request.stage),
            ..Default::default()
        };
        let timing_chain = vk::VkPresentTimingsInfoEXT {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_TIMINGS_INFO_EXT,
            pNext: (&raw const present_id2).cast(),
            swapchainCount: 1,
            pTimingInfos: &raw const timing_info,
        };
        let present = vk::VkPresentInfoKHR {
            sType: vk::VK_STRUCTURE_TYPE_PRESENT_INFO_KHR,
            pNext: if request.is_some() {
                (&raw const timing_chain).cast()
            } else {
                ptr::null()
            },
            waitSemaphoreCount: 1,
            pWaitSemaphores: &raw const render_finished,
            swapchainCount: 1,
            pSwapchains: &raw const self.swapchain.handle,
            pImageIndices: &raw const image_index,
            ..Default::default()
        };
        let result = unsafe {
            // SAFETY: Queue, swapchain, image index, wait semaphore, and the optional timing
            // chain are live for this call.
            self.device()
                .functions
                .queue_present
                .expect("loaded function")(self.device().queue, &raw const present)
        };
        let frame_index = self.presented_count;
        self.presented_count += 1;
        if let Some(request) = request {
            self.record_present_timing_outcome(request.present_id, frame_index);
        }
        if matches!(result, vk::VK_SUBOPTIMAL_KHR | vk::VK_ERROR_OUT_OF_DATE_KHR) {
            self.recreate_after_present = true;
        } else {
            check(result, operation)?;
        }
        self.drain_present_timing()
    }

    /// Issues the next present id and timing request for the current swapchain, if native timing
    /// is configured.
    fn present_timing_request(&mut self) -> Option<PresentTimingRequest> {
        let selection = self.device().adapter.present_timing.ok()?;
        let state = self.present_timing.as_mut()?;
        state.next_present_id += 1;
        Some(PresentTimingRequest {
            present_id: state.next_present_id,
            time_domain_id: state.time_domain_id,
            stage: selection.stage,
        })
    }

    /// Records which session frame index a chained present id identifies, so a drained report
    /// can be attributed to it.
    fn record_present_timing_outcome(&mut self, present_id: u64, frame_index: u64) {
        if let Some(state) = self.present_timing.as_mut() {
            if state.pending.len() >= PRESENT_FEEDBACK_CAP {
                state.pending.remove(0);
            }
            state.pending.push((present_id, frame_index));
        }
    }

    /// Drains every completed presentation-timing report for the current swapchain into the
    /// bounded feedback queue.
    pub(super) fn drain_present_timing(&mut self) -> Result<(), GraphicsError> {
        let Some(state) = self.present_timing.as_ref() else {
            return Ok(());
        };
        if state.pending.is_empty() {
            return Ok(());
        }
        let function = self
            .device()
            .functions
            .get_past_presentation_timing
            .expect("loaded function");
        let device_handle = self.device().handle;
        let info = vk::VkPastPresentationTimingInfoEXT {
            sType: vk::VK_STRUCTURE_TYPE_PAST_PRESENTATION_TIMING_INFO_EXT,
            swapchain: self.swapchain.handle,
            ..Default::default()
        };
        let mut properties = vk::VkPastPresentationTimingPropertiesEXT {
            sType: vk::VK_STRUCTURE_TYPE_PAST_PRESENTATION_TIMING_PROPERTIES_EXT,
            ..Default::default()
        };
        check_enumeration(
            unsafe {
                // SAFETY: This is the Vulkan two-call enumeration pattern over the properties
                // struct.
                function(device_handle, &raw const info, &raw mut properties)
            },
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
        check_enumeration(
            unsafe {
                // SAFETY: The array holds `presentationTimingCount` entries and each entry
                // provides one writable present-stage slot for the single requested stage.
                function(device_handle, &raw const info, &raw mut properties)
            },
            "query past presentation timing",
        )?;
        let drained_at = Instant::now();
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
            let (_, frame_index) = state.pending.remove(position);
            let presented_at = if timing.reportComplete == vk::VK_TRUE
                && timing.presentStageCount >= 1
            {
                // SAFETY: The driver wrote this entry's single stage slot allocated above.
                let time = unsafe { (*timing.pPresentStages).time };
                let (anchor_instant, anchor_time) = *state.anchor.get_or_insert((drained_at, time));
                if time >= anchor_time {
                    anchor_instant.checked_add(Duration::from_nanos(time - anchor_time))
                } else {
                    anchor_instant.checked_sub(Duration::from_nanos(anchor_time - time))
                }
            } else {
                None
            };
            if self.feedback.len() >= PRESENT_FEEDBACK_CAP {
                self.feedback.pop_front();
            }
            self.feedback
                .push_back(PresentedFrame::new(frame_index, presented_at));
        }
        Ok(())
    }

    /// Non-blocking drain of native presentation feedback behind
    /// `Surface::take_present_feedback`; drain failures surface through the session's next
    /// fallible operation.
    pub(super) fn take_present_feedback(&mut self) -> PresentFeedback {
        if self.device().adapter.present_timing.is_err() {
            return PresentFeedback::Unsupported;
        }
        if let Err(error) = self.drain_present_timing()
            && self.deferred_error.is_none()
        {
            self.deferred_error = Some(error);
        }
        let mut frames: Vec<PresentedFrame> = self.feedback.drain(..).collect();
        frames.sort_by_key(PresentedFrame::index);
        PresentFeedback::Reported(frames)
    }
}
