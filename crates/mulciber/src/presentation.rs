use core::num::NonZeroU64;

/// A two-dimensional extent in physical surface pixels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SurfaceExtent {
    width: u32,
    height: u32,
}

impl SurfaceExtent {
    /// Creates a physical surface extent.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns the width in physical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the height in physical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Returns whether either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Identifies one graphics-owned presentation configuration.
///
/// This is distinct from a desktop OS window revision. A graphics backend advances the generation
/// whenever it successfully replaces the native presentation configuration, even if the pixel extent
/// is unchanged.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SurfaceGeneration(NonZeroU64);

impl SurfaceGeneration {
    /// The first configured presentation generation.
    pub const INITIAL: Self = Self(NonZeroU64::MIN);

    /// Returns the numeric generation identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns the next generation, or `None` if the identifier space is exhausted.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(value) => match NonZeroU64::new(value) {
                Some(value) => Some(Self(value)),
                None => None,
            },
            None => None,
        }
    }
}

/// Observable facts about one configured presentation generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceInfo {
    extent: SurfaceExtent,
    generation: SurfaceGeneration,
}

impl SurfaceInfo {
    /// Creates information for the first non-empty presentation configuration.
    #[must_use]
    pub const fn initial(extent: SurfaceExtent) -> Option<Self> {
        if extent.is_empty() {
            None
        } else {
            Some(Self {
                extent,
                generation: SurfaceGeneration::INITIAL,
            })
        }
    }

    /// Creates the next presentation generation with a non-empty extent.
    #[must_use]
    pub const fn reconfigured(self, extent: SurfaceExtent) -> Option<Self> {
        if extent.is_empty() {
            return None;
        }
        match self.generation.next() {
            Some(generation) => Some(Self { extent, generation }),
            None => None,
        }
    }

    /// Returns the configured physical extent.
    #[must_use]
    pub const fn extent(self) -> SurfaceExtent {
        self.extent
    }

    /// Returns the graphics-owned presentation generation.
    #[must_use]
    pub const fn generation(self) -> SurfaceGeneration {
        self.generation
    }
}

/// A nonfatal reason why a presentation surface cannot currently provide a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SurfaceUnavailable {
    /// The desktop OS reports no drawable extent, such as while minimized.
    Suspended,
    /// The native presentation system did not provide a drawable or image yet.
    DrawableUnavailable,
    /// A bounded native acquisition attempt expired without producing a frame.
    TimedOut,
}

/// The result of attempting to acquire a surface-scoped frame.
///
/// `F` is the backend-owned frame representation. It will become Mulciber's concrete scoped frame
/// type only after both native backends consume this lifecycle contract.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "frame acquisition must be handled so a ready frame receives one disposition"]
pub enum FrameAcquire<F> {
    /// A frame is ready and must be presented or explicitly abandoned.
    Ready(F),
    /// The surface is temporarily unavailable; the device remains usable.
    Unavailable(SurfaceUnavailable),
    /// The surface was reconfigured and application-owned dependent resources must be rebuilt.
    Reconfigured(SurfaceInfo),
}

impl<F> FrameAcquire<F> {
    /// Maps the ready frame while preserving lifecycle outcomes.
    pub fn map_ready<T>(self, map: impl FnOnce(F) -> T) -> FrameAcquire<T> {
        match self {
            Self::Ready(frame) => FrameAcquire::Ready(map(frame)),
            Self::Unavailable(reason) => FrameAcquire::Unavailable(reason),
            Self::Reconfigured(info) => FrameAcquire::Reconfigured(info),
        }
    }
}

/// The completed disposition of one acquired frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FrameDisposition {
    /// Rendering was submitted and the frame was queued for presentation.
    Presented(SurfaceGeneration),
    /// The frame was safely released without presentation.
    Abandoned(SurfaceGeneration),
}

impl FrameDisposition {
    /// Returns the surface generation after the disposition completed.
    #[must_use]
    pub const fn generation(self) -> SurfaceGeneration {
        match self {
            Self::Presented(generation) | Self::Abandoned(generation) => generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameAcquire, FrameDisposition, SurfaceExtent, SurfaceGeneration, SurfaceInfo,
        SurfaceUnavailable,
    };

    #[test]
    fn empty_extent_cannot_describe_a_configured_surface() {
        assert!(SurfaceInfo::initial(SurfaceExtent::new(0, 720)).is_none());
        assert!(SurfaceInfo::initial(SurfaceExtent::new(1280, 0)).is_none());
    }

    #[test]
    fn every_reconfiguration_advances_the_graphics_generation() {
        let first = SurfaceInfo::initial(SurfaceExtent::new(1280, 720)).unwrap();
        let second = first.reconfigured(SurfaceExtent::new(1280, 720)).unwrap();
        let third = second.reconfigured(SurfaceExtent::new(1920, 1080)).unwrap();

        assert_eq!(first.generation().get(), 1);
        assert_eq!(second.generation().get(), 2);
        assert_eq!(third.generation().get(), 3);
        assert_eq!(third.extent(), SurfaceExtent::new(1920, 1080));
    }

    #[test]
    fn acquisition_mapping_cannot_erase_lifecycle_outcomes() {
        assert_eq!(
            FrameAcquire::Ready(4_u32).map_ready(u64::from),
            FrameAcquire::Ready(4_u64)
        );
        assert_eq!(
            FrameAcquire::<u32>::Unavailable(SurfaceUnavailable::TimedOut).map_ready(u64::from),
            FrameAcquire::Unavailable(SurfaceUnavailable::TimedOut)
        );
        let info = SurfaceInfo::initial(SurfaceExtent::new(960, 540)).unwrap();
        assert_eq!(
            FrameAcquire::<u32>::Reconfigured(info).map_ready(u64::from),
            FrameAcquire::Reconfigured(info)
        );
    }

    #[test]
    fn dispositions_report_the_resulting_generation() {
        assert_eq!(
            FrameDisposition::Presented(SurfaceGeneration::INITIAL).generation(),
            SurfaceGeneration::INITIAL
        );
        assert_eq!(
            FrameDisposition::Abandoned(SurfaceGeneration::INITIAL).generation(),
            SurfaceGeneration::INITIAL
        );
    }
}
