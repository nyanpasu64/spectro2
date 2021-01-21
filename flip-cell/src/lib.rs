//! Contains `FlipCell<T>`,
//! an atomic value written on one thread and read on another without tearing,
//! and accessed through `FlipWriter<T>` and `FlipReader<T>`.
//!
//! See `FlipCell` docs for details.
//!
//! Designed similarly to <https://github.com/Ralith/oddio/blob/55beef4/src/swap.rs>.
//!
//! TODO:
//! - Add cache padding between entries in SpectrumCell

mod dep {
    #[cfg(feature = "loom")]
    use loom as lib;
    #[cfg(not(feature = "loom"))]
    use std as lib;

    pub use lib::cell::UnsafeCell;
    pub use lib::sync::atomic::{AtomicU8, Ordering};
    pub use lib::sync::Arc;
}
use dep::*;

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
/// using the AcqRel memory ordering.
/// (Weaker orderings are not sound; see https://github.com/HadrienG2/triple-buffer/issues/14.)
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
///
/// # Send/Sync
///
/// `FlipCell<T>` is primarily intended for `T: Send`,
/// because it transfers `&mut` access to `T` between threads.
/// If `T: !Send` (like `MutexGuard`), it's practically useless unless `T: Sync`
/// and has interior mutability (like `MutexGuard<Atomic>`).
/// Which is basically useless as a contrived type.
///
/// If T is Send+Sync, FlipCell<T> is Send (or optionally Sync, it doesn't matter),
/// and FlipReader<T> is Send+Sync.
///
/// If T is Send, FlipCell<T> is Send and FlipReader<T> is Send.
///
/// If T is Sync, FlipCell<T> is optionally Sync, and FlipReader<T> is Sync.
///
/// If T is neither Send nor Sync, neither FlipCell nor FlipReader is Send/Sync.
pub struct FlipCell<T> {
    // TODO cache-align all of these variables
    data: [UnsafeCell<T>; 3],
    shared_state: SharedState,
}

// UnsafeCell<T> is Send if T is Send, so we don't need an unsafe impl.

/// There is no reason to share `&FlipCell` across multiple threads.
/// `FlipCell` instances cannot be publicly obtained.
/// Even if it was, the only methods it would expose (to convert into (ArcWriter, ArcReader)
/// or borrow as (RefWriter, RefReader)) would require `self` or `&mut self` respectively,
/// and it would have no `&self` methods.
///
/// Implementing Sync is harmless and makes the type (dubiously) more general.
unsafe impl<T> Sync for FlipCell<T> where T: Sync {}

// FlipReader/Writer contain Arc<FlipCell<T>>,
// and missing either Send or Sync for FlipCell<T> makes Arc<FlipCell<T>> !Send and !Sync.
// But FlipReader/Writer can legally be Send/Sync because they don't allow cloning the Arc,
// and don't provide aliased access to the T they point to.
// We will unsafely implement them by hand.

impl<T> FlipCell<T> {
    pub fn new3(shared_v: T, writer_v: T, reader_v: T) -> (FlipWriter<T>, FlipReader<T>) {
        let data = [
            UnsafeCell::new(shared_v),
            UnsafeCell::new(writer_v),
            UnsafeCell::new(reader_v),
        ];
        let shared_state = SharedState::new(0);

        let writer = Arc::new(FlipCell { data, shared_state });
        let reader = Arc::clone(&writer);
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

    pub fn new_clone(value: T) -> (FlipWriter<T>, FlipReader<T>)
    where
        T: Clone,
    {
        Self::new3(value.clone(), value.clone(), value)
    }

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

/// &mut FlipWriter<T> acts like &mut T, including the ability to swap it.
unsafe impl<T> Send for FlipWriter<T> where T: Send {}
/// &FlipWriter<T> provides no access whatsoever, so implementing Sync is harmless.
unsafe impl<T> Sync for FlipWriter<T> where T: Sync {}

impl<T> FlipWriter<T> {
    /// Obtain a mutable reference to the T we own in the FlipCell.
    #[cfg(not(feature = "loom"))]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.cell.data[self.write_index as usize].get() }
    }

    #[cfg(feature = "loom")]
    pub fn with_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        unsafe { self.cell.data[self.write_index as usize].with_mut(|p| f(&mut *p)) }
    }

