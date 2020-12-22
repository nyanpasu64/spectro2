use crate::common::SpectrumFrame;
use crate::sync2::{FlipCell, FlipReader, FlipWriter};

type SpectrumWriter = FlipWriter<SpectrumFrame>;
type SpectrumReader = FlipReader<SpectrumFrame>;

pub fn new_spectrum_cell(spectrum_size: usize) -> (SpectrumWriter, SpectrumReader) {
    FlipCell::new3(
        SpectrumFrame::new(spectrum_size),
        SpectrumFrame::new(spectrum_size),
        SpectrumFrame::new(spectrum_size),
    )
}
