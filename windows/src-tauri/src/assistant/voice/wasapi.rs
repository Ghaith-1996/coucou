// The default microphone on Windows, through WASAPI in shared mode — the
// `windows` crate the app already uses, no new dependency.
//
// Opened on the capture thread (COM is set up there and torn down with it),
// read every few tens of milliseconds, mixed down to mono f32. Dropping the
// capture stops the stream and releases the device, which is when Windows'
// microphone indicator goes away.

use windows::core::HRESULT;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use super::{Capture, VoiceError};

/// `E_ACCESSDENIED`: Settings → Privacy → Microphone refuses desktop apps.
const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005_u32 as i32);
/// `E_NOTFOUND` / `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`: no capture device.
const E_NOTFOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// A one-second buffer, in 100 ns units: plenty for reads every ~20 ms.
const BUFFER_100NS: i64 = 10_000_000;

#[derive(Default)]
pub struct Microphone;

impl super::Microphone for Microphone {
    fn open(&self) -> Result<Box<dyn Capture>, VoiceError> {
        Ok(Box::new(Wasapi::open()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    F32,
    I16,
    I32,
}

struct Wasapi {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    channels: usize,
    rate: u32,
    sample: Sample,
    /// Last: fields drop in order, so the COM objects above are released
    /// before COM itself is torn down.
    _com: ComGuard,
}

/// This thread's COM, when it was ours to set up (and so to tear down).
struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn device_error(err: &windows::core::Error) -> VoiceError {
    match err.code() {
        E_ACCESSDENIED => VoiceError::MicrophoneDenied,
        _ => VoiceError::NoMicrophone,
    }
}

impl Wasapi {
    fn open() -> Result<Self, VoiceError> {
        // S_OK or S_FALSE: ours to undo. Already set up differently on this
        // thread: carry on with what is there.
        let com = ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
        let (client, capture, channels, rate, sample) = unsafe { Self::open_stream() }?;
        Ok(Self { client, capture, channels, rate, sample, _com: com })
    }

    unsafe fn open_stream() -> Result<(IAudioClient, IAudioCaptureClient, usize, u32, Sample), VoiceError> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| device_error(&e))?;
        let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|e| {
            if e.code() == E_NOTFOUND {
                VoiceError::NoMicrophone
            } else {
                device_error(&e)
            }
        })?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| device_error(&e))?;
        let format = client.GetMixFormat().map_err(|e| device_error(&e))?;
        let parsed = parse_format(format);
        let init = parsed.and_then(|_| {
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, BUFFER_100NS, 0, format, None)
                .map_err(|e| device_error(&e))
        });
        CoTaskMemFree(Some(format as *const _));
        let (channels, rate, sample) = parsed?;
        init?;
        let capture: IAudioCaptureClient = client.GetService().map_err(|e| device_error(&e))?;
        client.Start().map_err(|e| device_error(&e))?;
        Ok((client, capture, channels, rate, sample))
    }
}

/// Channels, rate and sample type of the shared-mode mix format. Anything
/// but float, 16-bit or 32-bit PCM is refused rather than misread.
unsafe fn parse_format(format: *const WAVEFORMATEX) -> Result<(usize, u32, Sample), VoiceError> {
    if format.is_null() {
        return Err(VoiceError::NoMicrophone);
    }
    let base: WAVEFORMATEX = std::ptr::read_unaligned(format);
    let tag = if base.wFormatTag == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22 {
        let ext: WAVEFORMATEXTENSIBLE = std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE);
        // KSDATAFORMAT_SUBTYPE_PCM / _IEEE_FLOAT: the format tag in data1.
        let sub = ext.SubFormat;
        sub.data1 as u16
    } else {
        base.wFormatTag
    };
    let sample = match (tag, base.wBitsPerSample) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => Sample::F32,
        (WAVE_FORMAT_PCM, 16) => Sample::I16,
        (WAVE_FORMAT_PCM, 32) => Sample::I32,
        _ => return Err(VoiceError::NoMicrophone),
    };
    let channels = base.nChannels.max(1) as usize;
    Ok((channels, base.nSamplesPerSec, sample))
}

impl Capture for Wasapi {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), VoiceError> {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let lost = |e: windows::core::Error| {
            if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                VoiceError::NoMicrophone
            } else {
                device_error(&e)
            }
        };
        unsafe {
            loop {
                let packet = self.capture.GetNextPacketSize().map_err(lost)?;
                if packet == 0 {
                    return Ok(());
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).map_err(lost)?;
                let frames_usize = frames as usize;
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    out.extend(std::iter::repeat_n(0.0, frames_usize));
                } else {
                    let values = frames_usize * self.channels;
                    mix_down(data, values, self.channels, self.sample, out);
                }
                self.capture.ReleaseBuffer(frames).map_err(lost)?;
            }
        }
    }
}

/// Interleaved frames → mono, averaged over the channels.
unsafe fn mix_down(data: *const u8, values: usize, channels: usize, sample: Sample, out: &mut Vec<f32>) {
    let value = |i: usize| -> f32 {
        match sample {
            Sample::F32 => std::ptr::read_unaligned((data as *const f32).add(i)),
            Sample::I16 => std::ptr::read_unaligned((data as *const i16).add(i)) as f32 / 32_768.0,
            Sample::I32 => std::ptr::read_unaligned((data as *const i32).add(i)) as f32 / 2_147_483_648.0,
        }
    };
    out.reserve(values / channels);
    for frame in 0..values / channels {
        let sum: f32 = (0..channels).map(|c| value(frame * channels + c)).sum();
        out.push((sum / channels as f32).clamp(-1.0, 1.0));
    }
}

impl Drop for Wasapi {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
