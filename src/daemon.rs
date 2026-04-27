use anyhow::{Context, Result, bail};
use std::ffi::CString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::RawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::thread;
use std::time::Duration;

const SOCKET_NAME: &str = "xhisperflow_socket";
const TYPESTRING_LIMIT: usize = 4095;
const FLAG_UPPERCASE: i32 = 0x8000_0000u32 as i32;
const BUS_USB: u16 = 0x03;
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0;
const UI_DEV_CREATE: libc::c_ulong = 21_761;
const UI_DEV_DESTROY: libc::c_ulong = 21_762;
const UI_DEV_SETUP: libc::c_ulong = 1_079_792_899;
const UI_SET_EVBIT: libc::c_ulong = 1_074_025_828;
const UI_SET_KEYBIT: libc::c_ulong = 1_074_025_829;
const KEY_1: u16 = 2;
const KEY_2: u16 = 3;
const KEY_3: u16 = 4;
const KEY_4: u16 = 5;
const KEY_5: u16 = 6;
const KEY_6: u16 = 7;
const KEY_7: u16 = 8;
const KEY_8: u16 = 9;
const KEY_9: u16 = 10;
const KEY_0: u16 = 11;
const KEY_MINUS: u16 = 12;
const KEY_EQUAL: u16 = 13;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_Q: u16 = 16;
const KEY_W: u16 = 17;
const KEY_E: u16 = 18;
const KEY_R: u16 = 19;
const KEY_T: u16 = 20;
const KEY_Y: u16 = 21;
const KEY_U: u16 = 22;
const KEY_I: u16 = 23;
const KEY_O: u16 = 24;
const KEY_P: u16 = 25;
const KEY_LEFTBRACE: u16 = 26;
const KEY_RIGHTBRACE: u16 = 27;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_A: u16 = 30;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_F: u16 = 33;
const KEY_G: u16 = 34;
const KEY_H: u16 = 35;
const KEY_J: u16 = 36;
const KEY_K: u16 = 37;
const KEY_L: u16 = 38;
const KEY_SEMICOLON: u16 = 39;
const KEY_APOSTROPHE: u16 = 40;
const KEY_GRAVE: u16 = 41;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_BACKSLASH: u16 = 43;
const KEY_Z: u16 = 44;
const KEY_X: u16 = 45;
const KEY_C: u16 = 46;
const KEY_V: u16 = 47;
const KEY_B: u16 = 48;
const KEY_N: u16 = 49;
const KEY_M: u16 = 50;
const KEY_COMMA: u16 = 51;
const KEY_DOT: u16 = 52;
const KEY_SLASH: u16 = 53;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_SPACE: u16 = 57;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WrapKey {
    LeftAlt,
    RightAlt,
    LeftCtrl,
    RightCtrl,
    LeftShift,
    RightShift,
    Super,
}

impl WrapKey {
    pub fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "leftalt" => Some(Self::LeftAlt),
            "rightalt" => Some(Self::RightAlt),
            "leftctrl" => Some(Self::LeftCtrl),
            "rightctrl" => Some(Self::RightCtrl),
            "leftshift" => Some(Self::LeftShift),
            "rightshift" => Some(Self::RightShift),
            "super" => Some(Self::Super),
            _ => None,
        }
    }

    fn socket_byte(self) -> u8 {
        match self {
            Self::RightAlt => b'r',
            Self::LeftAlt => b'L',
            Self::LeftCtrl => b'C',
            Self::RightCtrl => b'R',
            Self::LeftShift => b'S',
            Self::RightShift => b'T',
            Self::Super => b'M',
        }
    }

    pub fn cli_name(self) -> &'static str {
        match self {
            Self::LeftAlt => "leftalt",
            Self::RightAlt => "rightalt",
            Self::LeftCtrl => "leftctrl",
            Self::RightCtrl => "rightctrl",
            Self::LeftShift => "leftshift",
            Self::RightShift => "rightshift",
            Self::Super => "super",
        }
    }
}

