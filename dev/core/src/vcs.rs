//! Volume Control Service - the volume buttons on the headphones.
//!
//! In LE Audio the headphones are the server: they own the volume, and both
//! sides see the same number. Pressing volume up on the earcup changes the
//! Volume State characteristic and notifies us; moving the Windows slider means
//! writing the Volume Control Point. Without this the two drift apart, and the
//! buttons appear to do nothing because only the headphones know they were
//! pressed.
//!
//! Every write carries the change counter from the last state we saw, which is
//! how the server rejects a command based on a value that has since moved. That
//! is not ceremony: two volume sources racing is exactly the situation this
//! protects against.

/// Assigned numbers for the service and its characteristics.
pub mod uuid {
    pub const VOLUME_CONTROL_SERVICE: u16 = 0x1844;
    pub const VOLUME_STATE: u16 = 0x2B7D;
    pub const VOLUME_CONTROL_POINT: u16 = 0x2B7E;
    pub const VOLUME_FLAGS: u16 = 0x2B7F;
}

/// Volume Control Point opcodes.
pub mod op {
    pub const RELATIVE_DOWN: u8 = 0x00;
    pub const RELATIVE_UP: u8 = 0x01;
    pub const UNMUTE_DOWN: u8 = 0x02;
    pub const UNMUTE_UP: u8 = 0x03;
    pub const SET_ABSOLUTE: u8 = 0x04;
    pub const UNMUTE: u8 = 0x05;
    pub const MUTE: u8 = 0x06;
}

/// What the headphones currently think the volume is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeState {
    /// 0 to 255, the whole usable range.
    pub setting: u8,
    pub muted: bool,
    /// Echoed back on every write, so a stale command is refused.
    pub change_counter: u8,
}

impl VolumeState {
    /// The setting as a percentage, for showing to a person.
    pub fn percent(&self) -> u8 {
        ((self.setting as u16 * 100 + 127) / 255) as u8
    }

    /// The setting as the 0.0-1.0 scalar Windows uses for an endpoint.
    pub fn scalar(&self) -> f32 {
        self.setting as f32 / 255.0
    }

    /// Converts a Windows endpoint scalar into a setting for the headphones.
    pub fn setting_from_scalar(scalar: f32) -> u8 {
        (scalar.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

/// Reads a Volume State value, from a read response or a notification.
pub fn parse_volume_state(value: &[u8]) -> Option<VolumeState> {
    let &[setting, mute, change_counter, ..] = value else {
        return None;
    };

    Some(VolumeState {
        setting,
        muted: mute != 0,
        change_counter,
    })
}

/// Builds a control point write that sets an exact volume.
pub fn set_absolute(state: &VolumeState, setting: u8) -> Vec<u8> {
    vec![op::SET_ABSOLUTE, state.change_counter, setting]
}

/// Builds a control point write that mutes or unmutes.
pub fn set_muted(state: &VolumeState, muted: bool) -> Vec<u8> {
    let opcode = if muted { op::MUTE } else { op::UNMUTE };
    vec![opcode, state.change_counter]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notification_is_read_the_same_way_as_a_read() {
        let state = parse_volume_state(&[0x80, 0x00, 0x07]).unwrap();

        assert_eq!(state.setting, 128);
        assert!(!state.muted);
        assert_eq!(state.change_counter, 7);
        assert_eq!(state.percent(), 50);
    }

    #[test]
    fn a_short_value_is_not_guessed_at() {
        assert_eq!(parse_volume_state(&[0x80, 0x00]), None);
        assert_eq!(parse_volume_state(&[]), None);
    }

    #[test]
    fn a_write_carries_the_counter_we_last_saw() {
        let state = VolumeState { setting: 100, muted: false, change_counter: 42 };

        assert_eq!(set_absolute(&state, 200), vec![op::SET_ABSOLUTE, 42, 200]);
        assert_eq!(set_muted(&state, true), vec![op::MUTE, 42]);
        assert_eq!(set_muted(&state, false), vec![op::UNMUTE, 42]);
    }

    #[test]
    fn the_windows_scale_and_the_bluetooth_scale_round_trip() {
        for setting in [0u8, 1, 63, 128, 200, 255] {
            let state = VolumeState { setting, muted: false, change_counter: 0 };
            assert_eq!(VolumeState::setting_from_scalar(state.scalar()), setting);
        }
    }

    #[test]
    fn the_extremes_stay_at_the_extremes() {
        assert_eq!(VolumeState::setting_from_scalar(0.0), 0);
        assert_eq!(VolumeState::setting_from_scalar(1.0), 255);
        assert_eq!(VolumeState::setting_from_scalar(-5.0), 0);
        assert_eq!(VolumeState::setting_from_scalar(9.0), 255);
    }
}
