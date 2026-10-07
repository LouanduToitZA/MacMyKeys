mod config;
mod machine;

use crate::config::{chord, socket_path};
use crate::machine::{Effect, Machine, KEY_BACKSPACE, KEY_CAPSLOCK};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, Key};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const VIRTUAL_NAME: &str = "MacMyKeys";

fn main() -> ExitCode {
    let mut check = false;
    let mut config_arg = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--config" => {
                config_arg = Some(match args.next() {
                    Some(path) => path,
                    None => {
                        eprintln!("macmykeys: --config needs a path");
                        return ExitCode::from(2);
                    }
                });
            }
            "--help" | "-h" => {
                println!("Usage: macmykeys [--check] [--config PATH]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("macmykeys: unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }

    let config_path = config::config_path(config_arg.as_deref());
    let config = match config::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("macmykeys: {err}");
            return ExitCode::from(1);
        }
    };
    let device_path = match apple_keyboard() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("macmykeys: {err}");
            return ExitCode::from(1);
        }
    };

    if check {
        return match check_devices(&device_path) {
            Ok(()) => {
                println!("macmykeys: keyboard and virtual keyboard are usable");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("macmykeys: {err}");
                ExitCode::from(1)
            }
        };
    }

    let mut physical = match Device::open(&device_path) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("macmykeys: cannot open {}: {err}", device_path.display());
            eprintln!("macmykeys: add this user to the input group, then log in again");
            return ExitCode::from(1);
        }
    };
    let mut virtual_keyboard = match build_virtual(&physical) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("macmykeys: cannot create a virtual keyboard: {err}");
            eprintln!("macmykeys: /dev/uinput must be in the input group");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = physical.grab() {
        eprintln!("macmykeys: cannot grab {}: {err}", device_path.display());
        return ExitCode::from(1);
    }
    if let Err(err) = set_nonblocking(physical.as_raw_fd()) {
        eprintln!("macmykeys: {err}");
        let _ = physical.ungrab();
        return ExitCode::from(1);
    }

    let bus = Bus::listen(&socket_path());
    let mut machine = Machine::new(config.letters, config.hold_ms);
    let mut virt_down = HashSet::new();
    let started = Instant::now();
    eprintln!("macmykeys: holding {}", device_path.display());

    let exit = loop {
        let now = elapsed_ms(started);
        match dispatch(
            machine.tick(now),
            &mut virtual_keyboard,
            &machine,
            &bus,
            &mut virt_down,
        ) {
            Ok(true) => break None,
            Ok(false) => {}
            Err(err) => break Some(err),
        }

        let ready = match wait_fd(physical.as_raw_fd(), machine.ms_until(now)) {
            Ok(ready) => ready,
            Err(err) => break Some(err),
        };
        if !ready {
            continue;
        }
        let events = match physical.fetch_events() {
            Ok(events) => events.collect::<Vec<_>>(),
            Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
            Err(err) => break Some(err),
        };
        let mut stop = false;
        for event in events {
            if event.event_type() != EventType::KEY || event.value() == 2 {
                continue;
            }
            let now = elapsed_ms(started);
            let effects = if event.value() == 1 {
                machine.press(event.code(), now)
            } else if event.value() == 0 {
                machine.release(event.code())
            } else {
                Vec::new()
            };
            match dispatch(effects, &mut virtual_keyboard, &machine, &bus, &mut virt_down) {
                Ok(true) => {
                    stop = true;
                    break;
                }
                Ok(false) => {}
                Err(err) => {
                    eprintln!("macmykeys: {err}");
                    let _ = release_virtual(&mut virtual_keyboard, &mut virt_down);
                    let _ = physical.ungrab();
                    return ExitCode::from(1);
                }
            }
        }
        if stop {
            break None;
        }
    };

    let _ = release_virtual(&mut virtual_keyboard, &mut virt_down);
    let _ = physical.ungrab();
    let _ = bus.send(r#"{"op":"hide"}"#);
    if let Some(err) = exit {
        eprintln!("macmykeys: {err}");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn dispatch(
    effects: Vec<Effect>,
    device: &mut VirtualDevice,
    machine: &Machine,
    bus: &Bus,
    virt_down: &mut HashSet<u16>,
) -> std::io::Result<bool> {
    for effect in effects {
        match effect {
            Effect::Key { code, value } => emit_key(device, virt_down, code, value)?,
            Effect::Show { chars } => {
                bus.send(&json!({ "op": "show", "chars": chars }).to_string());
            }
            Effect::Hide => bus.send(r#"{"op":"hide"}"#),
            Effect::Replace { steps, upper } => {
                inject(device, virt_down, machine, &steps, upper)?;
            }
            Effect::Exit => return Ok(true),
        }
    }
    Ok(false)
}

fn inject(
    device: &mut VirtualDevice,
    virt_down: &mut HashSet<u16>,
    machine: &Machine,
    steps: &[String],
    upper: bool,
) -> std::io::Result<()> {
    let modifiers = machine.held_modifiers();
    for code in &modifiers {
        emit_key(device, virt_down, *code, 0)?;
    }
    tap(device, virt_down, &[KEY_BACKSPACE])?;
    tap(device, virt_down, &[KEY_CAPSLOCK])?;
    for step in steps {
        let keys = chord(step, upper).ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidData, format!("unknown compose step {step}"))
        })?;
        tap(device, virt_down, &keys)?;
    }
    for code in machine.held_modifiers() {
        emit_key(device, virt_down, code, 1)?;
    }
    Ok(())
}