pub enum ClientCommand<'a> {
    Paste,
    Type(u8),
    TypeString(&'a str),
    Backspace,
    Key(WrapKey),
}

pub fn send_command(command: ClientCommand<'_>) -> Result<()> {
    let socket = connect_socket()?;

    let payload: Vec<u8> = match command {
        ClientCommand::Paste => vec![b'p'],
        ClientCommand::Backspace => vec![b'b'],
        ClientCommand::Key(key) => vec![key.socket_byte()],
        ClientCommand::Type(byte) => vec![b't', byte],
        ClientCommand::TypeString(text) => {
            if text.is_empty() {
                bail!("'typestring' requires a non-empty string argument");
            }
            if text.len() > TYPESTRING_LIMIT {
                bail!("'typestring' supports strings up to {TYPESTRING_LIMIT} bytes");
            }
            let mut data = Vec::with_capacity(text.len() + 1);
            data.push(b's');
            data.extend_from_slice(text.as_bytes());
            data
        }
    };

    socket
        .send(&payload)
        .context("failed to send command to xhisperflowtoold")?;
    Ok(())
}

pub fn is_running() -> bool {
    connect_socket().is_ok()
}

pub fn run_daemon() -> Result<()> {
    let mut device = UinputDevice::create().context("failed to create uinput device")?;
    let socket = bind_socket().context("failed to bind xhisperflow socket")?;
    eprintln!("xhisperflowtoold: listening on @{}", SOCKET_NAME);

    let mut buf = [0_u8; 8192];
    loop {
        let size = socket
            .recv(&mut buf)
            .context("failed to receive daemon command")?;
        if size == 0 {
            continue;
        }

        match buf[0] {
            b'p' => device.do_paste()?,
            b't' if size == 2 => device.type_char(buf[1])?,
            b's' if size > 1 => device.type_string(&buf[1..size])?,
            b'b' => device.do_backspace()?,
            b'r' => device.do_key(KEY_RIGHTALT)?,
            b'L' => device.do_key(KEY_LEFTALT)?,
            b'C' => device.do_key(KEY_LEFTCTRL)?,
            b'R' => device.do_key(KEY_RIGHTCTRL)?,
            b'S' => device.do_key(KEY_LEFTSHIFT)?,
            b'T' => device.do_key(KEY_RIGHTSHIFT)?,
            b'M' => device.do_key(KEY_LEFTMETA)?,
            _ => {}
        }
    }
}

fn connect_socket() -> Result<UnixDatagram> {
    let socket = UnixDatagram::unbound().context("failed to create datagram socket")?;
    let addr = SocketAddr::from_abstract_name(SOCKET_NAME.as_bytes())
        .context("failed to build abstract socket address")?;
    socket
        .connect_addr(&addr)
        .context("failed to connect to xhisperflowtoold")?;
    Ok(socket)
}

fn bind_socket() -> io::Result<UnixDatagram> {
    let addr = SocketAddr::from_abstract_name(SOCKET_NAME.as_bytes())?;
    UnixDatagram::bind_addr(&addr)
}

struct UinputDevice {
    fd: RawFd,
}

impl UinputDevice {
    fn create() -> Result<Self> {
        let path = CString::new("/dev/uinput").unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("failed to open /dev/uinput");
        }

        let device = Self { fd };
        if let Err(err) = device.initialize() {
            let _ = unsafe { libc::close(fd) };
            return Err(err);
        }

