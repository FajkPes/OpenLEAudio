//! Isochronous transfers over WinUSB.
//!
//! LE Audio carries sound on isochronous endpoints, and `nusb` exposes no
//! isochronous API on Windows - so those pipes are driven through the WinUSB
//! functions directly. Everything else (commands, events, ACL) stays on `nusb`.
//!
//! The buffer handed to `WinUsb_RegisterIsochBuffer` must outlive every transfer
//! that references it, so `IsochBuffer` owns its memory and unregisters on drop.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr;

use windows::core::PCWSTR;
use windows::core::GUID;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::Usb::{
    WinUsb_ControlTransfer, WinUsb_Free, WinUsb_GetAssociatedInterface, WinUsb_Initialize,
    WinUsb_AbortPipe, WinUsb_QueryPipe, WinUsb_ReadPipe, WinUsb_RegisterIsochBuffer, WinUsb_SetCurrentAlternateSetting,
    WinUsb_SetPipePolicy, WinUsb_UnregisterIsochBuffer, WinUsb_WriteIsochPipeAsap, WinUsb_WritePipe,
    WINUSB_INTERFACE_HANDLE, WINUSB_PIPE_INFORMATION, WINUSB_SETUP_PACKET,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::GetOverlappedResult;

/// One overlapped operation, with its own completion event.
///
/// Several transfers run at once on a single WinUSB handle: two reader threads
/// and the audio loop. `GetOverlappedResult` falls back to signalling the file
/// handle itself when `hEvent` is null, so without a private event per operation
/// the three of them collect each other's completions - reads return zero bytes
/// and packets arrive on the wrong pipe.
struct Operation {
    overlapped: Box<OVERLAPPED>,
}

impl Operation {
    fn new() -> Result<Self> {
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|_| WinUsbError::Control(last_error()))?;

        let mut overlapped = Box::new(OVERLAPPED::default());
        overlapped.hEvent = event;

        Ok(Self { overlapped })
    }

    fn as_mut(&mut self) -> *mut OVERLAPPED {
        self.overlapped.as_mut() as *mut OVERLAPPED
    }

    fn as_ref(&self) -> &OVERLAPPED {
        self.overlapped.as_ref()
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.overlapped.hEvent);
        }
    }
}
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::IO::OVERLAPPED;

#[derive(Debug, thiserror::Error)]
pub enum WinUsbError {
    #[error("could not open device at {path}: Windows error {code}")]
    Open { path: String, code: u32 },

    #[error("WinUsb_Initialize failed: Windows error {0}")]
    Initialize(u32),

    #[error("no isochronous endpoint found on interface {0}")]
    NoIsochEndpoint(u8),

    #[error("could not select alternate setting {setting}: Windows error {code}")]
    AlternateSetting { setting: u8, code: u32 },

    #[error("isochronous buffer registration failed: Windows error {0}")]
    BufferRegistration(u32),

    #[error("isochronous write failed: Windows error {0}")]
    Write(u32),

    #[error("payload of {got} bytes exceeds the {capacity} byte buffer")]
    PayloadTooLarge { got: usize, capacity: usize },

    #[error("no device found for interface class {0:?} - is the adapter bound to WinUSB?")]
    NoDeviceInterface(GUID),

    #[error("could not reach interface {index} of the device: Windows error {code}")]
    AssociatedInterface { index: u8, code: u32 },

    #[error("control transfer failed: Windows error {0}")]
    Control(u32),

    #[error("transfer on pipe {pipe:#04x} failed: Windows error {code}")]
    Pipe { pipe: u8, code: u32 },
}

/// Device interface class published by our INF.
///
/// The adapter's WinUSB path is found through this rather than by guessing at a
/// device path, so the lookup keeps working when the adapter moves ports.
pub const OLEA_INTERFACE_GUID: GUID = GUID::from_u128(0xB7C4E0A1_3F62_4D18_9E5B_2A8F6C1D4E70);

