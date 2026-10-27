//! The devices sound goes out through and comes in from, as the host knows them, and finding one
//! again by its id.

use cpal::traits::{DeviceTrait, HostTrait};

/// An output or input device as the host knows it: its `id`, the same from run to run, to keep
/// in a game's settings and open it by, and its `name` to show the player.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioDevice {
    /// The host's id for it, which `AudioEngine::on_device` and
    /// `Microphone::open_on` open it by.
    pub id: String,
    /// What it's called, as the system's own sound settings show it.
    pub name: String,
}

/// Which way a device carries sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    #[cfg_attr(not(feature = "input"), allow(dead_code))]
    Input,
    #[cfg_attr(not(feature = "output"), allow(dead_code))]
    Output,
}

/// The host's devices that carry sound `direction`, in its order; none if it can't list them.
pub(crate) fn list(direction: Direction) -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let devices: Vec<cpal::Device> = match direction {
        Direction::Input => host.input_devices().map(Iterator::collect),
        Direction::Output => host.output_devices().map(Iterator::collect),
    }
    .unwrap_or_default();
    devices.iter().filter_map(described).collect()
}

/// The device `id` names that carries sound `direction`, or the host's default for that way with
/// no id; `Err` with no default, `Ok(None)` when no device has that id (unplugged, or another
/// machine's).
pub(crate) fn find(direction: Direction, id: Option<&str>) -> Result<Option<cpal::Device>, ()> {
    let host = cpal::default_host();
    let Some(id) = id else {
        let default = match direction {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        };
        return default.map(Some).ok_or(());
    };
    let carries = |device: &cpal::Device| match direction {
        Direction::Input => device.supports_input(),
        Direction::Output => device.supports_output(),
    };
    Ok(id
        .parse::<cpal::DeviceId>()
        .ok()
        .and_then(|id| host.device_by_id(&id))
        .filter(carries))
}

/// The id and name of `device`, if it's still there to be asked; its id for a name it doesn't
/// give.
fn described(device: &cpal::Device) -> Option<AudioDevice> {
    let id = device.id().ok()?.to_string();
    let name = device
        .description()
        .map_or_else(|_| id.clone(), |d| d.name().to_owned());
    Some(AudioDevice { id, name })
}
