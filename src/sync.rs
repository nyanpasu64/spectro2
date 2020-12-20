/// Wildly unsafe. TODO test for UB using miri and loom.
use crate::common::SpectrumFrame;
use atomig::{Atom, Atomic, Ordering};
use std::cell::UnsafeCell;
use std::sync::Arc;

/// Allows passing SpectrumFrame from a writer to a reader.
/// The reader and writer each can obtain a non-aliased &mut to an instance.
/// The reader can "fetch" the latest version "published" by the writer.
///
/// This struct stores three SpectrumFrame instances in an array.
/// One is accessible by the reader, one is accessible by the writer,
/// and one is "published" and not accessible until the reader or writer claims it.
/// It may be available (not yet seen by reader) or stale (already processed by reader).
///
/// "is available" and "which instance is published" are stored in a single atomic value
/// (SpectrumStatus).
///
/// Unlike a pointer to a struct{atomic...}, the reader never sees torn writes.
/// Unlike an atomic box, it never allocates or deallocates.
/// Unlike a channel, writing never blocks, and instead replaces the in-flight entry.
///
/// I don't know if this code is exception-safe.
/// But luckily for us, this program is compiled with panic = "abort" ;)
///
/// Invariant:
/// {status.shared_index, SpectrumWriter.write_index, SpectrumReader.read_index}
/// must always be a permutation of 0..3.
struct SpectrumCell {
    data: [UnsafeCell<SpectrumFrame>; 3],
    status: Atomic<SpectrumStatus>,
}

unsafe impl Sync for SpectrumCell {}

pub fn new_spectrum_cell(spectrum_size: usize) -> (SpectrumWriter, SpectrumReader) {
    let data = [
        UnsafeCell::new(SpectrumFrame::new(spectrum_size)),
        UnsafeCell::new(SpectrumFrame::new(spectrum_size)),
        UnsafeCell::new(SpectrumFrame::new(spectrum_size)),
    ];
    let status = Atomic::new(SpectrumStatus {
        shared_index: 0,
        available: false,
    });

    let writer = Arc::new(SpectrumCell { data, status });
    let reader = writer.clone();
    (
        SpectrumWriter {
            cell: writer,
            write_index: 1,
        },
        SpectrumReader {
            cell: reader,
            read_index: 2,
        },
    )
}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct SpectrumStatus {
    /// Demilitarized. Do not yield a &mut to SpectrumCell.data[SpectrumStatus.shared_index].
    shared_index: u8,

    /// If true, data[shared_index] produced by the writer and should be fetched by the reader.
    available: bool,
}

impl Atom for SpectrumStatus {
    type Repr = u16;

    fn pack(self) -> Self::Repr {
        // safe_transmute only supports refutable transmute of byte slices, not pass-by-value
        ((self.shared_index as u16) << 8) + (self.available as u16)
    }

    fn unpack(src: Self::Repr) -> Self {
        Self {
            shared_index: (src >> 8) as u8,
            available: (src & 1) != 0,
        }
    }
}

pub struct SpectrumWriter {
    cell: Arc<SpectrumCell>,
    write_index: usize,
}

impl SpectrumWriter {
    /// Obtain a mutable reference to the SpectrumFrame we own in the SpectrumCell.
    pub fn get_mut(&mut self) -> &mut SpectrumFrame {
        unsafe { &mut *self.cell.data[self.write_index].get() }
    }

    /// Publish the currently owned SpectrumCell so it can be fetched by
    /// the reader thread (SpectrumReader). Obtain a different one to mutate.
    pub fn publish(&mut self) {
        let publish = SpectrumStatus {
            shared_index: self.write_index as _,
            available: true,
        };

        // The write has Release ordering, so all our past writes to
        // `&mut data[write_index]` are ordered before the write.
        // The read has Acquire ordering, so all our future writes to
        // `&mut data[unpublished.shared_index]` are ordered after the read.
        let unpublished = self.cell.status.swap(publish, Ordering::AcqRel);

        // ignore unpublished.available, we don't care if we unpublished a new or used frame.
        self.write_index = unpublished.shared_index as _;
    }
}

pub struct SpectrumReader {
    cell: Arc<SpectrumCell>,
    read_index: usize,
}

impl SpectrumReader {
    /// Obtain a mutable reference to the SpectrumFrame we own in the SpectrumCell.
    #[allow(dead_code)]
    pub fn get_mut(&mut self) -> &mut SpectrumFrame {
        unsafe { &mut *self.cell.data[self.read_index].get() }
    }

    /// Obtain a shared reference to the SpectrumFrame we own in the SpectrumCell.
    /// Still takes &mut self because I don't know if it's safe to not do so.
    #[allow(dead_code)]
    pub fn get(&mut self) -> &SpectrumFrame {
        unsafe { &*self.cell.data[self.read_index].get() }
    }

    /// If the writer thread (SpectrumWriter) has published a new version
    /// since our previous fetch, obtain that one to read (and possibly mutate)
    /// and publish our old entry for the writer to overwrite.
    pub fn fetch(&mut self) {
        if !self.cell.status.load(Ordering::Relaxed).available {
            return;
        }

        // We know it's available. Even if SpectrumWriter overwrites it, it'll still be available.
        // So unconditionally swap.
        let dirty = SpectrumStatus {
            shared_index: self.read_index as _,
            available: false,
        };

        // The write has Release ordering, so all our past accesses to
        // `&(mut) data[read_index]` are ordered before the write.
        // The read has Acquire ordering, so all our future accesses to
        // `&(mut) data[published.shared_index]` are ordered after the read.
        let published = self.cell.status.swap(dirty, Ordering::AcqRel);
        assert!(published.available);
        self.read_index = published.shared_index as _;
    }
}

#[cfg(test)]
mod tests {
    use super::SpectrumStatus;

    /// Ensure that `impl Atom for SpectrumStatus` round-trips.
    #[test]
    fn atom_round_trip() {
        let data = [
            SpectrumStatus {
                shared_index: 42,
                available: true,
            },
            SpectrumStatus {
                shared_index: 1,
                available: false,
            },
        ];

        use atomig::Atom;

        for orig in &data {
            let packed = orig.clone().pack();
            let round_trip = SpectrumStatus::unpack(packed);
            assert_eq!(&round_trip, orig);
        }
    }
}
