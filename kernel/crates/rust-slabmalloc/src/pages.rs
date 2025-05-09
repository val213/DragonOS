use crate::*;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
/// A trait defining bitfield operations we need for tracking allocated objects within a page.
pub(crate) trait Bitfield {
    fn initialize(&mut self, for_size: usize, capacity: usize);
    fn first_fit(
        &self,
        base_addr: usize,
        layout: Layout,
        page_size: usize,
    ) -> Option<(usize, usize)>;
    fn is_allocated(&self, idx: usize) -> bool;
    fn set_bit(&self, idx: usize);
    fn clear_bit(&self, idx: usize);
    fn is_full(&self) -> bool;
    fn all_free(&self, relevant_bits: usize) -> bool;
}

/// Implementation of bit operations on u64 slices.
///
/// We allow deallocations (i.e. clearning a bit in the field)
/// from any thread. That's why the bitfield is a bunch of AtomicU64.
impl Bitfield for [AtomicU64] {
    /// Initialize the bitfield
    ///
    /// # Arguments
    ///  * `for_size`: Object size we want to allocate
    ///  * `capacity`: Maximum size of the buffer the bitmap maintains.
    ///
    /// Ensures that we only have free slots for what we can allocate
    /// within the page (by marking everything else allocated).
    fn initialize(&mut self, for_size: usize, capacity: usize) {
        // Set everything to allocated
        for bitmap in self.iter_mut() {
            *bitmap = AtomicU64::new(u64::MAX);
        }

        // Mark actual slots as free
        let relevant_bits = core::cmp::min(capacity / for_size, self.len() * 64);
        for idx in 0..relevant_bits {
            self.clear_bit(idx);
        }
    }

    /// Tries to find a free block of memory that satisfies `alignment` requirement.
    ///
    /// # Notes
    /// * We pass size here to be able to calculate the resulting address within `data`.
    #[inline(always)]
    fn first_fit(
        &self,
        base_addr: usize,
        layout: Layout,
        page_size: usize,
    ) -> Option<(usize, usize)> {
        for (base_idx, b) in self.iter().enumerate() {
            let bitval = b.load(Ordering::Relaxed);
            if bitval == u64::MAX {
                continue;
            } else {
                let negated = !bitval;
                let first_free = negated.trailing_zeros() as usize;
                let idx: usize = base_idx * 64 + first_free;
                let offset = idx * layout.size();

                // TODO(bad): psize needs to be passed as arg
                let offset_inside_data_area =
                    offset <= (page_size - OBJECT_PAGE_METADATA_OVERHEAD - layout.size());
                if !offset_inside_data_area {
                    return None;
                }

                let addr: usize = base_addr + offset;
                let alignment_ok = addr % layout.align() == 0;
                let block_is_free = bitval & (1 << first_free) == 0;
                if alignment_ok && block_is_free {
                    return Some((idx, addr));
                }
            }
        }
        None
    }

    /// Check if the bit `idx` is set.
    #[inline(always)]
    fn is_allocated(&self, idx: usize) -> bool {
        let base_idx = idx / 64;
        let bit_idx = idx % 64;
        (self[base_idx].load(Ordering::Relaxed) & (1 << bit_idx)) > 0
    }

    /// Sets the bit number `idx` in the bit-field.
    #[inline(always)]
    fn set_bit(&self, idx: usize) {
        let base_idx = idx / 64;
        let bit_idx = idx % 64;
        self[base_idx].fetch_or(1 << bit_idx, Ordering::Relaxed);
    }

    /// Clears bit number `idx` in the bit-field.
    #[inline(always)]
    fn clear_bit(&self, idx: usize) {
        let base_idx = idx / 64;
        let bit_idx = idx % 64;
        self[base_idx].fetch_and(!(1 << bit_idx), Ordering::Relaxed);
    }