/// Finds the device path for a device interface class.
///
/// SetupAPI answers with a variable-length structure, so this asks twice: once
/// for the size, once for the content.
pub fn find_interface_path(interface: GUID) -> Result<String> {
    unsafe {
        let set = SetupDiGetClassDevsW(
            Some(&interface),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|_| WinUsbError::NoDeviceInterface(interface))?;

        let mut found = None;

        for index in 0..64u32 {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };

            if SetupDiEnumDeviceInterfaces(set, None, &interface, index, &mut data).is_err() {
                break;
            }

            // First call fails with "insufficient buffer" and fills in the size.
            let mut required = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut required), None);
            if required == 0 {
                continue;
            }

            let mut buffer = vec![0u8; required as usize];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;

            if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), required, None, None)
                .is_ok()
            {
                // DevicePath is a variable-length wide string at the end of the
                // structure; walk it to its terminator.
                let path_ptr = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let path_offset = path_ptr as usize - buffer.as_ptr() as usize;
                if path_offset >= buffer.len() {
                    continue;
                }
                let max_units = (buffer.len() - path_offset) / std::mem::size_of::<u16>();
                let mut length = 0usize;
                // Check the allocation-derived bound before dereferencing.
                while length < max_units && *path_ptr.add(length) != 0 {
                    length += 1;
                }
                if length == max_units {
                    continue;
                }
                let slice = std::slice::from_raw_parts(path_ptr, length);
                found = Some(String::from_utf16_lossy(slice));
                break;
            }
        }

        let _ = SetupDiDestroyDeviceInfoList(set);
        found.ok_or(WinUsbError::NoDeviceInterface(interface))
    }
}

type Result<T> = std::result::Result<T, WinUsbError>;

fn last_error() -> u32 {
    unsafe { GetLastError().0 }
}

/// Endpoint direction and type, as reported by `WinUsb_QueryPipe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

impl PipeType {
    fn from_raw(value: i32) -> Option<Self> {
        Some(match value {
            0 => PipeType::Control,
            1 => PipeType::Isochronous,
            2 => PipeType::Bulk,
            3 => PipeType::Interrupt,
            _ => return None,
        })
    }
}

/// One endpoint on an interface.
#[derive(Debug, Clone, Copy)]
pub struct PipeInfo {
    pub id: u8,
    pub pipe_type: PipeType,
    pub max_packet_size: u16,
    pub interval: u8,
}

impl PipeInfo {
    pub fn is_input(&self) -> bool {
        self.id & 0x80 != 0
    }
}

/// An open WinUSB handle on one USB interface.
pub struct WinUsbInterface {
    /// Owned only by the interface that opened the device; an associated view
    /// leaves this empty so the handle is closed exactly once.
    device: HANDLE,
    /// The owner's file handle, needed for overlapped waits even when this view
    /// does not own it.
    borrowed_device: HANDLE,
    handle: WINUSB_INTERFACE_HANDLE,
    interface_number: u8,
}

// WinUSB handles are safe to use from several threads as long as each pipe has
// one owner, which is how this stack drives them: one thread blocked on events,
// one on ACL, and the audio loop writing. Nothing here mutates shared state.
unsafe impl Send for WinUsbInterface {}
unsafe impl Sync for WinUsbInterface {}

impl WinUsbInterface {
    /// Opens a device by its interface path and initialises WinUSB on it.
    ///
    /// The path comes from SetupAPI, in the usual `\\?\usb#vid_...` form.
    pub fn open(path: &str, interface_number: u8) -> Result<Self> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        // GENERIC_READ | GENERIC_WRITE, which is what the WinUSB documentation
        // specifies and what every other WinUSB client asks for. The specific
        // rights FILE_GENERIC_READ/WRITE look equivalent but also demand
        // READ_CONTROL, which a device object's security descriptor does not
        // grant - the open then fails with access denied even though nothing
        // else holds the device.
        const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

