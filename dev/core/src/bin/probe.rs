//! Reports what the stack can see: Bluetooth controllers and audio endpoints.
//!
//! Read-only. Enumerates devices and, for each controller, tries to claim the
//! HCI interface - a controller still owned by the Microsoft stack refuses,
//! which is exactly what we want to confirm before anything gets rebound.

use olea_core::audio;
use olea_core::UsbTransport;

fn main() {
    println!("OpenLEAudio probe");
    println!("{}", "=".repeat(64));

    report_controllers();
    report_audio();
}

fn report_controllers() {
    println!("\nBLUETOOTH CONTROLLERS");
    println!("{}", "-".repeat(64));

    let controllers = match UsbTransport::list_controllers() {
        Ok(list) => list,
        Err(e) => {
            println!("  could not enumerate USB devices: {e}");
            return;
        }
    };

    if controllers.is_empty() {
        println!("  none found on the USB bus");
        return;
    }

    for (info, summary) in &controllers {
        println!(
            "  {:04x}:{:04x}  {}",
            summary.vendor_id,
            summary.product_id,
            summary.product.as_deref().unwrap_or("(no product string)")
        );

        match UsbTransport::open(info) {
            Ok(_) => println!("            available - bound to WinUSB, stack can drive it"),
            Err(e) => println!("            {e}"),
        }
    }
}

fn report_audio() {
    println!("\nAUDIO CAPTURE ENDPOINTS");
    println!("{}", "-".repeat(64));

    let devices = match audio::list_capture_devices() {
        Ok(devices) => devices,
        Err(e) => {
            println!("  could not enumerate audio devices: {e}");
            return;
        }
    };

    if devices.is_empty() {
        println!("  none active");
        return;
    }

    for device in &devices {
        let marker = if device.is_virtual_cable() { " <- virtual cable" } else { "" };
        println!("  {}{}", device.name, marker);

        // The cable is the one that matters, so report what rate it runs at.
        if device.is_virtual_cable() {
            report_cable_format(device);
        }
    }

    if !devices.iter().any(|d| d.is_virtual_cable()) {
        println!("\n  No virtual cable found. Install VB-Audio Cable and set");
        println!("  'CABLE Input' as the default Windows output device.");
    }
}

/// Probes which sample rates the cable will accept, since the codec has to match.
fn report_cable_format(device: &audio::AudioDevice) {
    // LC3 rates worth checking, most useful first.
    const RATES: &[u32] = &[48_000, 44_100, 32_000, 24_000, 16_000];

    for &rate in RATES {
        match audio::AudioCapture::open(&device.id, rate) {
            Ok(capture) => {
                println!("            runs at {} Hz - matches LC3 directly", capture.sample_rate());
                return;
            }
            Err(audio::AudioError::FormatMismatch { actual, channels, .. }) => {
                println!(
                    "            runs at {actual} Hz, {channels} channel(s)"
                );
                if actual != 48_000 {
                    println!("            set it to 48000 Hz in Sound Control Panel -> Advanced,");
                    println!("            or the stack has to resample and add latency");
                }
                return;
            }
            Err(_) => continue,
        }
    }

    println!("            could not open the device to check its format");
}
