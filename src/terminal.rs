use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type InteractiveTerminal = TerminalGuard<Box<dyn Write>>;

pub struct TerminalGuard<W: Write> {
    terminal: Terminal<CrosstermBackend<W>>,
    raw_enabled: bool,
    restored: bool,
}

impl InteractiveTerminal {
    pub fn open() -> io::Result<Self> {
        TerminalGuard::new(terminal_writer(), true)
    }
}

impl<W: Write> TerminalGuard<W> {
    pub fn new(writer: W, raw_enabled: bool) -> io::Result<Self> {
        if raw_enabled {
            enable_raw_mode()?;
        }
        let mut terminal = match Terminal::new(CrosstermBackend::new(writer)) {
            Ok(terminal) => terminal,
            Err(error) => {
                if raw_enabled {
                    let _ = disable_raw_mode();
                }
                return Err(error);
            }
        };
        if let Err(error) = execute!(terminal.backend_mut(), EnterAlternateScreen, Hide) {
            if raw_enabled {
                let _ = disable_raw_mode();
            }
            return Err(error);
        }
        Ok(Self {
            terminal,
            raw_enabled,
            restored: false,
        })
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<W>> {
        &mut self.terminal
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let mut first_error = None;
        if let Err(error) = self.terminal.show_cursor() {
            first_error = Some(error);
        }
        if let Err(error) = execute!(self.terminal.backend_mut(), LeaveAlternateScreen, Show) {
            first_error.get_or_insert(error);
        }
        if self.raw_enabled
            && let Err(error) = disable_raw_mode()
        {
            first_error.get_or_insert(error);
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            self.restored = true;
            Ok(())
        }
    }
}

impl<W: Write> Drop for TerminalGuard<W> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

type PanicHook = dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static;

pub struct PanicHookGuard {
    previous: Option<Box<PanicHook>>,
}

impl PanicHookGuard {
    pub fn install() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|information| {
            emergency_restore();
            eprintln!("{information}");
        }));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take()
            && !std::thread::panicking()
        {
            std::panic::set_hook(previous);
        }
    }
}

pub fn emergency_restore() {
    let _ = disable_raw_mode();
    let mut writer = terminal_writer();
    let _ = execute!(writer, LeaveAlternateScreen, Show);
}

pub fn write_selection(mut writer: impl Write, selection: Option<&Path>) -> io::Result<()> {
    let Some(path) = selection else {
        return Ok(());
    };
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "selected worktree path is not absolute",
        ));
    }
    write_path(&mut writer, path)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn terminal_writer() -> Box<dyn Write> {
    #[cfg(unix)]
    if let Ok(terminal) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        return Box::new(terminal);
    }
    Box::new(io::stderr())
}

#[cfg(unix)]
fn write_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    writer.write_all(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn write_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    writer.write_all(path.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn guard_restores_alternate_screen_and_cursor_on_drop() {
        let writer = SharedWriter::default();
        let captured = writer.0.clone();
        {
            let _guard = TerminalGuard::new(writer, false).unwrap();
        }
        let output = captured.lock().unwrap().clone();
        assert!(output.windows(8).any(|window| window == b"\x1b[?1049h"));
        assert!(output.windows(8).any(|window| window == b"\x1b[?1049l"));
        assert!(output.windows(6).any(|window| window == b"\x1b[?25h"));
    }

    #[test]
    fn selection_protocol_is_exact_and_cancellation_is_empty() {
        let mut output = Vec::new();
        write_selection(&mut output, Some(Path::new("/tmp/a path"))).unwrap();
        assert_eq!(output, b"/tmp/a path\n");
        output.clear();
        write_selection(&mut output, None).unwrap();
        assert!(output.is_empty());
        assert!(write_selection(Vec::new(), Some(Path::new("relative"))).is_err());
    }

    #[test]
    fn guard_restores_during_unwind() {
        let writer = SharedWriter::default();
        let captured = writer.0.clone();
        let result = std::panic::catch_unwind(|| {
            let _guard = TerminalGuard::new(writer, false).unwrap();
            panic!("simulated panic");
        });
        assert!(result.is_err());
        let output = captured.lock().unwrap().clone();
        assert!(output.windows(8).any(|window| window == b"\x1b[?1049l"));
        assert!(output.windows(6).any(|window| window == b"\x1b[?25h"));
    }
}