        let device = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|_| WinUsbError::Open {
            path: path.to_owned(),
            code: last_error(),
        })?;

        if device == INVALID_HANDLE_VALUE {
            return Err(WinUsbError::Open {
                path: path.to_owned(),
                code: last_error(),
            });
        }

        let mut handle = WINUSB_INTERFACE_HANDLE::default();
        let initialised = unsafe { WinUsb_Initialize(device, &mut handle) };

        if initialised.is_err() {
            let code = last_error();
            unsafe { let _ = CloseHandle(device); }
            return Err(WinUsbError::Initialize(code));
        }

        Ok(Self {
            device,
            borrowed_device: device,
            handle,
            interface_number,
        })
    }

    /// Lists the endpoints of the current alternate setting.
    pub fn pipes(&self, alternate_setting: u8) -> Vec<PipeInfo> {
        let mut pipes = Vec::new();

        for index in 0..32u8 {
            let mut info = WINUSB_PIPE_INFORMATION::default();
            let queried = unsafe {
                WinUsb_QueryPipe(self.handle, alternate_setting, index, &mut info)
            };

            if queried.is_err() {
                break;
            }

            if let Some(pipe_type) = PipeType::from_raw(info.PipeType.0) {
                pipes.push(PipeInfo {
                    id: info.PipeId,
                    pipe_type,
                    max_packet_size: info.MaximumPacketSize,
                    interval: info.Interval,
                });
            }
        }

        pipes
    }

    /// Selects an alternate setting. Bluetooth controllers park isochronous
    /// endpoints on non-zero settings, so this must be called before streaming.
    pub fn set_alternate_setting(&mut self, setting: u8) -> Result<()> {
        let ok = unsafe { WinUsb_SetCurrentAlternateSetting(self.handle, setting) };

        if ok.is_err() {
            return Err(WinUsbError::AlternateSetting {
                setting,
                code: last_error(),
            });
        }
        Ok(())
    }

    /// Finds the outbound isochronous endpoint on the given alternate setting.
    pub fn find_isoch_out(&self, alternate_setting: u8) -> Result<PipeInfo> {
        self.pipes(alternate_setting)
            .into_iter()
            .find(|p| p.pipe_type == PipeType::Isochronous && !p.is_input())
            .ok_or(WinUsbError::NoIsochEndpoint(self.interface_number))
    }

    /// Registers a buffer for isochronous writes on a pipe.
    pub fn register_isoch_buffer(&mut self, pipe_id: u8, capacity: usize) -> Result<IsochBuffer> {
        self.register_isoch_ring(pipe_id, capacity, 1)
    }

    /// Registers a buffer divided into `slots` independently transmitted regions.
    ///
    /// An isochronous write is asynchronous: the buffer must stay untouched until
    /// the transfer completes. Writing every packet to the same bytes would
    /// overwrite audio that is still on its way out, so packets rotate through
    /// slots and each carries its own OVERLAPPED.
    pub fn register_isoch_ring(
        &mut self,
        pipe_id: u8,
        slot_size: usize,
        slots: usize,
    ) -> Result<IsochBuffer> {
        let slots = slots.max(1);
        let slot_size = slot_size.max(1);
        let capacity = slot_size * slots;

        let mut storage = vec![0u8; capacity];
        // Allocate every completion object before registration. If this fails,
        // WinUSB has not yet retained a pointer to `storage`.
        let operations = (0..slots)
            .map(|_| Operation::new())
            .collect::<Result<Vec<_>>>()?;
        let mut buffer_handle: *mut c_void = ptr::null_mut();

        // WinUSB keeps the pointer to this memory, so `storage` must not move or
        // reallocate while the buffer stays registered. It is owned by IsochBuffer
        // below and only released after WinUsb_UnregisterIsochBuffer.
        let ok = unsafe {
            WinUsb_RegisterIsochBuffer(
                self.handle,
                pipe_id,
                storage.as_mut_slice(),
                &mut buffer_handle,
            )
        };

        if ok.is_err() {
            return Err(WinUsbError::BufferRegistration(last_error()));
        }

        Ok(IsochBuffer {
            handle: buffer_handle,
            interface: self.handle,
            pipe_id,
            device: self.device_handle(),
            storage,
            slot_size,
            slots,
            next_slot: 0,
            used: vec![false; slots],
            submitted: 0,
            failed: 0,
            last_error: 0,
            operations,
            continuing: false,
        })
    }

    /// Opens another interface of the same device.
    ///
    /// A Bluetooth controller keeps its isochronous endpoints on interface 1
    /// while commands and ACL live on interface 0, and WinUSB reaches the second
    /// one only through the first. `index` is zero-based from the interface this
    /// handle was opened on, so 0 means the next one.
    pub fn associated(&self, index: u8) -> Result<Self> {
        let mut handle = WINUSB_INTERFACE_HANDLE::default();

        let ok = unsafe { WinUsb_GetAssociatedInterface(self.handle, index, &mut handle) };
        if ok.is_err() {
            return Err(WinUsbError::AssociatedInterface {
                index,
                code: last_error(),
            });
        }

        Ok(Self {
            // The device handle belongs to the interface this one came from and
            // must not be closed twice, so the associated view does not own it.
            device: HANDLE::default(),
            borrowed_device: self.borrowed_device,
            handle,
            interface_number: self.interface_number + index + 1,
        })
    }

    pub fn interface_number(&self) -> u8 {
        self.interface_number
    }

    /// Sends a control transfer with no data returned.
    ///
    /// This is how HCI commands reach a Bluetooth controller: a class request to
    /// the device, with the command packet as the payload.
    pub fn control_out(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        let setup = WINUSB_SETUP_PACKET {
            RequestType: request_type,
            Request: request,
            Value: value,
            Index: index,
            Length: data.len() as u16,
        };

        // The buffer is only read for the duration of the call, and a control
        // transfer this small completes without going asynchronous.
        let mut sent = 0u32;
        let mut scratch = data.to_vec();
        let ok = unsafe {
            WinUsb_ControlTransfer(
                self.handle,
                setup,
                Some(scratch.as_mut_slice()),
                Some(&mut sent),
                None,
            )
        };

        if ok.is_err() {
            return Err(WinUsbError::Control(last_error()));
        }
        Ok(())
    }

    /// Reads from an IN pipe, blocking until data arrives.
    ///
    /// The handle is overlapped, so the call returns immediately and the wait
    /// happens in `GetOverlappedResult`. That keeps one blocked reader thread per
    /// pipe rather than a polling loop.
    /// Cancels whatever is pending on a pipe, so a blocked read returns.
    ///
    /// The reader threads spend their lives inside a blocking read. Dropping the
    /// receiving end does not wake them - they only notice once a packet arrives
    /// and the send fails - so on an idle adapter they block forever, and each
    /// one holds a reference to the interface. The handle then stays open after
    /// everything that owns it has been dropped, and the next attempt to open
    /// the adapter is told it belongs to another driver. It does: to us.
    pub fn abort_pipe(&self, pipe: u8) {
        unsafe {
            let _ = WinUsb_AbortPipe(self.handle, pipe);
        }
    }

    pub fn read_pipe(&self, pipe: u8, capacity: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; capacity];
        let mut operation = Operation::new()?;
        let mut read = 0u32;

        let started = unsafe {
            WinUsb_ReadPipe(
                self.handle,
                pipe,
                Some(buffer.as_mut_slice()),
                Some(&mut read),
                Some(operation.as_mut()),
            )
        };

        if started.is_err() {
            const ERROR_IO_PENDING: u32 = 997;
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(WinUsbError::Pipe { pipe, code });
            }

            let finished = unsafe {
                GetOverlappedResult(self.device_handle(), operation.as_ref(), &mut read, true)
            };
            if finished.is_err() {
                return Err(WinUsbError::Pipe { pipe, code: last_error() });
            }
        }

        buffer.truncate(read as usize);
        Ok(buffer)
    }

    /// Writes to an OUT pipe, blocking until the transfer completes.
    pub fn write_pipe(&self, pipe: u8, data: &[u8]) -> Result<()> {
        let mut operation = Operation::new()?;
        let mut written = 0u32;

        let started = unsafe {
            WinUsb_WritePipe(
                self.handle,
                pipe,
                data,
                Some(&mut written),
                Some(operation.as_mut()),
            )
        };

        if started.is_err() {
            const ERROR_IO_PENDING: u32 = 997;
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(WinUsbError::Pipe { pipe, code });
            }

            let finished = unsafe {
                GetOverlappedResult(self.device_handle(), operation.as_ref(), &mut written, true)
            };
            if finished.is_err() {
                return Err(WinUsbError::Pipe { pipe, code: last_error() });
            }
        }

        Ok(())
    }

    /// Stops a read from failing when the controller simply has nothing to say.
    ///
    /// Without this WinUSB fails a read that returns fewer bytes than asked for,
    /// which is the normal case for HCI events.
    pub fn allow_short_reads(&self, pipe: u8) -> Result<()> {
        use windows::Win32::Devices::Usb::WINUSB_PIPE_POLICY;
        const ALLOW_PARTIAL_READS: WINUSB_PIPE_POLICY = WINUSB_PIPE_POLICY(0x05);
        const AUTO_CLEAR_STALL: WINUSB_PIPE_POLICY = WINUSB_PIPE_POLICY(0x01);

        let enabled: u8 = 1;
        for policy in [ALLOW_PARTIAL_READS, AUTO_CLEAR_STALL] {
            let ok = unsafe {
                WinUsb_SetPipePolicy(
                    self.handle,
                    pipe,
                    policy,
                    1,
                    &enabled as *const u8 as *const c_void,
                )
            };
            if ok.is_err() {
                return Err(WinUsbError::Pipe { pipe, code: last_error() });
            }
        }
        Ok(())
    }

    /// The file handle overlapped waits are made against.
    fn device_handle(&self) -> HANDLE {
        if self.device.is_invalid() || self.device == HANDLE::default() {
            self.borrowed_device
        } else {
            self.device
        }
    }
}