    /// Checks if we could allocate more objects of a given `alloc_size` within the
    /// `capacity` of the memory allocator.
    ///
    /// # Note
    /// The ObjectPage will make sure to mark the top-most bits as allocated
    /// for large sizes (i.e., a size 512 SCAllocator will only really need 3 bits)
    /// to track allocated objects). That's why this function can be simpler
    /// than it would need to be in practice.
    #[inline(always)]
    fn is_full(&self) -> bool {
        self.iter()
            .filter(|&x| x.load(Ordering::Relaxed) != u64::MAX)
            .count()
            == 0
    }

    /// Checks if the page has currently no allocations.
    ///
    /// This is called `all_free` rather than `is_emtpy` because
    /// we already have an is_empty fn as part of the slice.
    #[inline(always)]
    fn all_free(&self, relevant_bits: usize) -> bool {
        for (idx, bitmap) in self.iter().enumerate() {
            let checking_bit_range = (idx * 64, (idx + 1) * 64);
            if relevant_bits >= checking_bit_range.0 && relevant_bits < checking_bit_range.1 {
                // Last relevant bitmap, here we only have to check that a subset of bitmap is marked free
                // the rest will be marked full
                let bits_that_should_be_free = relevant_bits - checking_bit_range.0;
                let free_mask = (1 << bits_that_should_be_free) - 1;
                return (free_mask & bitmap.load(Ordering::Relaxed)) == 0;
            }

            if bitmap.load(Ordering::Relaxed) == 0 {
                continue;
            } else {
                return false;
            }
        }

        true
    }
}

/// This trait is used to define a page from which objects are allocated
/// in an `SCAllocator`.
///
/// The implementor of this trait needs to provide access to the page meta-data,
/// which consists of:
/// - A bitfield (to track allocations),
/// - `prev` and `next` pointers to insert the page in free lists
pub trait AllocablePage {
    /// The total size (in bytes) of the page.
    ///
    /// # Note
    /// We also assume that the address of the page will be aligned to `SIZE`.
    const SIZE: usize;

    fn bitfield(&self) -> &[AtomicU64; 8];
    fn bitfield_mut(&mut self) -> &mut [AtomicU64; 8];
    fn prev(&mut self) -> &mut Rawlink<Self>
    where
        Self: core::marker::Sized;
    fn next(&mut self) -> &mut Rawlink<Self>
    where
        Self: core::marker::Sized;

    /// Tries to find a free block within `data` that satisfies `alignment` requirement.
    fn first_fit(&self, layout: Layout) -> Option<(usize, usize)> {
        let base_addr = (self as *const Self as *const u8) as usize;
        self.bitfield().first_fit(base_addr, layout, Self::SIZE)
    }

    /// Tries to allocate an object within this page.
    ///
    /// In case the slab is full, returns a null ptr.
    fn allocate(&mut self, layout: Layout) -> *mut u8 {
        match self.first_fit(layout) {
            Some((idx, addr)) => {
                self.bitfield().set_bit(idx);
                addr as *mut u8
            }
            None => ptr::null_mut(),
        }
    }

    /// Checks if we can still allocate more objects of a given layout within the page.
    fn is_full(&self) -> bool {
        self.bitfield().is_full()
    }

    /// Checks if the page has currently no allocations.
    fn is_empty(&self, relevant_bits: usize) -> bool {
        self.bitfield().all_free(relevant_bits)
    }

    /// Deallocates a memory object within this page.
    fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) -> Result<(), AllocationError> {
        trace!(
            "AllocablePage deallocating ptr = {:p} with {:?}",
            ptr,
            layout
        );
        let page_offset = (ptr.as_ptr() as usize) & (Self::SIZE - 1);
        assert!(page_offset % layout.size() == 0);
        let idx = page_offset / layout.size();
        assert!(
            self.bitfield().is_allocated(idx),
            "{:p} not marked allocated?",
            ptr
        );

        self.bitfield().clear_bit(idx);
        Ok(())
    }

    /// 统计page中还可以分配多少个object
    fn free_obj_count(&self) -> usize {
        // 统计page中还可以分配多少个object
        let mut free_obj_count = 0;

        // 遍历page中的bitfield(用来统计内存分配情况的u64数组)
        for b in self.bitfield().iter() {
            let bitval = b.load(Ordering::Relaxed);
            free_obj_count += bitval.count_zeros() as usize;
        }

        free_obj_count
    }
}

