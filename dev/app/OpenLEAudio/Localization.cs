using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace OpenLEAudio;

/// <summary>Runtime language packs with stable keys and an English fallback.</summary>
public static class Loc
{
    private sealed record Pack(string Code, string Name, Dictionary<string, string> Strings);

    private static readonly Dictionary<string, string> En = new()
    {
        ["nav.setup"] = "Setup", ["nav.devices"] = "Devices", ["nav.settings"] = "Settings", ["nav.language"] = "Language", ["nav.about"] = "About",
        ["devices.title"] = "Bluetooth & devices", ["status.starting"] = "Starting…",
        ["status.unavailable"] = "Unavailable", ["status.core_failed"] = "The audio service could not start",
        ["status.core_unavailable"] = "The audio service is unavailable",
        ["status.turning_on"] = "Turning on…", ["status.turning_off"] = "Turning off…",
        ["status.scanning"] = "Searching for devices…", ["status.off"] = "Off",
        ["status.adapter_off"] = "The adapter is off", ["status.none_found"] = "Nothing new nearby",
        ["status.scan_failed"] = "Search failed", ["devices.bluetooth"] = "Bluetooth",
        ["status.found"] = "Found: {0}",
        ["devices.scan_again"] = "Search again", ["devices.paired"] = "Paired devices",
        ["devices.found"] = "Discovered devices", ["devices.unpair"] = "Unpair",
        ["common.on"] = "On", ["common.off"] = "Off", ["common.saved"] = "saved",
        ["common.success"] = "Done", ["common.error"] = "Something went wrong",
        ["device.connecting"] = "Connecting…", ["device.connected_playing"] = "Connected, playing",
        ["device.connected"] = "Connected", ["device.paired"] = "Paired", ["device.playing"] = "Playing",
        ["device.disconnect"] = "Disconnect", ["device.connect"] = "Connect", ["device.pair"] = "Pair",
        ["log.title"] = "Activity", ["log.debug_on"] = "Debug on", ["log.debug_off"] = "Debug off",
        ["log.follow_on"] = "Following new entries", ["log.follow_off"] = "Following paused",
        ["log.down"] = "↓ Bottom", ["log.copy"] = "Copy all", ["log.copied"] = "Copied",
        ["log.clear"] = "Clear", ["log.debug_tip"] = "Detailed HCI/ACL/ISO packets; turning debug off clears the detailed log",
        ["log.down_tip"] = "Jump to the end and follow new entries again",
        ["log.debug_disabled"] = "Debug is off. The console keeps at most the latest 500 lines.",
        ["log.debug_enabled"] = "Debug enabled: showing detailed HCI/ACL/ISO packets.",
        ["log.core_ready"] = "The audio core is running.", ["log.connected"] = "Connected.",
        ["log.disconnected"] = "Disconnected.", ["log.bluetooth_first"] = "Turn Bluetooth on first.",
        ["log.reconnect_wait"] = "Reconnecting: waiting three seconds before connecting again…",
        ["log.reconnecting"] = "Trying to restore the connection…",
        ["log.reconnect_stopped"] = "Automatic reconnection has stopped.",
        ["metrics.signal"] = "Signal: {0}", ["metrics.loss"] = "Lost packets (60 s): {0} ({1})",
        ["metrics.stable"] = "Connection: stable", ["metrics.unstable"] = "Connection: unstable",
        ["metrics.fair"] = "Connection: fair", ["metrics.waiting"] = "Connection: waiting for data",
        ["settings.title"] = "Settings", ["settings.subtitle"] = "Changes are saved automatically and survive a restart.",
        ["settings.reset"] = "Restore defaults", ["settings.reconnect"] = "Reconnect",
        ["settings.filter"] = "Show", ["settings.layout"] = "Layout",
        ["settings.filter.all"] = "All categories", ["settings.filter.audio"] = "Audio",
        ["settings.filter.connection"] = "Connection", ["settings.filter.application"] = "Application & diagnostics",
        ["settings.layout.columns"] = "Adaptive columns", ["settings.layout.list"] = "List",
        ["settings.layout.panels"] = "3 panels",
        ["settings.custom_on_edit"] = "Editing this value switches LC3 quality to Custom automatically.",
        ["settings.main_preset"] = "Main LC3 preset",
        ["settings.panel.quality"] = "Sound quality & LC3",
        ["settings.panel.quality.sub"] = "Codec detail, bitrate and radio transport",
        ["settings.panel.connection"] = "Connection & microphone",
        ["settings.panel.connection.sub"] = "Recovery, idle behaviour and headset input",
        ["settings.panel.application"] = "Playback & application",
        ["settings.panel.application.sub"] = "Channel mapping, output level and diagnostics",
        ["settings.connected_hint"] = "The headphones are connected. Stream settings take effect after reconnecting.",
        ["settings.saved_now"] = "“{0}” saved and applied immediately.",
        ["settings.saved_scope"] = "“{0}” saved - {1}.",
        ["settings.applied_now"] = "applied now",
        ["settings.reconnect_required"] = "reconnect required",
        ["settings.startup_error"] = "Windows startup could not be changed: {0}",
        ["scope.immediately"] = "applies immediately", ["scope.reconnect"] = "applies after reconnecting the headphones",
        ["scope.adapter"] = "applies after restarting the adapter",
        ["section.playback"] = "Playback", ["section.codec"] = "LC3 codec",
        ["section.radio"] = "Radio transport", ["section.microphone"] = "Microphone",
        ["section.connection"] = "Connection", ["section.application"] = "Application",
        ["section.language"] = "Language",
        ["section.tuning"] = "Diagnostics", ["section.other"] = "Other settings",
        ["section.playback.sub"] = "Overall profile, stereo mapping and output level",
        ["section.codec.sub"] = "Audio detail and data rate; editable in Custom mode",
        ["section.radio.sub"] = "Balance latency, range and resilience",
        ["section.microphone.sub"] = "Disabled by default to preserve music bandwidth",
        ["section.connection.sub"] = "Recovery and idle behaviour",
        ["section.application.sub"] = "Background operation and Windows startup",
        ["section.language.sub"] = "Built-in and custom interface translations",
        ["section.tuning.sub"] = "Advanced adapter and stream diagnostics",
        ["setting.preset"] = "LC3 quality", ["setting.audio_mode"] = "Audio mode",
        ["setting.swap_channels"] = "Swap left and right", ["setting.rate_hz"] = "Sample rate",
        ["setting.frame_ms"] = "Frame duration (ms)", ["setting.octets"] = "Octets per frame (bitrate)",
        ["setting.phy"] = "Radio mode", ["setting.retransmissions"] = "Retransmissions",
        ["setting.max_latency_ms"] = "Link latency ceiling (ms)",
        ["setting.presentation_delay_ms"] = "Presentation delay (ms)",
        ["setting.diagnostics"] = "ASE diagnostics", ["setting.device"] = "Preferred device",
        ["setting.gain"] = "Pre-codec volume", ["setting.idle_timeout_min"] = "Sleep after silence (minutes)",
        ["setting.reconnect_enabled"] = "Automatic reconnection",
        ["setting.reconnect_interval_s"] = "Retry interval (seconds)",
        ["setting.reconnect_window_min"] = "Keep trying for (minutes)",
        ["setting.startup_reconnect_enabled"] = "Connect after Windows startup",
        ["setting.command_style"] = "HCI command addressing", ["setting.microphone_mode"] = "Headset microphone",
        ["setting.playback_source"] = "Playback capture source",
        ["setting.microphone_quality"] = "Microphone quality", ["setting.microphone_target"] = "Microphone destination",
        ["setting.microphone_gain"] = "Headset microphone volume", ["setting.monitor_enabled"] = "Microphone monitoring",
        ["setting.monitor_source"] = "Monitored microphone", ["setting.monitor_mode"] = "Monitoring mode",
        ["setting.monitor_gain"] = "Monitoring volume",
        ["setting.run_in_background"] = "Run in background", ["setting.start_with_windows"] = "Start with Windows",
        ["setting.language"] = "Language",
        ["desc.preset"] = "Sample rate, frame duration and LC3 bitrate preset.",
        ["desc.audio_mode"] = "Stereo uses two channels; compatibility reproduces the first working layout; mono uses one.",
        ["desc.swap_channels"] = "Swap the channels if the headphones play the sides in reverse.",
        ["desc.rate_hz"] = "Sample rate used by the Custom preset (16–48 kHz).",
        ["desc.frame_ms"] = "Frame duration used by the Custom preset (7.5 or 10 ms).",
        ["desc.octets"] = "LC3 bytes per frame; this controls the bitrate.",
        ["desc.phy"] = "2M is faster and uses less airtime; 1M reaches farther and tolerates interference.",
        ["desc.retransmissions"] = "How many times the radio may repeat a packet.",
        ["desc.max_latency_ms"] = "Maximum transport latency allowed for the link.",
        ["desc.presentation_delay_ms"] = "How long the headphones buffer a received frame before playing it.",
        ["desc.diagnostics"] = "Read each stream state after the audio channels are established.",
        ["desc.device"] = "The headphones to reconnect automatically.",
        ["desc.gain"] = "Gain before LC3 encoding; 0 is silent, 1 is unchanged and 2 is boost.",
        ["desc.idle_timeout_min"] = "Stop transmitting after sustained silence; 0 disables this.",
        ["desc.reconnect_enabled"] = "Reconnect automatically after the headphones go out of range.",
        ["desc.reconnect_interval_s"] = "How often to retry the connection.",
        ["desc.reconnect_window_min"] = "How long to retry; 0 means no time limit.",
        ["desc.startup_reconnect_enabled"] = "After an automatic background launch, look for remembered headphones every five seconds for three minutes.",
        ["desc.command_style"] = "How HCI commands are addressed over USB.",
        ["desc.playback_source"] = "The Windows capture endpoint sent to the headphones as music.",
        ["desc.microphone_mode"] = "Receive audio from the headset microphone over its Source ASE.",
        ["desc.microphone_quality"] = "LC3 voice quality; higher rates use more of the shared radio budget.",
        ["desc.microphone_target"] = "VB-CABLE writes to CABLE Input, which recording apps receive from CABLE Output.",
        ["desc.microphone_gain"] = "Gain applied to the headset microphone before publishing it to Windows.",
        ["desc.monitor_enabled"] = "Open the selected microphone and send it to the headphones.",
        ["desc.monitor_source"] = "Any active Windows microphone; the headset source appears when its LE microphone is enabled.",
        ["desc.monitor_mode"] = "Mix monitoring with captured music, or hear the microphone instead of music.",
        ["desc.monitor_gain"] = "Gain applied only to the monitored signal.",
        ["desc.run_in_background"] = "Closing the window keeps audio running and hides the app in the notification area.",
        ["desc.start_with_windows"] = "Launch the client in the background after signing in.",
        ["desc.language"] = "The interface language. Custom JSON language packs can be imported below.",
        ["trade.preset"] = "A preset fills the values below; Custom unlocks them.",
        ["trade.octets"] = "More octets improve bass and detail but require a higher data rate.",
        ["trade.retransmissions"] = "More retries reduce dropouts but may increase latency.",
        ["trade.max_latency_ms"] = "Lower reacts faster; higher tolerates interference better.",
        ["trade.presentation_delay_ms"] = "A buffer for synchronized playback in both ears.",
        ["trade.gain"] = "1.00 is unchanged; 1.05–2.00 boosts audio and the limiter prevents overflow.",
        ["trade.reconnect_interval_s"] = "Short retries more often; long is gentler on the adapter.",
        ["trade.reconnect_window_min"] = "0 keeps trying without a time limit.",
        ["trade.startup_reconnect_enabled"] = "Enabled by default; it stops as soon as the headphones connect or the three-minute window ends.",
        ["trade.idle_timeout_min"] = "0 disables sleeping the stream during silence.",
        ["trade.microphone_mode"] = "Off is the quality-first default: all capacity remains available for stereo music.",
        ["trade.microphone_quality"] = "Balanced is recommended for calls without spending the maximum airtime.",
        ["trade.microphone_target"] = "A separate VB-CABLE A/B cable avoids feeding the microphone back into music capture.",
        ["trade.microphone_gain"] = "1.00 is unchanged; boost carefully to avoid clipping.",
        ["trade.playback_source"] = "CABLE Output is the default music capture; this is separate from the preferred Bluetooth headset.",
        ["trade.monitor_enabled"] = "Off by default: no PC microphone is opened or monitored.",
        ["trade.monitor_source"] = "Changing the source never changes the music capture source.",
        ["trade.monitor_mode"] = "Mix keeps music audible; Replace mutes captured music while monitoring.",
        ["trade.monitor_gain"] = "1.00 is unchanged; boost carefully to avoid feedback or clipping.",
        ["trade.run_in_background"] = "Closing the window hides the client beside the clock.",
        ["trade.start_with_windows"] = "Starts hidden; open it from the notification-area icon.",
        ["choice.windows"] = "Windows-like (recommended)", ["choice.high_quality"] = "Highest quality",
        ["choice.low_latency"] = "Low latency", ["choice.robust"] = "Resilient connection",
        ["choice.custom"] = "Custom", ["choice.stereo"] = "Stereo (recommended)",
        ["choice.legacy"] = "Compatibility mode", ["choice.mono"] = "Mono",
        ["choice.mic_off"] = "Off - maximum playback quality", ["choice.mic_on"] = "On - receive headset microphone",
        ["choice.mic_voice"] = "Voice - 16 kHz", ["choice.mic_balanced"] = "Balanced - 32 kHz",
        ["choice.mic_high"] = "High - up to 48 kHz", ["choice.mic_vb_cable"] = "VB-CABLE Output for apps",
        ["choice.mic_vb_cable_a"] = "VB-CABLE A Output for apps", ["choice.mic_vb_cable_b"] = "VB-CABLE B Output for apps",
        ["choice.mic_no_target"] = "No virtual-cable output",
        ["choice.monitor_mix"] = "Mix with captured audio", ["choice.monitor_replace"] = "Replace captured audio",
        ["choice.class_device"] = "Bluetooth specification", ["choice.windows_command"] = "Windows-like",
        ["choice.class_interface"] = "Compatibility interface mode",
        ["slider.economical"] = "economical", ["slider.detail"] = "more detail",
        ["slider.faster"] = "faster", ["slider.resilient"] = "more resilient",
        ["slider.lower_latency"] = "lower latency", ["slider.more_headroom"] = "more headroom",
        ["slider.stable"] = "more stable", ["slider.silent"] = "0 · silent",
        ["slider.boost"] = "2 · boost", ["slider.default"] = "1.00 · default",
        ["slider.never"] = "never", ["slider.longer"] = "longer",
        ["slider.often"] = "more often", ["slider.gentle"] = "gentler",
        ["slider.unlimited"] = "unlimited", ["slider.limited"] = "limited time",
        ["language.english"] = "English", ["language.czech"] = "Čeština",
        ["language.title"] = "Language",
        ["language.subtitle"] = "Choose the interface language or manage custom translations.",
        ["language.import"] = "Import language…", ["language.export"] = "Export translation template…",
        ["language.json"] = "OpenLEAudio language pack", ["language.imported"] = "Language pack “{0}” imported.",
        ["language.import_error"] = "The language pack could not be imported: {0}",
        ["language.exported"] = "Translation template exported.",
        ["tray.open"] = "Open OpenLEAudio", ["tray.exit"] = "Exit",
        ["about.subtitle"] = "An open, configurable Bluetooth LE Audio client",
        ["about.how_title"] = "How it works",
        ["about.how_body"] = "The client captures stereo audio from the selected Windows output, splits it into left and right channels, encodes each with LC3 and sends timed ISO packets directly to the Bluetooth controller. GATT/ASCS signalling first discovers the headset capabilities, configures the codec and QoS, and only then starts streaming.",
        ["about.flow"] = "Windows audio → stereo capture → LC3 L/R → ISO SDU → CIS → headphones",
        ["about.ase_title"] = "ASE and CIS",
        ["about.ase_body1"] = "An ASE is an audio endpoint exposed by the headphones through ASCS. For stereo we normally configure two Sink ASEs: Front Left and Front Right. Both requests are sent atomically, like Windows, so both ears start in sync.",
        ["about.ase_body2"] = "Each active ASE maps to a CIS in one CIG. A CIS carries a separate LC3 stream for its ear. If a device supports stereo in one stream, the client can use one CIS with both channels; the plan always follows the discovered PACS/ASE capabilities.",
        ["about.windows_title"] = "Windows-like, but configurable",
        ["about.windows_body1"] = "The default preset follows captured Windows LE Audio behaviour: 48 kHz LC3, the same ASCS operation order, atomic setup of both ASEs and comparable QoS. This is an independent Bluetooth LE Audio implementation tuned against observed compatible behaviour, not a copy of Microsoft's driver.",
        ["about.windows_body2"] = "Unlike the system driver, LC3 bitrate, frame duration, PHY, retransmissions, latency and presentation delay are configurable. Windows-like is the safe default; Custom unlocks the individual values.",
        ["about.mapping_title"] = "Discovery, GATT and capability mapping",
        ["about.mapping_body1"] = "Discovery finds LE advertisements and known bonds. After encryption, the GATT client reads PACS codec records, audio locations and supported contexts, then discovers ASCS control points and ASE endpoints. OpenLEAudio maps those declared capabilities to a valid LC3 configuration instead of assuming that every headset supports every preset.",
        ["about.mapping_body2"] = "The mapping is explicit: Windows capture endpoint → PCM stereo → channel allocation → LC3 frames → ISO SDUs → one or two CIS links. Optional Source ASE audio can return the headset microphone to a separate virtual cable.",
        ["about.stack_title"] = "A user-mode stack with safety boundaries",
        ["about.stack_body1"] = "The dedicated adapter is bound per device to Microsoft's WinUSB driver. The OpenLEAudio user-mode stack owns HCI, ACL/L2CAP, ATT/GATT, SMP pairing, BAP signalling and ISO transport while other adapters can stay under Windows. No custom kernel code is loaded.",
        ["about.stack_body2"] = "Vendor-specific HCI commands are blocked, GATT writes are limited to handles found during discovery, packet lengths are validated and output passes through a limiter. The original adapter and VB-CABLE state can be restored from Setup.",
        ["about.credits_body"] = "Design, implementation, testing and debugging of OpenLEAudio.",
        ["setup.title"] = "Initial setup",
        ["setup.subtitle"] = "Prepare the driver, choose the Bluetooth adapter and configure the virtual audio cable.",
        ["setup.sign_title"] = "Sign the driver", ["setup.sign_button"] = "Sign driver",
        ["setup.sign_body"] = "Create a temporary local certificate and sign the WinUSB driver package before the first installation. Renew the signature every two years.",
        ["setup.sign_note"] = "The signing script removes the private signing key after installation. Secure Boot stays enabled because Windows loads Microsoft's WinUSB.sys.",
        ["setup.adapter_title"] = "Choose and verify the Bluetooth adapter",
        ["setup.adapter_body"] = "Select the dedicated USB adapter that OpenLEAudio may take over. Detection uses the hardware IDs from the signed driver package and works on both the Windows Bluetooth driver and WinUSB.",
        ["setup.detect_again"] = "Detect again", ["setup.detecting"] = "Detecting Bluetooth adapters…",
        ["setup.none_selected"] = "No adapter selected.", ["setup.none"] = "No present USB Bluetooth adapter was found.",
        ["setup.bind"] = "Switch to our stack", ["setup.restore"] = "Restore Windows driver", ["setup.status"] = "Driver status",
        ["setup.adapter_note"] = "LE is checked from the controller generation and Windows device data. Final LE Audio compatibility is confirmed safely after switching by reading the controller's LE feature set and the headset's GATT PACS/ASCS services; unsupported codec or channel configurations are rejected before streaming.",
        ["setup.vb_title"] = "Install and configure VB-CABLE",
        ["setup.vb_body"] = "Install VB-Audio Virtual Cable, then let OpenLEAudio set it to 48 kHz / 16-bit and use it as the Windows playback route. The original audio settings are backed up for restoration.",
        ["setup.vb_download"] = "VB-CABLE website", ["setup.vb_install"] = "Install VB-CABLE", ["setup.vb_setup"] = "Configure VB-CABLE",
        ["setup.vb_check"] = "Check VB-CABLE", ["setup.vb_restore"] = "Restore VB-CABLE",
        ["setup.stack_ours"] = "OpenLEAudio / WinUSB stack", ["setup.stack_windows"] = "Windows Bluetooth stack",
        ["setup.adapter_detail"] = "{0}\nHardware ID: {1}\nDriver: {2}\nCurrent binding: {3}\nDriver package: {4}",
        ["setup.supported"] = "supported; LE Audio controller features are verified after switching",
        ["setup.unsupported"] = "not listed in the signed INF; switching is disabled to protect this device",
        ["setup.detect_error"] = "Adapter detection failed: {0}", ["setup.files_missing"] = "The project setup scripts could not be found.",
        ["setup.choose_adapter"] = "Choose an adapter first.", ["setup.started"] = "The setup tool opened in a separate PowerShell window.",
        ["setup.restart_title"] = "Restart the app",
        ["setup.restart_body"] = "Close OpenLEAudio completely from the notification-area icon and start it again. The new process will open the selected WinUSB adapter and verify its Bluetooth 5.2+ LE Isochronous Channels support before scanning for LE Audio devices.",
    };