impl Drop for WinUsbInterface {
    fn drop(&mut self) {
        unsafe {
            let _ = WinUsb_Free(self.handle);
            // An associated interface borrows the device handle of the interface
            // it came from; only the owner closes it.
            if !self.device.is_invalid() && self.device != HANDLE::default() {
                let _ = CloseHandle(self.device);
            }
        }
    }
}

/// A registered isochronous buffer. Owns its memory for as long as WinUSB may
/// reference it, and unregisters before that memory is released.
pub struct IsochBuffer {
    handle: *mut c_void,
    interface: WINUSB_INTERFACE_HANDLE,
    pipe_id: u8,
    /// The file handle completions are collected against.
    device: HANDLE,
    storage: Vec<u8>,
    slot_size: usize,
    slots: usize,
    next_slot: usize,
    /// Whether a slot has ever been submitted, so its result is only collected
    /// once there is one to collect.
    used: Vec<bool>,
    submitted: u64,
    failed: u64,
    last_error: u32,
    /// One per slot, each with its own completion event.
    operations: Vec<Operation>,
    continuing: bool,
}

unsafe impl Send for IsochBuffer {}

impl IsochBuffer {
    /// Finishes a previous operation before its buffer, OVERLAPPED or event is
    /// reused. A one-second bound prevents a broken controller from hanging the
    /// audio thread forever; a timeout aborts the whole pipe and triggers the
    /// normal reconnect path.
    fn finish_slot(&mut self, slot: usize) -> Result<()> {
        if !self.used[slot] {
            return Ok(());
        }

        let mut transferred = 0u32;
        let mut result = unsafe {
            GetOverlappedResult(
                self.device,
                self.operations[slot].as_ref(),
                &mut transferred,
                false,
            )
        };

        const ERROR_IO_INCOMPLETE: u32 = 996;
        if result.is_err() && last_error() == ERROR_IO_INCOMPLETE {
            let waited = unsafe {
                WaitForSingleObject(self.operations[slot].as_ref().hEvent, 1000)
            };
            if waited == WAIT_TIMEOUT {
                unsafe { let _ = WinUsb_AbortPipe(self.interface, self.pipe_id); }
                self.continuing = false;
            } else if waited != WAIT_OBJECT_0 {
                unsafe { let _ = WinUsb_AbortPipe(self.interface, self.pipe_id); }
            }
            result = unsafe {
                GetOverlappedResult(
                    self.device,
                    self.operations[slot].as_ref(),
                    &mut transferred,
                    true,
                )
            };
        }

        self.used[slot] = false;
        if result.is_err() {
            let code = last_error();
            self.failed += 1;
            self.last_error = code;
            return Err(WinUsbError::Write(code));
        }

        // A completed OVERLAPPED is not resubmitted. A fresh one also gives the
        // next transfer a clean, unsignalled event.
        self.operations[slot] = Operation::new()?;
        Ok(())
    }

