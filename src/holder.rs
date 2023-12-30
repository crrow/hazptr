use std::sync::atomic::{AtomicPtr, Ordering};

use crate::{
    asymmetric_light_barrier,
    domain::{Global, HazPtrDomain},
    HazPtr, HazPtrObject,
};

/// HazPtrHolder is used for readers.
///
/// Since we never deallocate the HazPtr, so it will be fine with static
/// lifetime. And every access is shared access.
///
/// HazPtrHolder should know where the HazPtrDomain come from.
pub struct HazPtrHolder<'domain, F> {
    hazard: &'domain HazPtr,
    domain: &'domain HazPtrDomain<F>,
}

impl Default for HazPtrHolder<'static, Global> {
    fn default() -> Self {
        Self {
            hazard: HazPtrDomain::shared().acquire(),
            domain: HazPtrDomain::shared(),
        }
    }
}

impl<'domain, F> HazPtrHolder<'domain, F> {
    pub fn for_domain(d: &'domain HazPtrDomain<F>) -> Self {
        Self {
            hazard: d.acquire(),
            domain: d,
        }
    }

    pub fn protect<T>(&mut self, src: &'_ AtomicPtr<T>) -> Option<&'domain T>
    where
        T: HazPtrObject<'domain, F>, // the HazPtrObj's lifetime as long as domain's
    {
        let mut ptr = src.load(Ordering::Relaxed);
        loop {
            match unsafe { self.try_protect(ptr, src) } {
                Ok(r) => break r,
                Err(ptr2) => ptr = ptr2,
            }
        }
    }

    /// #Safety:
    ///    
    /// Caller must guarantee that the address in AtomicPtr is valid as a
    /// reference or null.     If it was null, then the address will be
    /// turned into an option through the [`std::ptr::NonNull::new`]. Caller
    /// must also guarantee that the value behinde the AtomicPtr will only be
    /// deallocated through calls to [`HazPtrObject::retire`] on the same
    /// [`HazPtrDomain`] as this holder has..
    ///
    /// The return type of &T is fine, since the lifetime is the
    /// [`HazPtrHolder`] it self, and the [`HazPtrObject`] will respect us.
    pub unsafe fn try_protect<T>(
        &mut self,
        ptr: *mut T, // i have already read the variable once; so we don't have to read it again.
        src: &'_ AtomicPtr<T>,
    ) -> Result<Option<&'domain T>, *mut T>
    where
        T: HazPtrObject<'domain, F>, // the HazPtrObj's lifetime as long as domain's
    {
        self.hazard.protect(ptr as *mut u8);

        asymmetric_light_barrier();

        // we are trying to protect the pointer of type of T,
        // but the protect method should protect every thing, so we just cast here.
        let ptr2 = src.load(Ordering::Acquire);
        if ptr != ptr2 {
            self.hazard.reset();
            return Err(ptr2);
        }

        // all good, protected
        return Ok(std::ptr::NonNull::new(ptr).map(|nn| {
            // safety: this is safe:
            // 1. target of ptr1 will not be deallocated since our hazard pointer is active.
            // 2. point address is valid by the safty contract of load.

            let r = unsafe { nn.as_ref() };
            debug_assert_eq!(
                r.domain() as *const HazPtrDomain<F>,
                self.domain as *const HazPtrDomain<F>,
                "object guareded by different domain than holder used to access"
            );
            r
        }));
    }
    pub fn reset(&mut self) {
        // we can do this here since the reset require a mutable reference,
        // if there exists loaded value T, then we cannot call this method.
        self.hazard.reset();
    }
}

impl<F> Drop for HazPtrHolder<'_, F> {
    fn drop(&mut self) {
        // make sure if is currently guarding something, then stop guarding that thing.
        self.reset();
        self.domain.release(self.hazard);
    }
}
