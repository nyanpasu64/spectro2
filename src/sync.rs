use atomicbox::AtomicBox;
use std::sync::atomic::AtomicBool;

use crate::common::SpectrumFrame;

pub struct AtomicSpectrum {
    pub data: AtomicBox<SpectrumFrame>,
    pub available: AtomicBool,
}

impl AtomicSpectrum {
    pub fn new(spectrum_size: usize) -> AtomicSpectrum {
        AtomicSpectrum {
            data: AtomicBox::new(Box::new(SpectrumFrame::new(spectrum_size))),
            available: false.into(),
        }
    }
}
