use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub struct HazPtr {
    // ptr is an AtomicPtr to self, because it should be writable by the thread that owns the
    // hazptr, and it should be readable by any writer thread.
    //
    // The ptr is a pointee points to another fat point.
    pub(crate) ptr: AtomicPtr<u8>, // *mut u8
    pub(crate) next: AtomicPtr<HazPtr>,
    pub(crate) active: AtomicBool,
}

impl HazPtr {
    pub(crate) fn reset(&self) {
        self.ptr.store(std::ptr::null_mut(), Ordering::Release);
    }
    pub(crate) fn protect(&self, ptr: *mut u8) {
        // It should receive a shared reference, we should make sure it still valid.
        self.ptr.store(ptr, Ordering::Release);
    }
    pub(crate) fn release(&self) {
        // It should receive a shared reference, we should make sure it still valid.
        self.active.store(false, Ordering::Release);
    }
    pub(crate) fn try_acquire(&self) -> bool {
        let active = self.active.load(Ordering::Acquire);
        !active
            && self
                .active
                .compare_exchange(active, true, Ordering::Release, Ordering::Relaxed)
                .is_ok()
    }
}
