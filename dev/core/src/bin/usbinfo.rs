//! Dumps the USB descriptors of the adapter the stack has claimed.
//!
//! The endpoint numbers used by the transport are the ones the Bluetooth USB
//! transport specification fixes, but a controller that answers nothing is
//! exactly the case where that assumption deserves checking against reality.

use nusb::transfer::EndpointType;

fn main() {
    println!("USB deskriptory Bluetooth adapteru");
    println!("{}", "=".repeat(66));

    let devices = match nusb::list_devices() {
        Ok(devices) => devices,
        Err(e) => {
            eprintln!("nelze vypsat USB zarizeni: {e}");
            std::process::exit(1);
        }
    };

    let mut found = false;

    for info in devices {
        let is_bluetooth = info.class() == 0xE0
            || info.interfaces().any(|i| i.class() == 0xE0 && i.subclass() == 0x01);

        if !is_bluetooth {
            continue;
        }
        found = true;

        println!(
            "\n{:04x}:{:04x}  {}",
            info.vendor_id(),
            info.product_id(),
            info.product_string().unwrap_or("(bez jmena)")
        );

        let device = match info.open() {
            Ok(device) => device,
            Err(e) => {
                println!("  nelze otevrit: {e}");
                println!("  (zarizeni drzi jiny driver - prepni ho na WinUSB)");
                continue;
            }
        };

        let config = match device.active_configuration() {
            Ok(config) => config,
            Err(e) => {
                println!("  nelze precist konfiguraci: {e}");
                continue;
            }
        };

        println!("  konfigurace {}", config.configuration_value());

        for alt in config.interface_alt_settings() {
            let endpoints: Vec<_> = alt.endpoints().collect();
            if endpoints.is_empty() && alt.alternate_setting() != 0 {
                continue;
            }

            println!(
                "\n  interface {} / alt {}  (class {:#04x}, sub {:#04x}, proto {:#04x})",
                alt.interface_number(),
                alt.alternate_setting(),
                alt.class(),
                alt.subclass(),
                alt.protocol()
            );

            if endpoints.is_empty() {
                println!("    bez endpointu");
            }

            for endpoint in endpoints {
                let address = endpoint.address();
                let direction = if address & 0x80 != 0 { "IN " } else { "OUT" };

                let kind = match endpoint.transfer_type() {
                    EndpointType::Control => "control",
                    EndpointType::Isochronous => "ISOCHRONOUS",
                    EndpointType::Bulk => "bulk",
                    EndpointType::Interrupt => "interrupt",
                };

                println!(
                    "    {address:#04x}  {direction}  {kind:<12} max {} B",
                    endpoint.max_packet_size()
                );
            }
        }

        println!("\n  What the OpenLEAudio transport expects:");
        println!("    0x81 IN   interrupt   HCI eventy");
        println!("    0x82 IN   bulk        ACL data");
        println!("    0x02 OUT  bulk        ACL data");
        println!("    0x03 OUT  bulk        ISO audio");
    }

    if !found {
        println!("\nZadny Bluetooth adapter nenalezen.");
    }

    // The audio path reaches the isochronous endpoints through WinUSB directly,
    // which needs a device interface path rather than a USB address. Checking it
    // here is read-only: nothing is opened for writing and no alternate setting
    // is selected.
    println!("\n{}", "=".repeat(66));
    println!("Isochronous audio path (read-only)");

    // Held open on purpose: during playback nusb owns interface 0 the whole
    // time, and the isochronous path has to work alongside it. Checking without
    // that contention would prove nothing.
    let held = olea_core::transport::UsbTransport::open_first();
    println!(
        "  nusb drzi adapter: {}",
        if held.is_ok() { "yes (as during playback)" } else { "ne" }
    );

    match olea_core::winusb::find_interface_path(olea_core::winusb::OLEA_INTERFACE_GUID) {
        Ok(path) => {
            println!("  nalezeno: {path}");
            describe_iso_interface(&path);
        }
        Err(e) => {
            println!("  NENALEZENO: {e}");
            println!("  (the adapter must be switched to the OpenLEAudio WinUSB driver)");
        }
    }
}

/// Lists what each alternate setting of the isochronous interface offers.
fn describe_iso_interface(path: &str) {
    use olea_core::winusb::WinUsbInterface;

    let owner = match WinUsbInterface::open(path, 0) {
        Ok(owner) => owner,
        Err(e) => {
            println!("  nelze otevrit interface 0: {e}");
            return;
        }
    };

    let interface = match owner.associated(0) {
        Ok(interface) => interface,
        Err(e) => {
            println!("  nelze dosahnout na interface 1: {e}");
            return;
        }
    };

    println!("  interface {} otevren", interface.interface_number());

    for setting in 0..8u8 {
        let pipes = interface.pipes(setting);
        if pipes.is_empty() {
            continue;
        }

        let summary: Vec<String> = pipes
            .iter()
            .map(|p| format!("{:#04x} {:?} {} B", p.id, p.pipe_type, p.max_packet_size))
            .collect();

        println!("    alt {setting}: {}", summary.join(", "));
    }
}