/// Holds allocated data within a 4 KiB page.
///
/// Has a data-section where objects are allocated from
/// and a small amount of meta-data in form of a bitmap
/// to track allocations at the end of the page.
///
/// # Notes
/// An object of this type will be exactly 4 KiB.
/// It is marked `repr(C)` because we rely on a well defined order of struct
/// members (e.g., dealloc does a cast to find the bitfield).
#[repr(C)]
#[repr(align(4096))]
pub struct ObjectPage<'a> {
    /// Holds memory objects.
    #[allow(dead_code)]
    data: [u8; OBJECT_PAGE_SIZE - OBJECT_PAGE_METADATA_OVERHEAD],

    /// Next element in list (used by `PageList`).
    next: Rawlink<ObjectPage<'a>>,
    /// Previous element in  list (used by `PageList`)
    prev: Rawlink<ObjectPage<'a>>,

    /// A bit-field to track free/allocated memory within `data`.
    pub(crate) bitfield: [AtomicU64; 8],
}
impl<'a> ObjectPage<'a> {
    pub fn new() -> Box<ObjectPage<'a>> {
        unsafe { Box::new_uninit().assume_init() }
    }
}

// These needs some more work to be really safe...
unsafe impl Send for ObjectPage<'_> {}
unsafe impl Sync for ObjectPage<'_> {}

impl AllocablePage for ObjectPage<'_> {
    const SIZE: usize = OBJECT_PAGE_SIZE;

    fn bitfield(&self) -> &[AtomicU64; 8] {
        &self.bitfield
    }
    fn bitfield_mut(&mut self) -> &mut [AtomicU64; 8] {
        &mut self.bitfield
    }

    fn prev(&mut self) -> &mut Rawlink<Self> {
        &mut self.prev
    }

    fn next(&mut self) -> &mut Rawlink<Self> {
        &mut self.next
    }
}

impl<'a> Default for ObjectPage<'a> {
    fn default() -> ObjectPage<'a> {
        unsafe { mem::MaybeUninit::zeroed().assume_init() }
    }
}

impl fmt::Debug for ObjectPage<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ObjectPage")
    }
}

/// A list of pages.
pub(crate) struct PageList<'a, T: AllocablePage> {
    /// Points to the head of the list.
    pub(crate) head: Option<&'a mut T>,
    /// Number of elements in the list.
    pub(crate) elements: usize,
}

