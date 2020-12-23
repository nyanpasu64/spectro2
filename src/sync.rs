/// Wildly unsafe.
/// Designed similarly to https://github.com/Ralith/oddio/blob/55beef4/src/swap.rs.
///
/// TODO:
/// - Test for UB using miri and loom
/// - Add cache padding between entries in SpectrumCell?
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
/// In particular, a panic within `impl Atom for SpectrumStatus` or atomig would be bad.
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
            is_initial: true,
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
    ///
    /// Return: Whether the reader has seen our previous published value.
    pub fn publish(&mut self) -> bool {
        let publish = SpectrumStatus {
            shared_index: self.write_index as _,
            available: true,
        };

        // The write has Release ordering, so all our past writes to
        // `data[write_index]` are ordered before the write.
        // I'm not sure if using Relaxed for the read is sound.
        // So use Acquire just to be safe.
        let depublished = self.cell.status.swap(publish, Ordering::AcqRel);

        self.write_index = depublished.shared_index as _;
        let dirty = !depublished.available;
        dirty
    }
}

pub struct SpectrumReader {
    cell: Arc<SpectrumCell>,
    read_index: u8,

    /// True if fetch() has never been called.
    is_initial: bool,
}

impl SpectrumReader {
    /// Obtain a shared reference to the SpectrumFrame we own in the SpectrumCell.
    #[allow(dead_code)]
    pub fn get(&self) -> &SpectrumFrame {
        unsafe { &*self.cell.data[self.read_index as usize].get() }
    }

    /// If the writer thread (SpectrumWriter) has published a new version
    /// since our previous fetch, obtain that one to read (and possibly mutate)
    /// and publish our old entry for the writer to overwrite.
    ///
    /// Return: Whether we updated our value.
    pub fn fetch(&mut self) -> bool {
        let is_initial = self.is_initial;
        self.is_initial = false;

        if !self.cell.status.load(Ordering::Relaxed).available {
            // On the first call to fetch, always return true even if we don't fetch a new value,
            // since the reader thread has never processed the initial value.
            return is_initial;
        }

        // We know it's available. Even if SpectrumWriter overwrites it, it'll still be available.
        // So unconditionally swap.
        let dirty = SpectrumStatus {
            shared_index: self.read_index,
            available: false,
        };

        // I'm not sure if using Relaxed for the write is sound.
        // So use Release just to be safe.
        // The read has Acquire ordering, so all our future accesses to
        // `data[published.shared_index]` are ordered after the read.
        let published = self.cell.status.swap(dirty, Ordering::AcqRel);
        assert!(published.available);

        self.read_index = published.shared_index as _;
        true
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
