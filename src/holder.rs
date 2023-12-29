use std::sync::atomic::{AtomicPtr, Ordering};

use crate::domain::HazPtrDomain;
use crate::{HazPtr, HazPtrObject};

/// HazPtrHolder is used for readers.
///
/// Since we never deallocate the HazPtr, so it will be fine with static lifetime.
/// And every access is shared access.
///
/// HazPtrHolder should know where the HazPtrDomain come from.
pub struct HazPtrHolder<'domain> {
    hazard: Option<&'domain HazPtr>,
    domain: &'domain HazPtrDomain,
}

impl Default for HazPtrHolder<'static> {
    fn default() -> Self {
        Self {
            hazard: None,
            domain: HazPtrDomain::shared(),
        }
    }
}

impl<'domain> HazPtrHolder<'domain> {
    pub fn for_domain(d: &'domain HazPtrDomain) -> Self {
        Self {
            hazard: None,
            domain: d,
        }
    }
    fn hazptr(&mut self) -> &'domain HazPtr {
        if let Some(ptr) = self.hazard {
            ptr
        } else {
            // if we don't have ptr yet then we acquire one from the global domain.
            let ptr = self.domain.acquire();
            self.hazard = Some(ptr);
            ptr
        }
    }
    /// #Safety:
    ///    
    /// Caller must guarantee that the address in AtomicPtr is valid as a reference or null.
    ///     If it was null, then the address will be turned into an option through the [`std::ptr::NonNull::new`].
    /// Caller must also guarantee that the value behinde the AtomicPtr will only be deallocated
    /// through calls to [`HazPtrObject::retire`] on the same [`HazPtrDomain`] as this holder has..
    ///
    /// The return type of &T is fine, since the lifetime is the [`HazPtrHolder`] it self, and the [`HazPtrObject`]
    /// will respect us.
    pub unsafe fn load<T>(&mut self, ptr: &'_ AtomicPtr<T>) -> Option<&'domain T>
    where
        T: HazPtrObject<'domain>, // the HazPtrObj's lifetime as long as domain's
    {
        let hazptr = self.hazptr();
        let mut ptr1 = ptr.load(Ordering::SeqCst);
        loop {
            // we are trying to protect the pointer of type of T,
            // but the protect method should protect every thing, so we just cast here.
            hazptr.protect(ptr1 as *mut u8);
            let ptr2 = ptr.load(Ordering::SeqCst);
            if ptr2 == ptr1 {
                // all good, protected
                break std::ptr::NonNull::new(ptr1).map(|nn| {
                    // safety: this is safe:
                    // 1. target of ptr1 will not be deallocated since our hazard pointer is active.
                    // 2. point address is valid by the safty contract of load.

                    let r = unsafe { nn.as_ref() };
                    debug_assert_eq!(
                        r.domain() as *const HazPtrDomain,
                        self.domain as *const HazPtrDomain,
                        "object guareded by different domain than holder used to access"
                    );
                    r
                });
            } else {
                ptr1 = ptr2;
            }
        }
    }
    pub fn reset(&mut self) {
        // we can do this here since the reset require a mutable reference,
        // if there exists loaded value T, then we cannot call this method.
        if let Some(hp) = self.hazard {
            hp.ptr.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
    }
}

impl Drop for HazPtrHolder<'_> {
    fn drop(&mut self) {
        // make sure if is currently guarding something, then stop guarding that thing.
        self.reset();

        if let Some(hp) = self.hazard {
            // inactive it, then other thread can reuse this thing.
            hp.active.store(false, Ordering::SeqCst);
        }
    }
}
