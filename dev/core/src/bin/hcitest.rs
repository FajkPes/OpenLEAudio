//! Low-level HCI probe: sends a few commands and reports exactly what came back.
//!
//! A capture of the Windows driver initialising this adapter settled the
//! question this tool used to be aimed at. The controller answers Read Local
//! Version within two milliseconds of enumerating, with no firmware download
//! anywhere in the sequence - so silence from our own stack is a transport
//! problem, not a mute chip.
//!
//! What the same capture also showed is that the command control transfer was
//! not addressed the way this stack addressed it. That is what this tool now
//! measures: it tries each addressing in turn and reports which one the
//! hardware answers, rather than leaving it to be argued about.
//!
//! Read-only in the sense that matters: Reset and Read Local Version change no
//! stored state, and no vendor-specific commands are sent.

use std::time::Duration;

use olea_core::hci;
use olea_core::transport::{CommandStyle, UsbTransport};

fn main() {
    println!("HCI test - co adapter odpovi");
    println!("{}", "=".repeat(66));

    // Before anything else: does the control pipe work at all? A device
    // descriptor read is answered by the USB silicon itself, with no Bluetooth
    // firmware involved. If even this is silent, the problem is our plumbing.
    println!("\n[A] cteni USB deskriptoru (odpovida USB cast cipu, ne firmware)");
    match probe_descriptor() {
        Ok(text) => println!("    OK: {text}"),
        Err(e) => println!("    SELHALO: {e}"),
    }

    // A device left in selective suspend answers descriptor reads but ignores
    // everything else. A USB reset brings it back to a known state.
    println!("\n[A2] USB reset zarizeni");
    match reset_device() {
        Ok(()) => println!("    OK"),
        Err(e) => println!("    SELHALO: {e}"),
    }
    std::thread::sleep(Duration::from_millis(500));

    let mut transport = match UsbTransport::open_first() {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("\nnelze otevrit adapter: {e}");
            std::process::exit(1);
        }
    };

    // One reader thread for the whole run, so a silent controller cannot hang
    // the test and the two phases below do not compete for the same pipe.
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = transport.clone();
    std::thread::spawn(move || loop {
        match reader.read_event() {
            Ok(raw) => {
                if sender.send(raw).is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = sender.send(format!("CHYBA: {e}").into_bytes());
                break;
            }
        }
    });

    println!("\n[B] hledam spravne adresovani HCI prikazu\n");

    match find_command_style(&mut transport, &receiver) {
        Some(style) => {
            println!("\n  WORKING: {}", style.label());
            transport.set_command_style(style);
        }
        None => {
            println!("\n  Zadne adresovani nefunguje - adapter neodpovida vubec.");
        }
    }

    println!("\n[C] posilam HCI prikazy\n");

    let commands: &[(&str, Vec<u8>)] = &[
        ("HCI Reset", hci::reset()),
        ("Read Local Version", hci::read_local_version()),
        ("Read BD_ADDR", hci::read_bd_addr()),
    ];

    for (name, packet) in commands {
        println!("--> {name}");
        println!("    odeslano: {}", hex(packet));

        if let Err(e) = transport.send_command(packet) {
            println!("    ODESLANI SELHALO: {e}\n");
            continue;
        }

        match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(raw) => {
                println!("    prislo   : {}", hex(&raw));
                describe(&raw);
            }
            Err(_) => println!("    TICHO (3 s bez odpovedi)"),
        }
        println!();
    }

    println!("{}", "=".repeat(66));
    println!("How to read this:");
    println!("  odpoved na Reset  -> cip komunikuje, stack je v poradku");
    println!("  ticho na vsechno  -> problem je v USB komunikaci");
}

/// Tries each way of addressing the command control transfer, and returns the
/// first one the controller answers.
///
/// Read Local Version is the probe: it changes nothing, and a controller that
/// answers it at all is working. Each attempt drains stale replies first, so a
/// late answer to an earlier attempt cannot be credited to the wrong style.
fn find_command_style(
    transport: &mut UsbTransport,
    receiver: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Option<CommandStyle> {
    for style in CommandStyle::ALL {
        while receiver.try_recv().is_ok() {}

        transport.set_command_style(style);
        print!("  {:<38}", style.label());

        if let Err(e) = transport.send_command(&hci::read_local_version()) {
            println!("odeslani selhalo: {e}");
            continue;
        }

        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(raw) => {
                println!("ODPOVEDEL");
                describe(&raw);
                return Some(style);
            }
            Err(_) => println!("ticho"),
        }
    }

    None
}

/// Resets the device, clearing any suspended or half-configured state.
fn reset_device() -> Result<(), String> {
    let info = nusb::list_devices()
        .map_err(|e| e.to_string())?
        .find(|d| d.class() == 0xE0 || d.interfaces().any(|i| i.class() == 0xE0))
        .ok_or("zadny Bluetooth adapter")?;

    let device = info.open().map_err(|e| e.to_string())?;
    device.reset().map_err(|e| e.to_string())
}

/// Reads the device descriptor over the control pipe.
///
/// This is served by the USB controller inside the chip and needs no Bluetooth
/// firmware, so it separates "our USB code is wrong" from "the chip is mute".
fn probe_descriptor() -> Result<String, String> {
    let info = nusb::list_devices()
        .map_err(|e| e.to_string())?
        .find(|d| d.class() == 0xE0 || d.interfaces().any(|i| i.class() == 0xE0))
        .ok_or("zadny Bluetooth adapter")?;

    let device = info.open().map_err(|e| e.to_string())?;
    let config = device.active_configuration().map_err(|e| e.to_string())?;

    Ok(format!(
        "konfigurace {}, {} interface(u)",
        config.configuration_value(),
        config.interfaces().count()
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(32)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn describe(raw: &[u8]) {
    let Some(event) = hci::Event::parse(raw) else {
        println!("    (cannot parse as an HCI event)");
        return;
    };

    match event.code {
        0x0E => {
            if let Some((opcode, params)) = event.command_complete() {
                let status = params.first().copied().unwrap_or(0xFF);
                println!(
                    "    Command Complete pro {opcode:#06x}, status {status:#04x} ({})",
                    if status == 0 { "uspech" } else { "chyba" }
                );

                if opcode == hci::op::READ_LOCAL_VERSION {
                    if let Some(version) = hci::LocalVersion::parse(params) {
                        println!(
                            "    Bluetooth {}, LMP {}, vyrobce {:#06x}",
                            version.bluetooth_version(),
                            version.lmp_version,
                            version.manufacturer
                        );
                    }
                }
            }
        }
        0x0F => println!("    Command Status"),
        other => println!("    event {other:#04x}"),
    }
}

