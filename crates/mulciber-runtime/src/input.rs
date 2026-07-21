use mulciber_platform::{
    ButtonState, InputEvent, KeyCode, LogicalPosition, Modifiers, PointerButton, ScrollDelta,
};

/// One scroll transition retained in the frame input snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollSample {
    delta: ScrollDelta,
    position: LogicalPosition,
}

impl ScrollSample {
    /// Returns the precise or coarse scroll delta supplied by the platform.
    #[must_use]
    pub const fn delta(self) -> ScrollDelta {
        self.delta
    }

    /// Returns the pointer position associated with the scroll transition.
    #[must_use]
    pub const fn position(self) -> LogicalPosition {
        self.position
    }
}

/// Held controls and transitions accumulated for the next simulation-bearing frame.
#[derive(Debug)]
pub struct InputSnapshot {
    focused: bool,
    modifiers: Modifiers,
    pointer_position: Option<LogicalPosition>,
    held_keys: Vec<KeyCode>,
    pressed_keys: Vec<KeyCode>,
    released_keys: Vec<KeyCode>,
    held_pointer_buttons: Vec<PointerButton>,
    pressed_pointer_buttons: Vec<PointerButton>,
    released_pointer_buttons: Vec<PointerButton>,
    scroll: Vec<ScrollSample>,
}

impl Default for InputSnapshot {
    fn default() -> Self {
        Self {
            focused: true,
            modifiers: Modifiers::default(),
            pointer_position: None,
            held_keys: Vec::new(),
            pressed_keys: Vec::new(),
            released_keys: Vec::new(),
            held_pointer_buttons: Vec::new(),
            pressed_pointer_buttons: Vec::new(),
            released_pointer_buttons: Vec::new(),
            scroll: Vec::new(),
        }
    }
}

impl InputSnapshot {
    /// Returns whether the application window currently has keyboard focus.
    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    /// Returns the latest aggregate modifier state.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Returns the latest known pointer position in logical window coordinates.
    #[must_use]
    pub const fn pointer_position(&self) -> Option<LogicalPosition> {
        self.pointer_position
    }

    /// Returns whether `key` is currently held.
    #[must_use]
    pub fn key_held(&self, key: KeyCode) -> bool {
        self.held_keys.contains(&key)
    }

    /// Returns whether `key` became pressed since transient input was last consumed.
    #[must_use]
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.pressed_keys.contains(&key)
    }

    /// Returns whether `key` became released since transient input was last consumed.
    #[must_use]
    pub fn key_released(&self, key: KeyCode) -> bool {
        self.released_keys.contains(&key)
    }

    /// Returns whether `button` is currently held.
    #[must_use]
    pub fn pointer_button_held(&self, button: PointerButton) -> bool {
        self.held_pointer_buttons.contains(&button)
    }

    /// Returns whether `button` became pressed since transient input was last consumed.
    #[must_use]
    pub fn pointer_button_pressed(&self, button: PointerButton) -> bool {
        self.pressed_pointer_buttons.contains(&button)
    }

    /// Returns whether `button` became released since transient input was last consumed.
    #[must_use]
    pub fn pointer_button_released(&self, button: PointerButton) -> bool {
        self.released_pointer_buttons.contains(&button)
    }

    /// Returns ordered scroll transitions accumulated since transient input was last consumed.
    #[must_use]
    pub fn scroll(&self) -> &[ScrollSample] {
        &self.scroll
    }

    pub(crate) fn handle_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::FocusChanged { focused } => self.set_focus(focused),
            InputEvent::Keyboard {
                key,
                state,
                modifiers,
                ..
            } => {
                self.modifiers = modifiers;
                update_button(
                    key,
                    state,
                    &mut self.held_keys,
                    &mut self.pressed_keys,
                    &mut self.released_keys,
                );
            }
            InputEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            InputEvent::PointerMoved {
                position,
                modifiers,
            } => {
                self.pointer_position = Some(position);
                self.modifiers = modifiers;
            }
            InputEvent::PointerButton {
                button,
                state,
                position,
                modifiers,
            } => {
                self.pointer_position = Some(position);
                self.modifiers = modifiers;
                update_button(
                    button,
                    state,
                    &mut self.held_pointer_buttons,
                    &mut self.pressed_pointer_buttons,
                    &mut self.released_pointer_buttons,
                );
            }
            InputEvent::Scroll {
                delta,
                position,
                modifiers,
            } => {
                self.pointer_position = Some(position);
                self.modifiers = modifiers;
                self.scroll.push(ScrollSample { delta, position });
            }
            _ => {}
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.release_all();
        }
    }

    pub(crate) fn release_all(&mut self) {
        self.released_keys.append(&mut self.held_keys);
        self.released_pointer_buttons
            .append(&mut self.held_pointer_buttons);
        self.modifiers = Modifiers::default();
    }

    pub(crate) fn end_frame(&mut self) {
        self.pressed_keys.clear();
        self.released_keys.clear();
        self.pressed_pointer_buttons.clear();
        self.released_pointer_buttons.clear();
        self.scroll.clear();
    }
}

