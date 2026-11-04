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
    pointer_delta: [f64; 2],
    held_keys: Vec<KeyCode>,
    pressed_keys: Vec<KeyCode>,
    released_keys: Vec<KeyCode>,
    held_pointer_buttons: Vec<PointerButton>,
    pressed_pointer_buttons: Vec<PointerButton>,
    released_pointer_buttons: Vec<PointerButton>,
    scroll: Vec<ScrollSample>,
    typed_text: String,
}

impl Default for InputSnapshot {
    fn default() -> Self {
        Self {
            focused: true,
            modifiers: Modifiers::default(),
            pointer_position: None,
            pointer_delta: [0.0; 2],
            held_keys: Vec::new(),
            pressed_keys: Vec::new(),
            released_keys: Vec::new(),
            held_pointer_buttons: Vec::new(),
            pressed_pointer_buttons: Vec::new(),
            released_pointer_buttons: Vec::new(),
            scroll: Vec::new(),
            typed_text: String::new(),
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

    /// Returns how far a captured pointer moved since the last rendered frame: the sum of its
    /// [`InputEvent::PointerDelta`] motion, in logical units with positive y down, for a camera
    /// that looks or orbits each frame. Unlike transitions and scroll, it is not latched through
    /// render-only frames: every rendered frame consumes it, so a frame never sees motion an earlier
    /// one already applied. Zero while the pointer isn't captured, which reports
    /// [`Self::pointer_position`] instead.
    #[must_use]
    pub const fn pointer_delta(&self) -> [f64; 2] {
        self.pointer_delta
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

    /// Returns the text typed since transient input was last consumed, in order: each
    /// [`InputEvent::Character`] the platform delivered, for chat, consoles and names.
    #[must_use]
    pub fn typed_text(&self) -> &str {
        &self.typed_text
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
            InputEvent::PointerDelta {
                delta_x,
                delta_y,
                modifiers,
            } => {
                self.pointer_delta[0] += delta_x;
                self.pointer_delta[1] += delta_y;
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
            InputEvent::Character(character) => self.typed_text.push(character),
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

    /// Ends a rendered frame, whether or not it ran fixed updates: what it consumed whatever it
    /// ran, the pointer's motion.
    pub(crate) fn end_render_frame(&mut self) {
        self.pointer_delta = [0.0; 2];
    }

    pub(crate) fn end_frame(&mut self) {
        self.pressed_keys.clear();
        self.released_keys.clear();
        self.pressed_pointer_buttons.clear();
        self.released_pointer_buttons.clear();
        self.scroll.clear();
        self.typed_text.clear();
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
    fn typed_text_keeps_order_for_one_frame() {
        let mut input = InputSnapshot::default();
        for character in ['h', '\u{e9}', ' ', '\u{1f600}'] {
            input.handle_event(InputEvent::Character(character));
        }
        assert_eq!(input.typed_text(), "h\u{e9} \u{1f600}");
        input.end_frame();
        assert_eq!(input.typed_text(), "");
    }

    fn pointer_delta(delta_x: f64, delta_y: f64) -> InputEvent {
        InputEvent::PointerDelta {
            delta_x,
            delta_y,
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn pointer_delta_sums_a_frames_motion_and_each_rendered_frame_consumes_it() {
        let mut input = InputSnapshot::default();
        assert_eq!(input.pointer_delta(), [0.0, 0.0]);
        input.handle_event(pointer_delta(3.0, -1.5));
        input.handle_event(pointer_delta(0.5, 4.0));
        assert_eq!(input.pointer_delta(), [3.5, 2.5]);
        assert_eq!(
            input.pointer_position(),
            None,
            "capture reports no position"
        );

        input.end_render_frame();
        assert_eq!(
            input.pointer_delta(),
            [0.0, 0.0],
            "a render-only frame consumes it too"
        );
        input.handle_event(pointer_delta(-2.0, 1.0));
        input.end_render_frame();
        input.end_frame();
        assert_eq!(input.pointer_delta(), [0.0, 0.0]);
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