    private static readonly Dictionary<string, string> Cs = new()
    {
        ["nav.setup"]="Příprava", ["nav.devices"]="Zařízení", ["nav.settings"]="Nastavení", ["nav.language"]="Jazyk", ["nav.about"]="O aplikaci",
        ["devices.title"]="Bluetooth a zařízení", ["status.starting"]="Spouštím…", ["devices.bluetooth"]="Bluetooth",
        ["devices.scan_again"]="Hledat znovu", ["devices.paired"]="Spárovaná zařízení", ["devices.found"]="Nalezená zařízení",
        ["devices.unpair"]="Odpárovat", ["common.on"]="Zapnuto", ["common.off"]="Vypnuto",
        ["common.success"]="Hotovo", ["common.error"]="Něco se nepodařilo",
        ["log.title"]="Průběh", ["log.debug_on"]="Debug zapnutý", ["log.debug_off"]="Debug vypnutý",
        ["log.follow_on"]="Sledování zapnuto", ["log.follow_off"]="Sledování vypnuto", ["log.down"]="↓ Dolů",
        ["log.copy"]="Kopírovat vše", ["log.clear"]="Vymazat",
        ["log.debug_tip"]="Podrobné HCI/ACL/ISO pakety; vypnutí podrobný log vyčistí",
        ["log.down_tip"]="Skočit na konec a znovu sledovat nové záznamy",
        ["log.debug_disabled"]="Debug je vypnutý. Konzole uchovává nejvýše 500 posledních řádků.",
        ["log.debug_enabled"]="Debug zapnut: zobrazuji podrobné HCI/ACL/ISO pakety.",
        ["log.core_ready"]="Jádro běží.", ["log.connected"]="Připojeno.", ["log.disconnected"]="Odpojeno.",
        ["log.bluetooth_first"]="Nejdřív zapni Bluetooth.", ["log.reconnect_wait"]="Přepojuji: před novým připojením čekám tři sekundy…",
        ["log.reconnecting"]="Zkouším obnovit spojení…", ["log.reconnect_stopped"]="Automatické obnovení spojení skončilo.",
        ["settings.title"]="Nastavení", ["settings.subtitle"]="Hodnoty se ukládají samy a přežijí restart.",
        ["settings.reset"]="Obnovit výchozí nastavení", ["about.subtitle"]="Otevřený a konfigurovatelný klient pro Bluetooth LE Audio",
        ["settings.filter"]="Zobrazit", ["settings.layout"]="Rozložení",
        ["settings.filter.all"]="Všechny kategorie", ["settings.filter.audio"]="Zvuk",
        ["settings.filter.connection"]="Připojení", ["settings.filter.application"]="Aplikace a diagnostika",
        ["settings.layout.columns"]="Adaptivní sloupce", ["settings.layout.list"]="Seznam pod sebou",
        ["settings.layout.panels"]="3 panely",
        ["settings.custom_on_edit"]="Úprava této hodnoty automaticky přepne kvalitu LC3 na Vlastní.",
        ["settings.main_preset"]="Hlavní LC3 preset",
        ["settings.panel.quality"]="Kvalita zvuku a LC3",
        ["settings.panel.quality.sub"]="Kodek, bitrate a rádiový přenos",
        ["settings.panel.connection"]="Připojení a mikrofon",
        ["settings.panel.connection.sub"]="Obnova spojení, nečinnost a vstup sluchátek",
        ["settings.panel.application"]="Přehrávání a aplikace",
        ["settings.panel.application.sub"]="Kanály, výstupní úroveň a diagnostika",
        ["tray.open"]="Otevřít OpenLEAudio", ["tray.exit"]="Ukončit",
        ["about.how_title"]="Jak program funguje",
        ["about.how_body"]="Klient zachytí stereo zvuk z vybraného výstupu Windows, rozdělí jej na levý a pravý kanál, každý zakóduje pomocí LC3 a předá přímo Bluetooth řadiči jako časované ISO pakety. Signalizace přes GATT/ASCS nejdříve zjistí schopnosti sluchátek, nastaví kodek a QoS a teprve poté spustí stream.",
        ["about.mapping_title"]="Discovery, GATT a mapování schopností",
        ["about.mapping_body1"]="Discovery najde LE reklamy a známá spárování. Po zašifrování GATT klient načte z PACS záznamy kodeku, umístění zvuku a podporované kontexty a poté najde řídicí body ASCS a koncové body ASE. OpenLEAudio mapuje deklarované schopnosti na platnou konfiguraci LC3 a nepředpokládá, že každá sluchátka umí každý preset.",
        ["about.mapping_body2"]="Mapování je explicitní: záznamový endpoint Windows → PCM stereo → přiřazení kanálů → LC3 rámce → ISO SDU → jeden nebo dva CIS spoje. Volitelný zvuk Source ASE může vrátit mikrofon sluchátek do samostatného virtuálního kabelu.",
        ["about.stack_title"]="User-mode stack s bezpečnostními hranicemi",
        ["about.stack_body1"]="Vyhrazený adaptér je po jednotlivém zařízení napojen na WinUSB driver Microsoftu. User-mode stack OpenLEAudio obsluhuje HCI, ACL/L2CAP, ATT/GATT, SMP párování, BAP signalizaci a ISO přenos, zatímco jiné adaptéry mohou zůstat pod Windows. Nenačítá se žádný vlastní kód do kernelu.",
        ["about.stack_body2"]="Vendor-specific HCI příkazy jsou blokované, GATT zápisy jsou omezené na handly nalezené při discovery, délky paketů se ověřují a výstup prochází limiterem. Původní stav adaptéru i VB-CABLE lze obnovit v Přípravě.",
        ["about.flow"]="Zvuk Windows → stereo capture → LC3 L/R → ISO SDU → CIS → sluchátka",
        ["about.ase_title"]="ASE a CIS",
        ["about.ase_body1"]="ASE je koncový bod zvuku, který sluchátka vystaví přes ASCS. Pro stereo obvykle nakonfigurujeme dva Sink ASE: levý s umístěním Front Left a pravý s Front Right. Oba požadavky odesíláme společně, stejně jako Windows, aby se uši spustila synchronně.",
        ["about.ase_body2"]="Každý aktivní ASE se mapuje na CIS v jedné CIG. CIS nese samostatný tok LC3 pro příslušné ucho. Pokud zařízení podporuje stereo v jednom toku, klient umí zvolit i jediný CIS s oběma kanály; konfigurace se vždy přizpůsobí nalezeným PACS/ASE schopnostem.",
        ["about.windows_title"]="Podobně jako Windows, ale nastavitelně",
        ["about.windows_body1"]="Výchozí preset vychází z porovnání skutečné komunikace Windows LE Audio: 48 kHz LC3, shodné pořadí ASCS operací, atomické nastavení obou ASE a obdobné QoS. Nejde o kopii ovladače Microsoftu; jde o samostatnou implementaci standardů Bluetooth LE Audio naladěnou podle pozorovaného kompatibilního chování.",
        ["about.windows_body2"]="Na rozdíl od systémového ovladače lze zvolit bitrate LC3, délku rámce, PHY, počet opakování, latenci a prezentační zpoždění. Preset Jako Windows je bezpečný výchozí bod; Vlastní nastavení hodnoty odemkne.",
        ["about.credits_body"]="Návrh, implementace, testování a ladění OpenLEAudio.",
        ["status.unavailable"]="Nedostupné", ["status.core_failed"]="Jádro se nepodařilo spustit",
        ["status.core_unavailable"]="Jádro není dostupné", ["status.turning_on"]="Zapínám…",
        ["status.turning_off"]="Vypínám…", ["status.scanning"]="Hledám zařízení…", ["status.off"]="Vypnuto",
        ["status.adapter_off"]="Adaptér je vypnutý", ["status.none_found"]="Nic nového v okolí",
        ["status.scan_failed"]="Hledání selhalo", ["common.saved"]="uloženo",
        ["status.found"]="Nalezeno: {0}", ["settings.startup_error"]="Spuštění s Windows se nepodařilo nastavit: {0}",
        ["device.connecting"]="Připojuji…", ["device.connected_playing"]="Připojeno, hraje",
        ["device.connected"]="Připojeno", ["device.paired"]="Spárováno", ["device.playing"]="Hraje",
        ["device.disconnect"]="Odpojit", ["device.connect"]="Připojit", ["device.pair"]="Spárovat",
        ["log.copied"]="Zkopírováno", ["settings.reconnect"]="Znovu připojit",
        ["metrics.signal"]="Signál: {0}", ["metrics.loss"]="Ztracené pakety (60 s): {0} ({1})",
        ["metrics.stable"]="Spojení: stabilní", ["metrics.unstable"]="Spojení: nestabilní",
        ["metrics.fair"]="Spojení: přijatelné", ["metrics.waiting"]="Spojení: čekám na data",
        ["settings.connected_hint"]="Sluchátka jsou připojená. Nastavení streamu se projeví až po přepojení.",
        ["settings.saved_now"]="„{0}“ uloženo, projeví se hned.",
        ["settings.saved_scope"]="„{0}“ uloženo - {1}.",
        ["settings.applied_now"]="použito hned",
        ["settings.reconnect_required"]="nutné znovu připojit",
        ["scope.immediately"]="projeví se hned", ["scope.reconnect"]="projeví se po odpojení a připojení sluchátek",
        ["scope.adapter"]="projeví se po vypnutí a zapnutí adaptéru",
        ["section.playback"]="Přehrávání", ["section.codec"]="Kodek LC3", ["section.radio"]="Rádiový přenos",
        ["section.microphone"]="Mikrofon", ["section.connection"]="Připojení", ["section.application"]="Aplikace",
        ["section.language"]="Jazyk",
        ["section.tuning"]="Ladění", ["section.other"]="Další nastavení",
        ["section.playback.sub"]="Celkový profil, stereo a výstupní úroveň",
        ["section.codec.sub"]="Detail zvuku a datový tok; upravuje se v režimu Vlastní",
        ["section.radio.sub"]="Kompromis mezi odezvou, dosahem a odolností",
        ["section.microphone.sub"]="Ve výchozím stavu nebere kapacitu hudebnímu streamu",
        ["section.connection.sub"]="Obnovení spojení a chování při nečinnosti",
        ["section.application.sub"]="Jazyk, běh na pozadí a spuštění s Windows",
        ["section.language.sub"]="Vestavěné a vlastní překlady rozhraní",
        ["section.tuning.sub"]="Pokročilá diagnostika adaptéru a streamů",
        ["setting.preset"]="Kvalita LC3", ["setting.audio_mode"]="Režim zvuku",
        ["setting.swap_channels"]="Prohodit levý a pravý", ["setting.rate_hz"]="Vzorkovací frekvence",
        ["setting.frame_ms"]="Délka rámce (ms)", ["setting.octets"]="Oktety na rámec (bitrate)",
        ["setting.phy"]="Rádiový režim", ["setting.retransmissions"]="Počet opakování",
        ["setting.max_latency_ms"]="Strop latence linku (ms)", ["setting.presentation_delay_ms"]="Zpoždění přehrání (ms)",
        ["setting.diagnostics"]="Diagnostika ASE", ["setting.device"]="Preferované zařízení",
        ["setting.gain"]="Hlasitost před kodérem", ["setting.idle_timeout_min"]="Uspat po tichu (minuty)",
        ["setting.reconnect_enabled"]="Automatické připojení", ["setting.reconnect_interval_s"]="Interval pokusů (sekundy)",
        ["setting.reconnect_window_min"]="Zkoušet po dobu (minuty)", ["setting.command_style"]="Adresování HCI příkazů",
        ["setting.startup_reconnect_enabled"]="Připojit po spuštění Windows",
        ["setting.playback_source"]="Zdroj přehrávaného zvuku",
        ["setting.microphone_mode"]="Mikrofon sluchátek", ["setting.microphone_quality"]="Kvalita mikrofonu",
        ["setting.microphone_target"]="Výstup mikrofonu sluchátek", ["setting.microphone_gain"]="Hlasitost mikrofonu sluchátek",
        ["setting.monitor_enabled"]="Odposlech mikrofonu", ["setting.monitor_source"]="Odposlouchávaný mikrofon",
        ["setting.monitor_mode"]="Režim odposlechu", ["setting.monitor_gain"]="Hlasitost odposlechu",
        ["setting.run_in_background"]="Běžet na pozadí",
        ["setting.start_with_windows"]="Spouštět s Windows", ["setting.language"]="Jazyk",
        ["desc.preset"]="LC3 preset: vzorkovací frekvence, délka rámce a bitrate.",
        ["desc.audio_mode"]="Stereo používá dva kanály; kompatibilní režim původní funkční rozložení; mono jeden.",
        ["desc.swap_channels"]="Prohodí levý a pravý kanál, pokud sluchátka hrají strany obráceně.",
        ["desc.rate_hz"]="Vzorkovací frekvence pro preset Vlastní (16–48 kHz).",
        ["desc.frame_ms"]="Délka rámce pro preset Vlastní (7,5 nebo 10 ms).",
        ["desc.octets"]="Počet LC3 bajtů na rámec; určuje bitrate.",
        ["desc.phy"]="2M je rychlejší a šetří vysílací čas; 1M má delší dosah a lépe snáší rušení.",
        ["desc.retransmissions"]="Kolikrát smí rádio paket zopakovat.",
        ["desc.max_latency_ms"]="Maximální povolená přenosová latence linku.",
        ["desc.presentation_delay_ms"]="Jak dlouho sluchátka přijatý rámec podrží před přehráním.",
        ["desc.diagnostics"]="Po ustavení kanálů přečte stav každého streamu.",
        ["desc.device"]="Sluchátka, která se mají automaticky znovu připojit.",
        ["desc.gain"]="Zesílení před LC3 kodérem; 0 je ticho, 1 beze změny a 2 boost.",
        ["desc.idle_timeout_min"]="Po delším tichu přestane vysílat; 0 tuto funkci vypne.",
        ["desc.reconnect_enabled"]="Po ztrátě dosahu se automaticky připojí zpět.",
        ["desc.reconnect_interval_s"]="Jak často se připojení opakuje.",
        ["desc.reconnect_window_min"]="Jak dlouho se má zkoušet; 0 znamená bez omezení.",
        ["desc.startup_reconnect_enabled"]="Po automatickém spuštění na pozadí tři minuty každých pět sekund hledá zapamatovaná sluchátka.",
        ["desc.command_style"]="Způsob adresování HCI příkazů po USB.",
        ["desc.playback_source"]="Zvukový vstup Windows, který se posílá do sluchátek jako hudba.",
        ["desc.microphone_mode"]="Přijímá zvuk z mikrofonu sluchátek přes jeho Source ASE.",
        ["desc.microphone_quality"]="Kvalita hlasového LC3; vyšší frekvence spotřebuje více společné rádiové kapacity.",
        ["desc.microphone_target"]="VB-CABLE zapisuje do CABLE Input, odkud jej nahrávací aplikace dostanou jako CABLE Output.",
        ["desc.microphone_gain"]="Zesílení mikrofonu sluchátek před odesláním do Windows.",
        ["desc.monitor_enabled"]="Otevře vybraný mikrofon a pošle jej do sluchátek.",
        ["desc.monitor_source"]="Libovolný aktivní mikrofon Windows; mikrofon sluchátek se ukáže po zapnutí jeho LE vstupu.",
        ["desc.monitor_mode"]="Přimíchá odposlech k hudbě, nebo jím hudbu nahradí.",
        ["desc.monitor_gain"]="Zesílení použité pouze pro odposlech.",
        ["desc.run_in_background"]="Zavření okna nechá zvuk běžet a aplikaci schová k hodinám.",
        ["desc.start_with_windows"]="Po přihlášení spustí klienta skrytě na pozadí.",
        ["desc.language"]="Jazyk rozhraní; vlastní JSON překlady lze importovat níže.",
        ["trade.preset"]="Preset vyplní hodnoty níže; Vlastní je odemkne.",
        ["trade.octets"]="Více oktetů znamená lepší basy a detail, ale vyšší datový tok.",
        ["trade.retransmissions"]="Více opakování omezuje výpadky, může ale zvýšit odezvu.",
        ["trade.max_latency_ms"]="Nižší hodnota reaguje rychleji, vyšší lépe snáší rušení.",
        ["trade.presentation_delay_ms"]="Rezerva pro synchronní přehrání obou sluchátek.",
        ["trade.gain"]="1,00 je beze změny; 1,05–2,00 zesiluje a limiter zabrání přetečení.",
        ["trade.reconnect_interval_s"]="Krátce zkouší častěji; delší interval šetří adaptér.",
        ["trade.reconnect_window_min"]="0 znamená zkoušet bez časového omezení.",
        ["trade.startup_reconnect_enabled"]="Ve výchozím stavu zapnuto; skončí po připojení nebo po třech minutách.",
        ["trade.idle_timeout_min"]="0 vypne uspání streamu při tichu.",
        ["trade.microphone_mode"]="Vypnuto je výchozí: celá kapacita zůstane hudbě a stereo kvalitě.",
        ["trade.microphone_quality"]="Vyvážená je doporučená pro hovory bez maximální spotřeby vysílacího času.",
        ["trade.microphone_target"]="Samostatný VB-CABLE A/B zabrání návratu mikrofonu do stejného hudebního vstupu.",
        ["trade.microphone_gain"]="1,00 je beze změny; vyšší hodnoty mohou oříznout hlas.",
        ["trade.playback_source"]="CABLE Output je výchozí zdroj hudby a je oddělený od preferovaných Bluetooth sluchátek.",
        ["trade.monitor_enabled"]="Výchozí je vypnuto: aplikace neotevírá ani neodposlouchává žádný mikrofon z PC.",
        ["trade.monitor_source"]="Změna mikrofonu nikdy nezmění hlavní zdroj hudby.",
        ["trade.monitor_mode"]="Přimíchat ponechá hudbu; Nahradit ji během odposlechu ztlumí.",
        ["trade.monitor_gain"]="1,00 je beze změny; zesílení může způsobit vazbu nebo ořezání.",
        ["trade.run_in_background"]="Zavření okna aplikaci schová vedle hodin.",
        ["trade.start_with_windows"]="Spustí klienta skrytě; otevřete jej ikonou vedle hodin.",
        ["choice.windows"]="Jako Windows (doporučeno)", ["choice.high_quality"]="Nejvyšší kvalita",
        ["choice.low_latency"]="Nízká latence", ["choice.robust"]="Odolné spojení", ["choice.custom"]="Vlastní nastavení",
        ["choice.stereo"]="Stereo (doporučeno)", ["choice.legacy"]="Kompatibilní režim", ["choice.mono"]="Mono",
        ["choice.mic_off"]="Vypnutý - maximum kvality", ["choice.mic_on"]="Zapnutý - přijímat mikrofon sluchátek",
        ["choice.mic_voice"]="Hlas - 16 kHz", ["choice.mic_balanced"]="Vyvážená - 32 kHz",
        ["choice.mic_high"]="Vysoká - až 48 kHz", ["choice.mic_vb_cable"]="VB-CABLE Output pro aplikace",
        ["choice.mic_vb_cable_a"]="VB-CABLE A Output pro aplikace", ["choice.mic_vb_cable_b"]="VB-CABLE B Output pro aplikace",
        ["choice.mic_no_target"]="Bez výstupu do virtuálního kabelu", ["choice.class_device"]="Dle Bluetooth specifikace",
        ["choice.monitor_mix"]="Přimíchat k zachycenému zvuku", ["choice.monitor_replace"]="Nahradit zachycený zvuk",
        ["choice.windows_command"]="Jako Windows", ["choice.class_interface"]="Kompatibilní interface režim",
        ["slider.economical"]="úspornější", ["slider.detail"]="více detailu", ["slider.faster"]="rychlejší",
        ["slider.resilient"]="odolnější", ["slider.lower_latency"]="nižší odezva", ["slider.more_headroom"]="větší rezerva",
        ["slider.stable"]="stabilnější", ["slider.silent"]="0 · ticho", ["slider.boost"]="2 · boost",
        ["slider.default"]="1,00 · výchozí", ["slider.never"]="nevypínat", ["slider.longer"]="delší čas",
        ["slider.often"]="častěji", ["slider.gentle"]="šetrněji", ["slider.unlimited"]="bez omezení",
        ["slider.limited"]="omezená doba", ["language.english"]="English", ["language.czech"]="Čeština",
        ["language.title"]="Jazyk", ["language.subtitle"]="Vyberte jazyk rozhraní nebo spravujte vlastní překlady.",
        ["language.import"]="Importovat jazyk…", ["language.export"]="Exportovat šablonu překladu…",
        ["language.json"]="Jazykový balíček OpenLEAudio", ["language.imported"]="Jazykový balíček „{0}“ byl importován.",
        ["language.import_error"]="Jazykový balíček se nepodařilo importovat: {0}",
        ["language.exported"]="Šablona překladu byla exportována.",
        ["setup.title"]="První nastavení", ["setup.subtitle"]="Připravte ovladač, vyberte Bluetooth adaptér a nastavte virtuální zvukový kabel.",
        ["setup.sign_title"]="Podepsat driver", ["setup.sign_button"]="Podepsat driver",
        ["setup.sign_body"]="Před první instalací vytvoří dočasný lokální certifikát a podepíše balíček ovladače WinUSB. Podpis obnovte každé dva roky.",
        ["setup.sign_note"]="Po instalaci skript soukromý podpisový klíč odstraní. Secure Boot zůstává zapnutý, protože Windows načítá vlastní WinUSB.sys od Microsoftu.",
        ["setup.adapter_title"]="Vybrat a ověřit Bluetooth adaptér", ["setup.adapter_body"]="Vyberte vyhrazený USB adaptér, který smí OpenLEAudio převzít. Detekce používá Hardware ID z podepsaného balíčku driveru a funguje s Bluetooth driverem Windows i s WinUSB.",
        ["setup.detect_again"]="Zjistit znovu", ["setup.detecting"]="Zjišťuji Bluetooth adaptéry…", ["setup.none_selected"]="Není vybraný adaptér.",
        ["setup.none"]="Nebyl nalezen žádný přítomný USB Bluetooth adaptér.", ["setup.bind"]="Přepnout na náš stack",
        ["setup.restore"]="Vrátit Windows driver", ["setup.status"]="Stav driveru",
        ["setup.adapter_note"]="LE se předběžně ověří podle generace řadiče a dat Windows. Konečná kompatibilita LE Audio se bezpečně potvrdí po přepnutí načtením LE funkcí řadiče a služeb GATT PACS/ASCS sluchátek; nepodporovaná konfigurace kodeku nebo kanálů je odmítnuta ještě před streamem.",
        ["setup.vb_title"]="Nainstalovat a nastavit VB-CABLE", ["setup.vb_body"]="Nainstalujte VB-Audio Virtual Cable a nechte OpenLEAudio nastavit 48 kHz / 16 bitů a trasu přehrávání Windows. Původní nastavení zvuku se zazálohuje pro obnovení.",
        ["setup.vb_download"]="Web VB-CABLE", ["setup.vb_install"]="Nainstalovat VB-CABLE", ["setup.vb_setup"]="Nastavit VB-CABLE", ["setup.vb_check"]="Zkontrolovat VB-CABLE", ["setup.vb_restore"]="Vrátit VB-CABLE",
        ["setup.stack_ours"]="OpenLEAudio / WinUSB stack", ["setup.stack_windows"]="Bluetooth stack Windows",
        ["setup.adapter_detail"]="{0}\nHardware ID: {1}\nDriver: {2}\nAktuální napojení: {3}\nBalíček driveru: {4}",
        ["setup.supported"]="podporováno; funkce řadiče pro LE Audio se ověří po přepnutí",
        ["setup.unsupported"]="není v podepsaném INF; přepnutí je kvůli ochraně zařízení zakázané",
        ["setup.detect_error"]="Zjištění adaptérů selhalo: {0}",
        ["setup.files_missing"]="Instalační skripty projektu se nepodařilo najít.", ["setup.choose_adapter"]="Nejdřív vyberte adaptér.",
        ["setup.started"]="Nástroj nastavení se otevřel v samostatném okně PowerShellu.",
        ["setup.restart_title"]="Restartovat aplikaci",
        ["setup.restart_body"]="Ukončete OpenLEAudio úplně přes ikonu vedle hodin a spusťte jej znovu. Nový proces otevře vybraný WinUSB adaptér a před hledáním LE Audio zařízení ověří podporu Bluetooth 5.2+ LE Isochronous Channels.",
    };

