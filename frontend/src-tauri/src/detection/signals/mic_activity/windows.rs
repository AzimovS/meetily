//! Windows mic-activity sampler via WASAPI `IAudioSessionManager2`.
//!
//! Per tick:
//! 1. Enumerate sessions on the default eCapture endpoint.
//! 2. Filter to sessions in `AudioSessionStateActive` (excludes idle
//!    sessions that have a capture endpoint open but aren't transmitting).
//! 3. Skip the system-sounds session and our own PID.
//! 4. Resolve remaining PIDs to their EXE basename via
//!    `QueryFullProcessImageNameW`. That basename becomes the bundle
//!    key the matcher looks up (`Zoom.exe`, `chrome.exe`, ...).
//!
//! No state is cached between ticks — COM objects are acquired and
//! released each call. That's a few sub-millisecond property reads;
//! the `Send + Sync` bound on `SignalSampler` plus COM's `!Send`
//! semantics make caching the enumerator more trouble than it's worth.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use log::{debug, trace, warn};
use windows::{
    core::{Interface, PWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, RPC_E_CHANGED_MODE},
        Media::Audio::{
            eCapture, eConsole, AudioSessionStateActive, IAudioSessionControl2,
            IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
        },
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
            ProcessStatus::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32},
            Threading::{GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    },
};

use crate::detection::signals::SignalSampler;
use crate::detection::types::MicSnapshot;

thread_local! {
    /// Tracks whether this thread has already called `CoInitializeEx`.
    /// We never call `CoUninitialize` — tokio worker threads live the
    /// app's lifetime, so per-thread init once is the correct shape.
    static COM_INITIALIZED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_com() {
    COM_INITIALIZED.with(|flag| {
        if flag.get() {
            return;
        }
        // SAFETY: CoInitializeEx is always safe to call; it returns
        // S_OK on first init, S_FALSE if already initialized to the
        // same apartment, RPC_E_CHANGED_MODE if a different apartment
        // was previously set. All three are fine for our purposes —
        // we only need *some* COM apartment on this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            warn!("CoInitializeEx failed: {:?}", hr);
        }
        flag.set(true);
    });
}

pub struct WindowsMicActivitySampler {
    own_pid: u32,
    /// PIDs for which OpenProcess has already been denied. Used to
    /// warn once-per-PID rather than spamming logs every tick. Typical
    /// cause: EDR software (Defender for Endpoint, CrowdStrike) blocks
    /// cross-process queries for its protected processes.
    warned_denied_pids: Mutex<HashSet<u32>>,
}

impl WindowsMicActivitySampler {
    pub fn new() -> Result<Self> {
        ensure_com();
        let own_pid = unsafe { GetCurrentProcessId() };

        // Probe on construction so we fail fast if WASAPI is broken
        // (headless CI, no audio subsystem). The factory falls back to
        // the stub sampler in that case.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| anyhow!("CoCreateInstance IMMDeviceEnumerator failed: {:?}", e))?;
            let _device = enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .map_err(|e| anyhow!("No default capture endpoint: {:?}", e))?;
        }

        Ok(Self {
            own_pid,
            warned_denied_pids: Mutex::new(HashSet::new()),
        })
    }

    fn snapshot_inner(&self) -> Result<MicSnapshot> {
        ensure_com();

        // SAFETY: every call below is a normal WASAPI / COM pattern.
        // `windows` crate handles ref-counting via Drop; per-tick
        // acquire-and-release is bounded and cheap.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("CoCreateInstance IMMDeviceEnumerator")?;

            let device = enumerator
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .context("GetDefaultAudioEndpoint(eCapture)")?;

            let session_manager: IAudioSessionManager2 = device
                .Activate(CLSCTX_ALL, None)
                .context("Activate IAudioSessionManager2")?;

            let enum_sessions = session_manager
                .GetSessionEnumerator()
                .context("GetSessionEnumerator")?;

            let count = enum_sessions.GetCount().context("GetCount")?;

            let mut active = Vec::new();

            for i in 0..count {
                let session = match enum_sessions.GetSession(i) {
                    Ok(s) => s,
                    Err(e) => {
                        trace!("GetSession({}) failed: {:?}", i, e);
                        continue;
                    }
                };

                let session2: IAudioSessionControl2 = match session.cast() {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                // Note: we don't use `IsSystemSoundsSession()` to skip the
                // system session because its Result<()> mapping makes
                // S_OK and S_FALSE indistinguishable — checking would
                // always "match". Instead we rely on the PID-0 filter
                // below, which is what the system session reports.

                let state = match session2.GetState() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if state != AudioSessionStateActive {
                    continue;
                }

                let pid = match session2.GetProcessId() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if pid == 0 || pid == self.own_pid {
                    continue;
                }

                match self.process_exe_basename(pid) {
                    Some(name) => active.push(name),
                    None => trace!("couldn't resolve PID {} to exe name", pid),
                }
            }

            debug!(
                "Windows mic snapshot: {} active {:?}",
                active.len(),
                active
            );
            Ok(MicSnapshot {
                active_bundles: active,
            })
        }
    }
}

impl WindowsMicActivitySampler {
    fn process_exe_basename(&self, pid: u32) -> Option<String> {
        // SAFETY: OpenProcess / QueryFullProcessImageNameW / CloseHandle
        // form a standard Win32 pattern. We use PROCESS_QUERY_LIMITED_INFORMATION
        // which is the least-privileged access that still lets us read the
        // image path, minimising AV false positives.
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(handle) => {
                    let result = query_image_basename(handle);
                    let _ = CloseHandle(handle);
                    result
                }
                Err(e) => {
                    if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
                        // Warn once per PID — EDR-protected processes
                        // routinely deny even PROCESS_QUERY_LIMITED_INFORMATION.
                        // Without this breadcrumb the feature appears
                        // permanently broken to the user.
                        let mut warned = self.warned_denied_pids.lock().unwrap();
                        if warned.insert(pid) {
                            warn!(
                                "OpenProcess denied for pid {} (likely EDR-protected); \
                                 {} unique PIDs seen so far",
                                pid,
                                warned.len()
                            );
                        }
                    } else {
                        trace!("OpenProcess({}) failed: {:?}", pid, e);
                    }
                    None
                }
            }
        }
    }
}

unsafe fn query_image_basename(handle: HANDLE) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = buf.len() as u32;
    QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buf.as_mut_ptr()),
        &mut size,
    )
    .ok()?;
    let len = size as usize;
    if len == 0 || len > buf.len() {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len]);
    Path::new(&path)
        .file_name()
        .and_then(|os| os.to_str())
        .map(|s| s.to_string())
}

impl SignalSampler for WindowsMicActivitySampler {
    fn snapshot(&self) -> Result<MicSnapshot> {
        match self.snapshot_inner() {
            Ok(s) => Ok(s),
            Err(e) => {
                warn!("Windows mic-activity snapshot failed: {}", e);
                Ok(MicSnapshot::default())
            }
        }
    }
}
