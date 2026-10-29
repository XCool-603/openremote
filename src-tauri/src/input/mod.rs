use crate::protocol::{KeyAction, KeyEvent, MouseAction, MouseButton, MouseEvent};
use anyhow::Result;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::sync::Mutex;

/// InputSimulator wraps Enigo to execute remote mouse and keyboard events
pub struct InputSimulator {
    enigo: Mutex<Enigo>,
}

impl InputSimulator {
    pub fn new() -> Result<Self> {
        let settings = Settings::default();
        let enigo = Enigo::new(&settings)
            .map_err(|e| anyhow::anyhow!("Failed to initialize Enigo: {:?}", e))?;
        Ok(Self {
            enigo: Mutex::new(enigo),
        })
    }

    /// Move mouse cursor to (x, y) coordinates
    pub fn move_mouse(&self, x: i32, y: i32) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| anyhow::anyhow!("Failed to move mouse: {:?}", e))?;
        Ok(())
    }

    /// Click mouse button
    pub fn click(&self, button: MouseButton) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        let enigo_btn = map_mouse_button(button);
        enigo
            .button(enigo_btn, Direction::Click)
            .map_err(|e| anyhow::anyhow!("Failed to click mouse button: {:?}", e))?;
        Ok(())
    }

    /// Press mouse button down
    pub fn mouse_down(&self, button: MouseButton) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        let enigo_btn = map_mouse_button(button);
        enigo
            .button(enigo_btn, Direction::Press)
            .map_err(|e| anyhow::anyhow!("Failed to press mouse button: {:?}", e))?;
        Ok(())
    }

    /// Release mouse button up
    pub fn mouse_up(&self, button: MouseButton) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        let enigo_btn = map_mouse_button(button);
        enigo
            .button(enigo_btn, Direction::Release)
            .map_err(|e| anyhow::anyhow!("Failed to release mouse button: {:?}", e))?;
        Ok(())
    }

    /// Scroll mouse wheel
    pub fn scroll(&self, length: i32) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        enigo
            .scroll(length, enigo::Axis::Vertical)
            .map_err(|e| anyhow::anyhow!("Failed to scroll mouse: {:?}", e))?;
        Ok(())
    }

    /// Press key down
    pub fn key_press(&self, key_str: &str) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        let key = parse_key(key_str);
        enigo
            .key(key, Direction::Press)
            .map_err(|e| anyhow::anyhow!("Failed to press key: {:?}", e))?;
        Ok(())
    }

    /// Release key up
    pub fn key_release(&self, key_str: &str) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        let key = parse_key(key_str);
        enigo
            .key(key, Direction::Release)
            .map_err(|e| anyhow::anyhow!("Failed to release key: {:?}", e))?;
        Ok(())
    }

    /// Type string text directly
    pub fn type_text(&self, text: &str) -> Result<()> {
        let mut enigo = self.enigo.lock().unwrap();
        enigo
            .text(text)
            .map_err(|e| anyhow::anyhow!("Failed to type text: {:?}", e))?;
        Ok(())
    }

    /// Handle incoming protocol MouseEvent
    pub fn handle_mouse_event(&self, event: &MouseEvent) -> Result<()> {
        match event.action {
            MouseAction::Move => {
                self.move_mouse(event.x, event.y)?;
            }
            MouseAction::Down => {
                self.move_mouse(event.x, event.y)?;
                if let Some(btn) = event.button {
                    self.mouse_down(btn)?;
                }
            }
            MouseAction::Up => {
                self.move_mouse(event.x, event.y)?;
                if let Some(btn) = event.button {
                    self.mouse_up(btn)?;
                }
            }
            MouseAction::Click => {
                self.move_mouse(event.x, event.y)?;
                if let Some(btn) = event.button {
                    self.click(btn)?;
                }
            }
            MouseAction::DoubleClick => {
                self.move_mouse(event.x, event.y)?;
                if let Some(btn) = event.button {
                    self.click(btn)?;
                    self.click(btn)?;
                }
            }
            MouseAction::Scroll => {
                if let Some(dy) = event.delta_y {
                    self.scroll(dy)?;
                }
            }
        }
        Ok(())
    }

    /// Handle incoming protocol KeyEvent
    pub fn handle_key_event(&self, event: &KeyEvent) -> Result<()> {
        match event.action {
            KeyAction::Down => self.key_press(&event.key)?,
            KeyAction::Up => self.key_release(&event.key)?,
            KeyAction::Click => {
                self.key_press(&event.key)?;
                self.key_release(&event.key)?;
            }
        }
        Ok(())
    }
}

fn map_mouse_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Middle => Button::Middle,
        MouseButton::Right => Button::Right,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
        MouseButton::ScrollUp | MouseButton::ScrollDown => Button::Middle,
    }
}

fn parse_key(key: &str) -> Key {
    match key.to_lowercase().as_str() {
        "enter" | "return" => Key::Return,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "escape" | "esc" => Key::Escape,
        "shift" => Key::Shift,
        "control" | "ctrl" => Key::Control,
        "alt" => Key::Alt,
        "meta" | "win" | "command" | "cmd" => Key::Meta,
        "capslock" => Key::CapsLock,
        "up" | "arrowup" => Key::UpArrow,
        "down" | "arrowdown" => Key::DownArrow,
        "left" | "arrowleft" => Key::LeftArrow,
        "right" | "arrowright" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "delete" => Key::Delete,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        s if s.len() == 1 => {
            let ch = s.chars().next().unwrap();
            Key::Unicode(ch)
        }
        _ => Key::Unicode(key.chars().next().unwrap_or(' ')),
    }
}