    private static readonly Dictionary<string, Pack> Custom = new(StringComparer.OrdinalIgnoreCase);
    public static string CurrentCode { get; private set; } = "en";
    public static string DirectoryPath => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenLEAudio", "Languages");

    static Loc() => ReloadCustomPacks();

    public static IEnumerable<(string Code, string Name)> Languages =>
        new[] { ("en", T("language.english", "English")), ("cs", T("language.czech", "Čeština")) }
            .Concat(Custom.Values.OrderBy(p => p.Name).Select(p => (p.Code, p.Name)));

    public static void SetLanguage(string? code) => CurrentCode = code == "cs" || (code is not null && Custom.ContainsKey(code)) ? code : "en";

    public static string T(string key, params object[] args)
    {
        string value;
        if (Custom.TryGetValue(CurrentCode, out var custom) && custom.Strings.TryGetValue(key, out var translated)) value = translated;
        else if (CurrentCode == "cs" && Cs.TryGetValue(key, out var czech)) value = czech;
        else value = En.TryGetValue(key, out var english) ? english : key;
        return args.Length == 0 ? value : string.Format(value, args);
    }

    public static void Apply(DependencyObject root)
    {
        TranslateOne(root);
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++) Apply(VisualTreeHelper.GetChild(root, i));
    }

    private static void TranslateOne(DependencyObject value)
    {
        if (value is TextBlock text) text.Text = TranslateLiteral(text.Text);
        if (value is ContentControl content && content.Content is string label) content.Content = TranslateLiteral(label);
        if (value is ToggleSwitch toggle)
        {
            if (toggle.OnContent is string on) toggle.OnContent = TranslateLiteral(on);
            if (toggle.OffContent is string off) toggle.OffContent = TranslateLiteral(off);
        }
        if (value is InfoBar info)
        {
            info.Title = TranslateLiteral(info.Title);
            info.Message = TranslateLiteral(info.Message);
        }
        if (ToolTipService.GetToolTip(value) is string tip) ToolTipService.SetToolTip(value, TranslateLiteral(tip));
    }

    private static string TranslateLiteral(string source)
    {
        var key = En.FirstOrDefault(x => x.Value == source).Key
            ?? Cs.FirstOrDefault(x => x.Value == source).Key
            ?? Custom.Values.SelectMany(pack => pack.Strings).FirstOrDefault(x => x.Value == source).Key;
        return key is null ? source : T(key);
    }

    public static string Import(string sourcePath)
    {
        var json = File.ReadAllText(sourcePath);
        var pack = JsonSerializer.Deserialize<Pack>(json, new JsonSerializerOptions { PropertyNameCaseInsensitive = true })
            ?? throw new InvalidDataException("Empty language pack.");
        if (string.IsNullOrWhiteSpace(pack.Code) || string.IsNullOrWhiteSpace(pack.Name) || pack.Strings is null)
            throw new InvalidDataException("The pack needs code, name and strings fields.");
        if (pack.Code is "en" or "cs" || pack.Code.Any(c => !(char.IsLetterOrDigit(c) || c is '-' or '_')))
            throw new InvalidDataException("Use a unique language code containing letters, digits, '-' or '_'.");
        Directory.CreateDirectory(DirectoryPath);
        var target = Path.Combine(DirectoryPath, pack.Code + ".json");
        File.Copy(sourcePath, target, true);
        ReloadCustomPacks();
        return pack.Code;
    }

    public static void ExportTemplate(string path)
    {
        var pack = new Pack("my-language", "My language", new Dictionary<string, string>(En));
        File.WriteAllText(path, JsonSerializer.Serialize(pack, new JsonSerializerOptions { WriteIndented = true }));
    }

    private static void ReloadCustomPacks()
    {
        Custom.Clear();
        if (!Directory.Exists(DirectoryPath)) return;
        foreach (var path in Directory.EnumerateFiles(DirectoryPath, "*.json"))
        {
            try
            {
                var pack = JsonSerializer.Deserialize<Pack>(File.ReadAllText(path), new JsonSerializerOptions { PropertyNameCaseInsensitive = true });
                if (pack is not null && !string.IsNullOrWhiteSpace(pack.Code) && !string.IsNullOrWhiteSpace(pack.Name)) Custom[pack.Code] = pack;
            }
            catch { /* One broken community translation must not stop the app. */ }
        }
    }
}
