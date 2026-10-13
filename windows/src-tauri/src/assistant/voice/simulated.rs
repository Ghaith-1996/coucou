// Stand-ins for the parts of voice that touch the OS or a speech engine.
//
// `Transcriber` is what P0 ships with: it hears that something was said and
// for how long, and says so, marked `simulated` so the island labels it. It
// never pretends to understand words. `Microphone` is for the tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Capture, Transcript, VoiceError};

/// The P0 transcriber: deterministic, offline, and honest about it.
#[derive(Default)]
pub struct Transcriber;

impl super::Transcriber for Transcriber {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<Transcript, VoiceError> {
        let seconds = samples.len() as f64 / sample_rate.max(1) as f64;
        Ok(Transcript {
            text: format!("Simulated transcript: {seconds:.1} s of speech heard."),
            simulated: true,
        })
    }
}

/// What a simulated microphone does on each read.
#[derive(Debug, Clone)]
pub enum Chunk {
    /// `n` samples of a tone at this amplitude.
    Tone(usize, f32),
    Silence(usize),
    /// The device goes away.
    Fail(VoiceError),
}

/// A microphone that plays a script, then keeps delivering silence. Each
/// read waits `pace`, like a real device would.
#[derive(Clone)]
pub struct Microphone {
    pub rate: u32,
    pub open_error: Option<VoiceError>,
    pub script: Arc<Mutex<VecDeque<Chunk>>>,
    pub pace: Duration,
    /// How many captures are open right now (the microphone in use).
    pub open_now: Arc<Mutex<usize>>,
}

impl Microphone {
    pub fn new(rate: u32, script: Vec<Chunk>) -> Self {
        Self {
            rate,
            open_error: None,
            script: Arc::new(Mutex::new(script.into())),
            pace: Duration::from_millis(5),
            open_now: Arc::new(Mutex::new(0)),
        }
    }
}

struct Open {
    mic: Microphone,
}

impl Drop for Open {
    fn drop(&mut self) {
        *self.mic.open_now.lock().unwrap() -= 1;
    }
}

impl super::Microphone for Microphone {
    fn open(&self) -> Result<Box<dyn Capture>, VoiceError> {
        if let Some(error) = self.open_error {
            return Err(error);
        }
        *self.open_now.lock().unwrap() += 1;
        Ok(Box::new(Open { mic: self.clone() }))
    }
}

impl Capture for Open {
    fn sample_rate(&self) -> u32 {
        self.mic.rate
    }

    fn read(&mut self, out: &mut Vec<f32>) -> Result<(), VoiceError> {
        std::thread::sleep(self.mic.pace);
        let next = self.mic.script.lock().unwrap().pop_front();
        match next {
            Some(Chunk::Tone(n, amp)) => out.extend((0..n).map(|i| if i % 2 == 0 { amp } else { -amp })),
            Some(Chunk::Silence(n)) => out.extend(std::iter::repeat_n(0.0, n)),
            Some(Chunk::Fail(error)) => return Err(error),
            None => out.extend(std::iter::repeat_n(0.0, (self.mic.rate / 100) as usize)),
        }
        Ok(())
    }
}
