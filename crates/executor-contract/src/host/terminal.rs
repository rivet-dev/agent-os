#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalWindowSize {
    pub rows: u16,
    pub columns: u16,
    pub x_pixels: u16,
    pub y_pixels: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalAttributes {
    pub input_flags: u32,
    pub output_flags: u32,
    pub control_flags: u32,
    pub local_flags: u32,
    pub line_discipline: u8,
    pub control_characters: [u8; 32],
    pub input_speed: u32,
    pub output_speed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalOperation {
    IsTerminal {
        fd: u32,
    },
    GetAttributes {
        fd: u32,
    },
    SetAttributes {
        fd: u32,
        attributes: TerminalAttributes,
    },
    GetWindowSize {
        fd: u32,
    },
    SetWindowSize {
        fd: u32,
        size: TerminalWindowSize,
    },
    GetForegroundProcessGroup {
        fd: u32,
    },
    SetForegroundProcessGroup {
        fd: u32,
        pgid: u32,
    },
    GetSession {
        fd: u32,
    },
    SetRawMode {
        fd: u32,
        enabled: bool,
    },
    OpenPty,
}
