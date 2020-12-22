//! Contains `FlipCell<T>`,
//! an atomic value written on one thread and read on another without tearing,
//! and accessed through `FlipWriter<T>` and `FlipReader<T>`.
//!
//! See `FlipCell` docs for details.
//!
//! Designed similarly to <https://github.com/Ralith/oddio/blob/55beef4/src/swap.rs>.
//!
//! TODO:
//! - Test for UB using miri and loom
//! - Add cache padding between entries in SpectrumCell
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// An atomic value written on one thread and read on another without tearing.
///
/// You do not interact directly with a `FlipCell<T>`,
/// but instead a `FlipWriter<T>` and `FlipReader<T>`,
/// each of which can be sent to a different thread.
///
/// The writer has access to a non-aliased `&mut T` instance,
/// and can "publish" the value at any time, releasing it and obtaining a different `&mut T`.
/// The reader has access to a `&T` (in theory, `&mut` as well) unaffected by the writer thread,
/// but can "fetch" the latest value published by the writer at any time.
///
/// # Implementation
///
/// `FlipCell` holds three `T` instances in an array, accessed via one writer and one reader struct.
/// The writer and reader each hold an "owning index" into the array,
/// which can be converted into a `&mut T` or `&T` (respectively) at any time.
/// This struct also holds an atomic value, composed of a third "shared index"
/// (owning an array entry which is not directly accessible),
/// and a bit-flag for whether the array entry was last owned by the writer or reader.
///
/// Internally, the writer or reader can atomically swap their "owning index" with the shared index,
/// using various memory orderings.
/// This transfers ownership of the T instances without moving them in memory.
///
/// # Public API
///
/// When the writer publishes a value written to its `&mut T`,
/// it unconditionally swaps its owning index into the shared index.
///
/// When the reader fetches the latest value,
/// it only swaps the shared index into its owning index
/// if the shared index's array entry was last owned by the writer.
/// Otherwise it does nothing (since the shared index points to a stale `T`
/// already seen earlier by the reader)
///
/// # Safety invariants
///
/// {shared_state & INDEX_MASK, FlipWriter.write_index, FlipReader.read_index}
/// must always be a permutation of 0..3.
pub struct FlipCell<T> {
    // TODO cache-align all of these variables
    data: [UnsafeCell<T>; 3],
    shared_state: SharedState,
}

/// Based of Mutex's impls. Hopefully sound.
unsafe impl<T> Sync for FlipCell<T> where T: Send {}
unsafe impl<T> Send for FlipCell<T> where T: Send {}

impl<T> FlipCell<T> {
    pub fn new3(shared_v: T, writer_v: T, reader_v: T) -> (FlipWriter<T>, FlipReader<T>) {
        let data = [
            UnsafeCell::new(shared_v),
            UnsafeCell::new(writer_v),
            UnsafeCell::new(reader_v),
        ];
        let shared_state = 0.into();

        let writer = Arc::new(FlipCell { data, shared_state });
        let reader = writer.clone();
        (
            FlipWriter {
                cell: writer,
                write_index: 1,
            },
            FlipReader {
                cell: reader,
                read_index: 2,
                is_initial: true,
            },
        )
    }

    #[allow(dead_code)]
    pub fn new_clone(value: T) -> (FlipWriter<T>, FlipReader<T>)
    where
        T: Clone,
    {
        Self::new3(value.clone(), value.clone(), value)
    }

    #[allow(dead_code)]
    pub fn new_default() -> (FlipWriter<T>, FlipReader<T>)
    where
        T: Default,
    {
        Self::new3(T::default(), T::default(), T::default())
    }
}

type SharedState = AtomicU8;
const INDEX_MASK: u8 = 0b011;
const FRESH_FLAG: u8 = 0b100;

/// Used to write and publish values into a `FlipCell`.
/// See `FlipCell` docs for details.
pub struct FlipWriter<T> {
    cell: Arc<FlipCell<T>>,
    write_index: u8,
}

impl<T> FlipWriter<T> {
    /// Obtain a mutable reference to the T we own in the FlipCell.
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.cell.data[self.write_index as usize].get() }
    }

    /// Publish the currently owned FlipCell so it can be fetched by
    /// the reader thread (FlipReader). Obtain a different one to mutate.
    pub fn publish(&mut self) {
        let publish_index = self.write_index | FRESH_FLAG;

        // The write has Release ordering, so all our past writes to
        // `data[write_index]` are ordered before the write.
        // I'm not sure if using Relaxed for the read is sound.
        // So use Acquire just to be safe.
        let depublished = self.cell.shared_state.swap(publish_index, Ordering::AcqRel);

        self.write_index = depublished & INDEX_MASK;
    }
}

/// Used to fetch the latest value from a `FlipCell`.
/// See `FlipCell` docs for details.
pub struct FlipReader<T> {
    cell: Arc<FlipCell<T>>,
    read_index: u8,

    /// True if fetch() has never been called.
    is_initial: bool,
}

impl<T> FlipReader<T> {
    /// Obtain a shared reference to the T we own in the FlipCell.
    pub fn get(&self) -> &T {
        unsafe { &*self.cell.data[self.read_index as usize].get() }
    }

    /// If the writer thread (FlipWriter) has published a new version
    /// since our previous fetch, obtain that one to read (and possibly mutate)
    /// and publish our old entry for the writer to overwrite.
    ///
    /// Return: Whether we updated our value.
    pub fn fetch(&mut self) -> bool {
        let is_initial = self.is_initial;
        self.is_initial = false;

        if self.cell.shared_state.load(Ordering::Relaxed) & FRESH_FLAG == 0 {
            // On the first call to fetch, always return true even if we don't fetch a new value,
            // since the reader thread has never processed the initial value.
            return is_initial;
        }

        // We know it's available. Even if FlipWriter overwrites it, it'll still be available.
        // So unconditionally swap.
        let stale_state = self.read_index;

        // I'm not sure if using Relaxed for the write is sound.
        // So use Release just to be safe.
        // The read has Acquire ordering, so all our future accesses to
        // `data[published_state & INDEX_MASK]` are ordered after the read.
        let published_state = self.cell.shared_state.swap(stale_state, Ordering::AcqRel);
        assert!(published_state & FRESH_FLAG == FRESH_FLAG);

        self.read_index = published_state & INDEX_MASK;
        true
    }
}