        Ok(device)
    }

    fn initialize(&self) -> Result<()> {
        self.ioctl_int(UI_SET_EVBIT, i32::from(EV_KEY))?;

        for key in supported_keys() {
            self.ioctl_int(UI_SET_KEYBIT, i32::from(*key))?;
        }

        let mut setup: libc::uinput_setup = unsafe { zeroed() };
        setup.id.bustype = BUS_USB;
        setup.id.vendor = 0x1234;
        setup.id.product = 0x5678;

        let name = "xhisperflow";
        for (idx, byte) in name.bytes().enumerate() {
            setup.name[idx] = byte as i8;
        }

        self.ioctl_ptr(UI_DEV_SETUP, &setup)?;
        self.ioctl_none(UI_DEV_CREATE)?;
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    fn emit(&self, event_type: u16, code: u16, value: i32) -> Result<()> {
        let mut event: libc::input_event = unsafe { zeroed() };
        event.type_ = event_type;
        event.code = code;
        event.value = value;
        let written = unsafe {
            libc::write(
                self.fd,
                &event as *const libc::input_event as *const libc::c_void,
                size_of::<libc::input_event>(),
            )
        };
        if written < 0 {
            return Err(io::Error::last_os_error()).context("failed to write input event");
        }
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }

    fn do_paste(&mut self) -> Result<()> {
        self.emit(EV_KEY, KEY_LEFTCTRL, 1)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(8_000));
        self.emit(EV_KEY, KEY_V, 1)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(8_000));
        self.emit(EV_KEY, KEY_V, 0)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(2_000));
        self.emit(EV_KEY, KEY_LEFTCTRL, 0)?;
        self.sync()?;
        Ok(())
    }

    fn type_char(&mut self, byte: u8) -> Result<()> {
        let mapped = ascii_to_keycode(byte);
        if mapped < 0 {
            return Ok(());
        }

        let keycode = (mapped & 0xffff) as u16;
        let needs_shift = (mapped & FLAG_UPPERCASE) != 0;

        if needs_shift {
            self.emit(EV_KEY, KEY_LEFTSHIFT, 1)?;
            self.sync()?;
            thread::sleep(Duration::from_micros(2_000));
        }

        self.emit(EV_KEY, keycode, 1)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(8_000));
        self.emit(EV_KEY, keycode, 0)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(2_000));

        if needs_shift {
            self.emit(EV_KEY, KEY_LEFTSHIFT, 0)?;
            self.sync()?;
        }

        Ok(())
    }

    fn type_string(&mut self, text: &[u8]) -> Result<()> {
        for &byte in text {
            self.type_char(byte)?;
        }
        Ok(())
    }

    fn do_backspace(&mut self) -> Result<()> {
        self.emit(EV_KEY, KEY_BACKSPACE, 1)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(8_000));
        self.emit(EV_KEY, KEY_BACKSPACE, 0)?;
        self.sync()?;
        Ok(())
    }

    fn do_key(&mut self, keycode: u16) -> Result<()> {
        self.emit(EV_KEY, keycode, 1)?;
        self.sync()?;
        thread::sleep(Duration::from_micros(8_000));
        self.emit(EV_KEY, keycode, 0)?;
        self.sync()?;
        Ok(())
    }

    fn ioctl_none(&self, request: libc::c_ulong) -> Result<()> {
        let rc = unsafe { libc::ioctl(self.fd, request) };
        if rc < 0 {
            return Err(io::Error::last_os_error()).context("ioctl failed");
        }
        Ok(())
    }

    fn ioctl_int(&self, request: libc::c_ulong, value: i32) -> Result<()> {
        let rc = unsafe { libc::ioctl(self.fd, request, value) };
        if rc < 0 {
            return Err(io::Error::last_os_error()).context("ioctl failed");
        }
        Ok(())
    }

    fn ioctl_ptr<T>(&self, request: libc::c_ulong, value: &T) -> Result<()> {
        let rc = unsafe { libc::ioctl(self.fd, request, value) };
        if rc < 0 {
            return Err(io::Error::last_os_error()).context("ioctl failed");
        }
        Ok(())
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        let _ = unsafe { libc::ioctl(self.fd, UI_DEV_DESTROY) };
        let _ = unsafe { libc::close(self.fd) };
    }
}

