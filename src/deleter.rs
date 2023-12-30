pub trait Reclaim {}
impl<T> Reclaim for T {}

pub trait Deleter {
    unsafe fn delete(&self, ptr: *mut dyn Reclaim);
}

impl Deleter for unsafe fn(*mut dyn Reclaim) {
    unsafe fn delete(&self, ptr: *mut dyn Reclaim) {
        (*self)(ptr)
    }
}

pub mod deleters {
    use super::Reclaim;

    unsafe fn _drop_box(ptr: *mut dyn Reclaim) {
        // Safety: Safe by the safety gurantees of retire and because its only used when
        // retiring box objects.
        let _ = unsafe { Box::from_raw(ptr) };
    }
    /// # Safety
    ///
    /// Can only be used on values that were originally derived from a Box.
    pub const drop_box: unsafe fn(*mut dyn Reclaim) = _drop_box;

    unsafe fn _drop_in_place(ptr: *mut dyn Reclaim) {
        unsafe {
            std::ptr::drop_in_place(ptr);
        }
    }

    /// # Safety
    /// Always safe to use given requirements on HazPtrObj::retire,
    /// but may lead to memory leaks if the pointer type itself needs drop.
    pub const drop_in_place: unsafe fn(*mut dyn Reclaim) = _drop_in_place;
}
