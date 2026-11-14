#![allow(dead_code)]
pub mod driver;

use unikey::{Engine, InputMethod};

/// A text field: what an application shows after applying the engine's output
/// the way the Wayland frontend does (delete `backs` chars, type `text`;
/// pass the key through when the engine did nothing).
pub struct Editor {
    pub engine: Engine,
    pub text: String,
}

impl Editor {
    pub fn new(im: InputMethod) -> Self {
        let mut engine = Engine::new();
        engine.set_input_method(im);
        Editor { engine, text: String::new() }
    }

    pub fn key(&mut self, c: char) {
        self.engine.set_caps_state(c.is_ascii_uppercase(), false);
        let out = self.engine.process(c as u32);
        if out.handled {
            for _ in 0..out.backs {
                self.text.pop();
            }
            self.text.push_str(&out.text);
        } else {
            self.text.push(c);
        }
    }

    pub fn backspace(&mut self) {
        let out = self.engine.process_backspace();
        if out.handled {
            for _ in 0..out.backs {
                self.text.pop();
            }
            self.text.push_str(&out.text);
        } else {
            self.text.pop();
        }
    }

    pub fn type_str(&mut self, s: &str) -> &str {
        for c in s.chars() {
            self.key(c);
        }
        &self.text
    }
}

pub fn typed(im: InputMethod, keys: &str) -> String {
    let mut e = Editor::new(im);
    e.type_str(keys).to_owned()
}