    /// Queues one payload for transmission at the next isochronous opportunity.
    ///
    /// The first packet starts a stream; later ones continue it, which keeps the
    /// controller's frame timing aligned instead of restarting it each interval.
    pub fn write(&mut self, payload: &[u8]) -> Result<()> {
        if payload.len() > self.slot_size {
            return Err(WinUsbError::PayloadTooLarge {
                got: payload.len(),
                capacity: self.slot_size,
            });
        }

        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.slots;

        // Collect the result of this slot's previous transfer before reusing it.
        // Without this a failing pipe looks exactly like a working one: the
        // submit call succeeds every time and the error only ever appears in the
        // completion nobody reads.
        self.finish_slot(slot)?;

        let offset = slot * self.slot_size;
        self.storage[offset..offset + payload.len()].copy_from_slice(payload);

        let ok = unsafe {
            WinUsb_WriteIsochPipeAsap(
                self.handle,
                offset as u32,
                payload.len() as u32,
                self.continuing,
                Some(self.operations[slot].as_ref() as *const OVERLAPPED),
            )
        };

        // ERROR_IO_PENDING is the normal outcome for an overlapped transfer.
        const ERROR_IO_PENDING: u32 = 997;
        if ok.is_err() {
            let code = last_error();
            if code != ERROR_IO_PENDING {
                return Err(WinUsbError::Write(code));
            }
        }

        self.used[slot] = true;
        self.submitted += 1;
        self.continuing = true;
        Ok(())
    }