impl<'a, T: AllocablePage> PageList<'a, T> {
    #[cfg(feature = "unstable")]
    pub(crate) const fn new() -> PageList<'a, T> {
        PageList {
            head: None,
            elements: 0,
        }
    }

    #[cfg(not(feature = "unstable"))]
    pub(crate) fn new() -> PageList<'a, T> {
        PageList {
            head: None,
            elements: 0,
        }
    }

    pub(crate) fn iter_mut<'b: 'a>(&mut self) -> ObjectPageIterMut<'b, T> {
        let m = match self.head {
            None => Rawlink::none(),
            Some(ref mut m) => Rawlink::some(*m),
        };

        ObjectPageIterMut {
            head: m,
            phantom: core::marker::PhantomData,
        }
    }

    /// Inserts `new_head` at the front of the list.
    pub(crate) fn insert_front<'b>(&'b mut self, mut new_head: &'a mut T) {
        match self.head {
            None => {
                *new_head.prev() = Rawlink::none();
                self.head = Some(new_head);
            }
            Some(ref mut head) => {
                *new_head.prev() = Rawlink::none();
                *head.prev() = Rawlink::some(new_head);
                mem::swap(head, &mut new_head);
                *head.next() = Rawlink::some(new_head);
            }
        }

        self.elements += 1;
    }

    /// Removes `slab_page` from the list.
    pub(crate) fn remove_from_list(&mut self, slab_page: &mut T) {
        unsafe {
            match slab_page.prev().resolve_mut() {
                None => {
                    self.head = slab_page.next().resolve_mut();
                }
                Some(prev) => {
                    *prev.next() = match slab_page.next().resolve_mut() {
                        None => Rawlink::none(),
                        Some(next) => Rawlink::some(next),
                    };
                }
            }

            match slab_page.next().resolve_mut() {
                None => (),
                Some(next) => {
                    *next.prev() = match slab_page.prev().resolve_mut() {
                        None => Rawlink::none(),
                        Some(prev) => Rawlink::some(prev),
                    };
                }
            }
        }

        *slab_page.prev() = Rawlink::none();
        *slab_page.next() = Rawlink::none();
        self.elements -= 1;
    }

    /// Removes `slab_page` from the list.
    pub(crate) fn pop<'b>(&'b mut self) -> Option<&'a mut T> {
        match self.head {
            None => None,
            Some(ref mut head) => {
                let head_next = head.next();
                let mut new_head = unsafe { head_next.resolve_mut() };
                mem::swap(&mut self.head, &mut new_head);
                let _ = self.head.as_mut().map(|n| {
                    *n.prev() = Rawlink::none();
                });

                self.elements -= 1;

                #[allow(clippy::manual_inspect)]
                new_head.map(|node| {
                    *node.prev() = Rawlink::none();
                    *node.next() = Rawlink::none();
                    node
                })
            }
        }
    }

    /// Does the list contain `s`?
    pub(crate) fn contains(&mut self, s: *const T) -> bool {
        for slab_page in self.iter_mut() {
            if core::ptr::eq(slab_page, s) {
                return true;
            }
        }

        false
    }
}

/// Iterate over all the pages inside a slab allocator
pub(crate) struct ObjectPageIterMut<'a, P: AllocablePage> {
    head: Rawlink<P>,
    phantom: core::marker::PhantomData<&'a P>,
}

impl<'a, P: AllocablePage + 'a> Iterator for ObjectPageIterMut<'a, P> {
    type Item = &'a mut P;

    #[inline]
    fn next(&mut self) -> Option<&'a mut P> {
        unsafe {
            #[allow(clippy::manual_inspect)]
            self.head.resolve_mut().map(|next| {
                self.head = match next.next().resolve_mut() {
                    None => Rawlink::none(),
                    Some(ref mut sp) => Rawlink::some(*sp),
                };
                next
            })
        }
    }
}

/// Rawlink is a type like Option<T> but for holding a raw pointer.
///
/// We use it to link AllocablePages together. You probably won't need
/// to use this type if you're not implementing AllocablePage
/// for a custom page-size.
pub struct Rawlink<T> {
    p: *mut T,
}

impl<T> Default for Rawlink<T> {
    fn default() -> Self {
        Rawlink { p: ptr::null_mut() }
    }
}

impl<T> Rawlink<T> {
    /// Like Option::None for Rawlink
    pub(crate) fn none() -> Rawlink<T> {
        Rawlink { p: ptr::null_mut() }
    }

    /// Like Option::Some for Rawlink
    pub(crate) fn some(n: &mut T) -> Rawlink<T> {
        Rawlink { p: n }
    }

    /// Convert the `Rawlink` into an Option value
    ///
    /// **unsafe** because:
    ///
    /// - Dereference of raw pointer.
    /// - Returns reference of arbitrary lifetime.
    #[allow(dead_code)]
    pub(crate) unsafe fn resolve<'a>(&self) -> Option<&'a T> {
        self.p.as_ref()
    }

    /// Convert the `Rawlink` into an Option value
    ///
    /// **unsafe** because:
    ///
    /// - Dereference of raw pointer.
    /// - Returns reference of arbitrary lifetime.
    pub(crate) unsafe fn resolve_mut<'a>(&mut self) -> Option<&'a mut T> {
        self.p.as_mut()
    }

    /// Return the `Rawlink` and replace with `Rawlink::none()`
    #[allow(dead_code)]
    pub(crate) fn take(&mut self) -> Rawlink<T> {
        mem::replace(self, Rawlink::none())
    }
}