fn supported_keys() -> &'static [u16] {
    &[
        KEY_Q,
        KEY_W,
        KEY_E,
        KEY_R,
        KEY_T,
        KEY_Y,
        KEY_U,
        KEY_I,
        KEY_O,
        KEY_P,
        KEY_A,
        KEY_S,
        KEY_D,
        KEY_F,
        KEY_G,
        KEY_H,
        KEY_J,
        KEY_K,
        KEY_L,
        KEY_Z,
        KEY_X,
        KEY_C,
        KEY_V,
        KEY_B,
        KEY_N,
        KEY_M,
        KEY_0,
        KEY_1,
        KEY_2,
        KEY_3,
        KEY_4,
        KEY_5,
        KEY_6,
        KEY_7,
        KEY_8,
        KEY_9,
        KEY_SPACE,
        KEY_MINUS,
        KEY_EQUAL,
        KEY_LEFTBRACE,
        KEY_RIGHTBRACE,
        KEY_SEMICOLON,
        KEY_APOSTROPHE,
        KEY_GRAVE,
        KEY_BACKSLASH,
        KEY_COMMA,
        KEY_DOT,
        KEY_SLASH,
        KEY_TAB,
        KEY_ENTER,
        KEY_BACKSPACE,
        KEY_LEFTCTRL,
        KEY_RIGHTCTRL,
        KEY_LEFTALT,
        KEY_RIGHTALT,
        KEY_LEFTSHIFT,
        KEY_RIGHTSHIFT,
        KEY_LEFTMETA,
    ]
}

