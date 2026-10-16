use crate::{SINK, bytes};
use std::fmt::{self, Write};

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

/// Query the host before constructing an expensive message. Unattached images
/// have no output; native plugins never install their own subscriber or writer.
#[allow(unsafe_code)]
pub fn log_enabled(level: Level) -> bool {
    SINK.get()
        .is_some_and(|sink| unsafe { (sink.log_enabled)(level as u32) })
}

/// Emit a bounded message through the host's process-lifetime C callbacks.
#[allow(unsafe_code)]
pub fn log(level: Level, target: &str, message: fmt::Arguments<'_>) {
    let Some(sink) = SINK.get() else {
        return;
    };
    if target.len() > 256 || !unsafe { (sink.log_enabled)(level as u32) } {
        return;
    }
    let mut message_buffer = Message(String::new());
    let _ = message_buffer.write_fmt(message);
    unsafe {
        (sink.log)(level as u32, bytes(target), bytes(&message_buffer.0));
    }
}

struct Message(String);
impl Write for Message {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut length = text.len().min(8192 - self.0.len());
        while !text.is_char_boundary(length) {
            length -= 1;
        }
        self.0.push_str(&text[..length]);
        if length < text.len() {
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}

#[macro_export]
macro_rules! log {
    ($level:expr, $($message:tt)*) => {{
        let level = $level;
        if $crate::log_enabled(level) {
            $crate::log(level, module_path!(), format_args!($($message)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_bound_formatting_without_splitting_unicode() {
        let mut message = Message(String::new());
        assert!(write!(message, "{}suffix", "界".repeat(3000)).is_err());
        assert_eq!(message.0.len(), 8190);
        assert!(message.0.chars().all(|c| c == '界'));
    }
}