    /// Publish the currently owned FlipCell so it can be fetched by
    /// the reader thread (FlipReader). Obtain a different one to mutate.
    pub fn publish(&mut self) {
        let publish_index = self.write_index | FRESH_FLAG;

        // The write has Release ordering, so all our past writes to
        // `data[write_index]` are ordered before the write.
        // The read has Acquire ordering, so all our future writes to
        // `data[depublished]` are ordered after the read.
        //
        // (Using Relaxed for the read is not sound; see https://github.com/HadrienG2/triple-buffer/issues/14.)
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

/// &mut FlipReader<T> acts like &mut T, but only the ability to swap it.
unsafe impl<T> Send for FlipReader<T> where T: Send {}
/// &FlipWriter<T> acts like &T.
unsafe impl<T> Sync for FlipReader<T> where T: Sync {}

impl<T> FlipReader<T> {
    /// Obtain a shared reference to the T we own in the FlipCell.
    #[cfg(not(feature = "loom"))]
    pub fn get(&self) -> &T {
        unsafe { &*self.cell.data[self.read_index as usize].get() }
    }

    #[cfg(feature = "loom")]
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        unsafe { self.cell.data[self.read_index as usize].with(|p| f(&*p)) }
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

        // The write has Release ordering, so all our past reads to
        // `data[read_index]` are ordered before the write.
        // The read has Acquire ordering, so all our future reads to
        // `data[published_state & INDEX_MASK]` are ordered after the read.
        //
        // (Using Relaxed for the write is not sound; see https://github.com/HadrienG2/triple-buffer/issues/14.)
        let published_state = self.cell.shared_state.swap(stale_state, Ordering::AcqRel);
        assert!(published_state & FRESH_FLAG == FRESH_FLAG);

        self.read_index = published_state & INDEX_MASK;
        true
    }
}

#[cfg(test)]
#[cfg(feature = "loom")]
mod tests {
    use super::FlipCell;
    use loom::thread;

    /// Use Loom to test all reorderings of a reader and writer thread
    /// interacting with FlipCell, and check for possible data races.
    ///
    /// This test fails with an UnsafeCell concurrent access (data race),
    /// unless I mark both shared_state.swap() as AcqRel, not Acquire or Release.
    /// I don't know if it's a false positive or not.
    /// Nonetheless I'll leave both in place to be safe.
    #[test]
    fn loom_flip_cell() {
        loom::model(|| {
            let initial = 0i32;
            let write_begin = 1i32;
            let write_end = 4i32;

            let (mut writer, mut reader) = FlipCell::new_clone(initial);

            let write_thread = thread::spawn(move || {
                for x in write_begin..write_end {
                    writer.with_mut(|p| *p = x);
                    writer.publish();
                }
            });

            let mut last_seen = -1i32;
            for _ in 0..8 {
                let is_fresh = reader.fetch();
                let x = reader.with(|&x| x);

                assert!((initial..write_end).contains(&x));
                assert!(x >= last_seen);
                assert!((x > last_seen) == is_fresh);

                last_seen = x;
            }

            write_thread.join().unwrap();
        });
    }
}

#[cfg(test)]
#[cfg(not(feature = "loom"))]
mod tests {
    use crate::FlipCell;

    #[allow(dead_code)]
    fn ensure_sync<T: Sync>(_: &T) {}

    #[allow(dead_code)]
    fn ensure_send<T: Send>(_: &mut T) {}

    /// Ensure that we can wrap a !Send type in a FlipCell,
    /// as long as we don't move a reader/writer across threads.
    #[test]
    fn not_send() {
        use std::marker::PhantomData;
        use std::sync::MutexGuard;

        #[derive(Clone)]
        struct NotSend(i32, PhantomData<MutexGuard<'static, i32>>);

        let (mut writer, reader) = FlipCell::new_clone(NotSend(0, PhantomData));

        writer.get_mut().0 = 2;
        ensure_sync(&reader);
    }

    /// Ensure that we can wrap a !Sync type in a FlipCell,
    /// as long as we don't share a reader/writer across threads.
    #[test]
    fn not_sync() {
        use std::cell::RefCell;
        use std::thread;

        let not_sync = RefCell::new(0i32);
        let (mut writer, mut reader) = FlipCell::new_clone(not_sync);

        let writer_th = thread::spawn(move || {
            writer.get_mut();
            writer.publish();
        });
        let reader_th = thread::spawn(move || {
            reader.fetch();
            reader.get();
        });

        writer_th.join().unwrap();
        reader_th.join().unwrap();
    }

    /// Can we obtain &T on multiple threads, pointing to a non-Sync type?
    /// If so, it can lead to memory unsafety.
    ///
    /// Currently, this code (which contains data races) is rejected properly.
    /// To verify, uncomment this test and ensure it fails to build.
    #[test]
    fn miri_reader_sync() {
        // use std::cell::Cell;
        // use std::sync::Arc;
        // use std::thread;

        // let not_sync = Cell::new(0);
        // let (_, reader) = FlipCell::new_clone(not_sync);
        // let reader = Arc::new(reader);

        // let mut threads = vec![];
        // for i in 0..3 {
        //     let reader = Arc::clone(&reader);
        //     threads.push(thread::spawn(move || {
        //         reader.get().replace(i);
        //     }));
        // }

        // for thread in threads {
        //     thread.join().unwrap();
        // }
    }

    /// What is the lifetime of a FlipCell constructed from a non-'static value?
    ///
    /// Currently the lifetime is properly bounded.
    /// To verify, uncomment this test and ensure it fails to build.
    #[test]
    fn miri_lifetime() {
        // use std::sync::Arc;

        // let (mut writer, mut reader) = {
        //     let mut non_static = 0;
        //     let non_static = Arc::new(&mut non_static);
        //     let (writer, reader) = FlipCell::new_clone(non_static);
        //     (writer, reader)
        // };
    }
}