fn ascii_to_keycode(byte: u8) -> i32 {
    match byte {
        b'\t' => i32::from(KEY_TAB),
        b'\n' => i32::from(KEY_ENTER),
        b' ' => i32::from(KEY_SPACE),
        b'!' => i32::from(KEY_1) | FLAG_UPPERCASE,
        b'"' => i32::from(KEY_APOSTROPHE) | FLAG_UPPERCASE,
        b'#' => i32::from(KEY_3) | FLAG_UPPERCASE,
        b'$' => i32::from(KEY_4) | FLAG_UPPERCASE,
        b'%' => i32::from(KEY_5) | FLAG_UPPERCASE,
        b'&' => i32::from(KEY_7) | FLAG_UPPERCASE,
        b'\'' => i32::from(KEY_APOSTROPHE),
        b'(' => i32::from(KEY_9) | FLAG_UPPERCASE,
        b')' => i32::from(KEY_0) | FLAG_UPPERCASE,
        b'*' => i32::from(KEY_8) | FLAG_UPPERCASE,
        b'+' => i32::from(KEY_EQUAL) | FLAG_UPPERCASE,
        b',' => i32::from(KEY_COMMA),
        b'-' => i32::from(KEY_MINUS),
        b'.' => i32::from(KEY_DOT),
        b'/' => i32::from(KEY_SLASH),
        b'0' => i32::from(KEY_0),
        b'1' => i32::from(KEY_1),
        b'2' => i32::from(KEY_2),
        b'3' => i32::from(KEY_3),
        b'4' => i32::from(KEY_4),
        b'5' => i32::from(KEY_5),
        b'6' => i32::from(KEY_6),
        b'7' => i32::from(KEY_7),
        b'8' => i32::from(KEY_8),
        b'9' => i32::from(KEY_9),
        b':' => i32::from(KEY_SEMICOLON) | FLAG_UPPERCASE,
        b';' => i32::from(KEY_SEMICOLON),
        b'<' => i32::from(KEY_COMMA) | FLAG_UPPERCASE,
        b'=' => i32::from(KEY_EQUAL),
        b'>' => i32::from(KEY_DOT) | FLAG_UPPERCASE,
        b'?' => i32::from(KEY_SLASH) | FLAG_UPPERCASE,
        b'@' => i32::from(KEY_2) | FLAG_UPPERCASE,
        b'A' => i32::from(KEY_A) | FLAG_UPPERCASE,
        b'B' => i32::from(KEY_B) | FLAG_UPPERCASE,
        b'C' => i32::from(KEY_C) | FLAG_UPPERCASE,
        b'D' => i32::from(KEY_D) | FLAG_UPPERCASE,
        b'E' => i32::from(KEY_E) | FLAG_UPPERCASE,
        b'F' => i32::from(KEY_F) | FLAG_UPPERCASE,
        b'G' => i32::from(KEY_G) | FLAG_UPPERCASE,
        b'H' => i32::from(KEY_H) | FLAG_UPPERCASE,
        b'I' => i32::from(KEY_I) | FLAG_UPPERCASE,
        b'J' => i32::from(KEY_J) | FLAG_UPPERCASE,
        b'K' => i32::from(KEY_K) | FLAG_UPPERCASE,
        b'L' => i32::from(KEY_L) | FLAG_UPPERCASE,
        b'M' => i32::from(KEY_M) | FLAG_UPPERCASE,
        b'N' => i32::from(KEY_N) | FLAG_UPPERCASE,
        b'O' => i32::from(KEY_O) | FLAG_UPPERCASE,
        b'P' => i32::from(KEY_P) | FLAG_UPPERCASE,
        b'Q' => i32::from(KEY_Q) | FLAG_UPPERCASE,
        b'R' => i32::from(KEY_R) | FLAG_UPPERCASE,
        b'S' => i32::from(KEY_S) | FLAG_UPPERCASE,
        b'T' => i32::from(KEY_T) | FLAG_UPPERCASE,
        b'U' => i32::from(KEY_U) | FLAG_UPPERCASE,
        b'V' => i32::from(KEY_V) | FLAG_UPPERCASE,
        b'W' => i32::from(KEY_W) | FLAG_UPPERCASE,
        b'X' => i32::from(KEY_X) | FLAG_UPPERCASE,
        b'Y' => i32::from(KEY_Y) | FLAG_UPPERCASE,
        b'Z' => i32::from(KEY_Z) | FLAG_UPPERCASE,
        b'[' => i32::from(KEY_LEFTBRACE),
        b'\\' => i32::from(KEY_BACKSLASH),
        b']' => i32::from(KEY_RIGHTBRACE),
        b'^' => i32::from(KEY_6) | FLAG_UPPERCASE,
        b'_' => i32::from(KEY_MINUS) | FLAG_UPPERCASE,
        b'`' => i32::from(KEY_GRAVE),
        b'a' => i32::from(KEY_A),
        b'b' => i32::from(KEY_B),
        b'c' => i32::from(KEY_C),
        b'd' => i32::from(KEY_D),
        b'e' => i32::from(KEY_E),
        b'f' => i32::from(KEY_F),
        b'g' => i32::from(KEY_G),
        b'h' => i32::from(KEY_H),
        b'i' => i32::from(KEY_I),
        b'j' => i32::from(KEY_J),
        b'k' => i32::from(KEY_K),
        b'l' => i32::from(KEY_L),
        b'm' => i32::from(KEY_M),
        b'n' => i32::from(KEY_N),
        b'o' => i32::from(KEY_O),
        b'p' => i32::from(KEY_P),
        b'q' => i32::from(KEY_Q),
        b'r' => i32::from(KEY_R),
        b's' => i32::from(KEY_S),
        b't' => i32::from(KEY_T),
        b'u' => i32::from(KEY_U),
        b'v' => i32::from(KEY_V),
        b'w' => i32::from(KEY_W),
        b'x' => i32::from(KEY_X),
        b'y' => i32::from(KEY_Y),
        b'z' => i32::from(KEY_Z),
        b'{' => i32::from(KEY_LEFTBRACE) | FLAG_UPPERCASE,
        b'|' => i32::from(KEY_BACKSLASH) | FLAG_UPPERCASE,
        b'}' => i32::from(KEY_RIGHTBRACE) | FLAG_UPPERCASE,
        b'~' => i32::from(KEY_GRAVE) | FLAG_UPPERCASE,
        _ => -1,
    }
}
