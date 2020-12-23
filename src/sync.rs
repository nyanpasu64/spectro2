use crate::common::SpectrumFrame;
use crate::sync2::{ArcReader, ArcWriter, FlipCell};

type SpectrumWriter = ArcWriter<SpectrumFrame>;
type SpectrumReader = ArcReader<SpectrumFrame>;

pub fn new_spectrum_cell(spectrum_size: usize) -> (SpectrumWriter, SpectrumReader) {
    FlipCell::new3(
        SpectrumFrame::new(spectrum_size),
        SpectrumFrame::new(spectrum_size),
        SpectrumFrame::new(spectrum_size),
    )
    .into_arc()
}