    /// How many transfers were submitted, and how many came back failed.
    pub fn statistics(&self) -> (u64, u64, u32) {
        (self.submitted, self.failed, self.last_error)
    }

    /// Marks the stream as broken, so the next write restarts frame timing.
    pub fn restart_stream(&mut self) {
        self.continuing = false;
    }

    /// The largest single payload this buffer accepts.
    pub fn capacity(&self) -> usize {
        self.slot_size
    }
}

impl Drop for IsochBuffer {
    fn drop(&mut self) {
        unsafe {
            // Cancel first, then collect every completion. Only after WinUSB no
            // longer owns any operation may its buffer and events be released.
            let _ = WinUsb_AbortPipe(self.interface, self.pipe_id);
            for slot in 0..self.slots {
                if self.used[slot] {
                    let mut transferred = 0u32;
                    let _ = GetOverlappedResult(
                        self.device,
                        self.operations[slot].as_ref(),
                        &mut transferred,
                        true,
                    );
                    self.used[slot] = false;
                }
            }

            if WinUsb_UnregisterIsochBuffer(self.handle).is_err() {
                // Catastrophic driver failure: leaking a small ring is safer
                // than handing WinUSB dangling pointers into freed Rust memory.
                std::mem::forget(std::mem::take(&mut self.storage));
                std::mem::forget(std::mem::take(&mut self.operations));
            }
        }
    }
}