fn update_button<T: Copy + PartialEq>(
    button: T,
    state: ButtonState,
    held: &mut Vec<T>,
    pressed: &mut Vec<T>,
    released: &mut Vec<T>,
) {
    match state {
        ButtonState::Pressed if !held.contains(&button) => {
            held.push(button);
            pressed.push(button);
        }
        ButtonState::Released => {
            if let Some(index) = held.iter().position(|candidate| *candidate == button) {
                held.swap_remove(index);
                released.push(button);
            }
        }
        ButtonState::Pressed => {}
    }
}

#[cfg(test)]
mod tests {
    use mulciber_platform::{
        ButtonState, InputEvent, KeyCode, LogicalPosition, Modifiers, PointerButton, ScrollDelta,
    };

    use super::InputSnapshot;

    fn key(key: KeyCode, state: ButtonState, repeat: bool) -> InputEvent {
        InputEvent::Keyboard {
            key,
            state,
            repeat,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn key_transitions_last_one_frame_while_held_state_persists() {
        let mut input = InputSnapshot::default();
        input.handle_event(key(KeyCode::KeyW, ButtonState::Pressed, false));
        assert!(input.key_pressed(KeyCode::KeyW));
        assert!(input.key_held(KeyCode::KeyW));

        input.end_frame();
        assert!(!input.key_pressed(KeyCode::KeyW));
        assert!(input.key_held(KeyCode::KeyW));

        input.handle_event(key(KeyCode::KeyW, ButtonState::Released, false));
        assert!(input.key_released(KeyCode::KeyW));
        assert!(!input.key_held(KeyCode::KeyW));
    }

    #[test]
    fn repeated_key_events_do_not_create_new_press_transitions() {
        let mut input = InputSnapshot::default();
        input.handle_event(key(KeyCode::KeyW, ButtonState::Pressed, false));
        input.end_frame();
        input.handle_event(key(KeyCode::KeyW, ButtonState::Pressed, true));
        assert!(!input.key_pressed(KeyCode::KeyW));
        assert!(input.key_held(KeyCode::KeyW));
    }

    #[test]
    fn focus_loss_releases_every_held_control() {
        let mut input = InputSnapshot::default();
        input.handle_event(key(KeyCode::KeyW, ButtonState::Pressed, false));
        input.handle_event(InputEvent::PointerButton {
            button: PointerButton::Primary,
            state: ButtonState::Pressed,
            position: LogicalPosition::new(4.0, 7.0),
            modifiers: Modifiers::default(),
        });
        input.end_frame();

        input.handle_event(InputEvent::FocusChanged { focused: false });
        assert!(!input.focused());
        assert!(!input.key_held(KeyCode::KeyW));
        assert!(input.key_released(KeyCode::KeyW));
        assert!(!input.pointer_button_held(PointerButton::Primary));
        assert!(input.pointer_button_released(PointerButton::Primary));
    }

    #[test]
    fn scroll_preserves_units_order_and_positions() {
        let mut input = InputSnapshot::default();
        let position = LogicalPosition::new(4.0, 7.0);
        input.handle_event(InputEvent::Scroll {
            delta: ScrollDelta::Precise { x: 1.5, y: -2.0 },
            position,
            modifiers: Modifiers::default(),
        });
        assert_eq!(input.scroll().len(), 1);
        assert_eq!(input.scroll()[0].position(), position);
        assert_eq!(
            input.scroll()[0].delta(),
            ScrollDelta::Precise { x: 1.5, y: -2.0 }
        );
    }
}