fn tap(
    device: &mut VirtualDevice,
    virt_down: &mut HashSet<u16>,
    codes: &[u16],
) -> std::io::Result<()> {
    for code in codes {
        emit_key(device, virt_down, *code, 1)?;
    }
    for code in codes.iter().rev() {
        emit_key(device, virt_down, *code, 0)?;
    }
    thread::sleep(Duration::from_millis(12));
    Ok(())
}

fn emit_key(
    device: &mut VirtualDevice,
    virt_down: &mut HashSet<u16>,
    code: u16,
    value: i32,
) -> std::io::Result<()> {
    if value == 1 && virt_down.contains(&code) {
        return Ok(());
    }
    let event = InputEvent::new(EventType::KEY, code, value);
    device.emit(&[event])?;
    if value == 0 {
        virt_down.remove(&code);
    } else if value == 1 {
        virt_down.insert(code);
    }
    Ok(())
}

fn release_virtual(
    device: &mut VirtualDevice,
    virt_down: &mut HashSet<u16>,
) -> std::io::Result<()> {
    let held: Vec<u16> = virt_down.iter().copied().collect();
    for code in held {
        emit_key(device, virt_down, code, 0)?;
    }
    Ok(())
}

fn check_devices(path: &Path) -> Result<(), String> {
    let device = Device::open(path).map_err(|err| {
        format!(
            "cannot open {}: {err}. Add this user to the input group, then log in again",
            path.display()
        )
    })?;
    build_virtual(&device).map(|_| ()).map_err(|err| {
        format!("cannot create a virtual keyboard: {err}. /dev/uinput must be writable by the input group")
    })
}

fn build_virtual(physical: &Device) -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<Key>::new();
    if let Some(supported) = physical.supported_keys() {
        for key in supported.iter() {
            keys.insert(key);
        }
    }
    evdev::uinput::VirtualDeviceBuilder::new()?
        .name(VIRTUAL_NAME)
        .with_keys(&keys)?
        .build()
}

fn apple_keyboard() -> Result<PathBuf, String> {
    let text = fs::read_to_string("/proc/bus/input/devices").map_err(|err| err.to_string())?;
    let mut name = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("N: Name=") {
            name = rest.trim_matches('"').to_string();
        } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
            if name.contains("Apple Internal Keyboard") && !name.contains(VIRTUAL_NAME) {
                for token in rest.split_whitespace() {
                    if let Some(number) = token.strip_prefix("event") {
                        if !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()) {
                            return Ok(PathBuf::from(format!("/dev/input/event{number}")));
                        }
                    }
                }
            }
        }
    }
    Err("Apple internal keyboard not found".into())
}

fn set_nonblocking(fd: i32) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn wait_fd(fd: i32, timeout_ms: i32) -> std::io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err);
    }
    Ok(ready > 0)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

struct Bus {
    clients: Arc<Mutex<Vec<UnixStream>>>,
}

impl Bus {
    fn listen(path: &Path) -> Self {
        let _ = fs::remove_file(path);
        let listener = UnixListener::bind(path).unwrap_or_else(|err| {
            eprintln!("macmykeys: cannot listen on {}: {err}", path.display());
            std::process::exit(1);
        });
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let incoming = Arc::clone(&clients);
        thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => incoming.lock().unwrap().push(stream),
                    Err(err) => eprintln!("macmykeys: overlay connection failed: {err}"),
                }
            }
        });
        Self { clients }
    }

    fn send(&self, line: &str) {
        let mut payload = String::with_capacity(line.len() + 1);
        payload.push_str(line);
        payload.push('\n');
        let mut clients = self.clients.lock().unwrap();
        clients.retain_mut(|client| client.write_all(payload.as_bytes()).is_ok());
    }
}