/// The audio path: one open isochronous pipe, ready to take HCI ISO packets.
///
/// Ties together everything above - find the device, reach the interface the
/// isochronous endpoints live on, pick an alternate setting that actually
/// reserves bandwidth, and register a buffer to write through.
pub struct IsoSink {
    // Declared first so it drops first: the buffer must be unregistered while
    // its interface is still open.
    buffer: IsochBuffer,
    _interface: WinUsbInterface,
    pipe: PipeInfo,
    alternate_setting: u8,
}

impl IsoSink {
    /// Opens the outbound isochronous pipe of the adapter bound to WinUSB.
    ///
    /// `alternate_setting` 0 reserves no bandwidth at all, so the widest setting
    /// the controller offers is chosen instead.
    pub fn from_interface(mut interface: WinUsbInterface) -> Result<Self> {
        // Widest setting wins: more bytes per interval means fewer frames spent
        // on one SDU, and the narrow ones cannot carry an LC3 frame at all.
        let mut best: Option<(u8, PipeInfo)> = None;
        for setting in 0..8u8 {
            if let Ok(pipe) = interface.find_isoch_out(setting) {
                let better = best
                    .as_ref()
                    .map(|(_, current)| pipe.max_packet_size > current.max_packet_size)
                    .unwrap_or(true);
                if better {
                    best = Some((setting, pipe));
                }
            }
        }

        let (alternate_setting, pipe) =
            best.ok_or(WinUsbError::NoIsochEndpoint(interface.interface_number()))?;

        interface.set_alternate_setting(alternate_setting)?;

        // Eight slots at a 10 ms interval means a slot is reused after 80 ms,
        // far longer than a transfer of this size takes to complete.
        const SLOTS: usize = 8;
        const SLOT_SIZE: usize = 512;

        let buffer = interface.register_isoch_ring(pipe.id, SLOT_SIZE, SLOTS)?;

        Ok(Self {
            buffer,
            _interface: interface,
            pipe,
            alternate_setting,
        })
    }

    /// Sends one HCI ISO data packet.
    pub fn send(&mut self, packet: &[u8]) -> Result<()> {
        self.buffer.write(packet)
    }

    /// Restarts frame timing, for when a stream is torn down and set up again.
    pub fn restart(&mut self) {
        self.buffer.restart_stream();
    }

    /// Submitted transfers, failed transfers, and the last error code.
    pub fn statistics(&self) -> (u64, u64, u32) {
        self.buffer.statistics()
    }

    pub fn describe(&self) -> String {
        format!(
            "endpoint {:#04x}, alt setting {}, {} B na paket",
            self.pipe.id, self.alternate_setting, self.pipe.max_packet_size
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_direction_comes_from_the_endpoint_address() {
        let out = PipeInfo {
            id: 0x02,
            pipe_type: PipeType::Isochronous,
            max_packet_size: 192,
            interval: 1,
        };
        let inbound = PipeInfo { id: 0x82, ..out };

        assert!(!out.is_input());
        assert!(inbound.is_input());
    }

    #[test]
    fn pipe_types_map_from_the_raw_values() {
        assert_eq!(PipeType::from_raw(1), Some(PipeType::Isochronous));
        assert_eq!(PipeType::from_raw(2), Some(PipeType::Bulk));
        assert_eq!(PipeType::from_raw(9), None);
    }
}
